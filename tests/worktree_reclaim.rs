//! Tests for `dot-agent-deck worktree list|reclaim` — reclaiming merged
//! worktrees behind a three-part gate (PR state, cleanliness, ownership).
//!
//! Fast tier on purpose: these drive the REAL binary as a subprocess against
//! real git repositories in tempdirs, with a synthetic `gh` on `PATH`. No PTY,
//! no daemon, no LLM, no `e2e` feature gate — the CLI-subprocess shape mirrors
//! other real-binary-subprocess integration tests in this suite, and
//! `tests/e2e_issue_dispatch.rs` is the precedent for the stub-`gh` seam.
//!
//! The two scenarios that carry the whole argument are `002` (a
//! squash-merged branch, whose commits are NOT in `main`'s ancestry — the case
//! `git branch --merged` misses) and `004` (a branch that IS an ancestor of
//! `main` but has no PR at all — the case `git branch --merged` would wrongly
//! delete).

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use dot_agent_deck::agent_pty::ORCHESTRATION_UNKNOWN_SENTINEL;
use spec::spec;

// Issue #322. Fast-tier, and deliberately does NOT link `tests/common/mod.rs`;
// the ~40-line crate-internal resolver is `#[path]`-included instead, at two
// extra test executions rather than the harness's ~530. Every fixture here is a
// real git repository plus its worktrees — structurally unbounded, and the
// largest shape in the fast tier — so it is exactly what must not land on the
// RAM-backed `/tmp`. See `docs/develop/e2e-temp-dirs.md`.
#[path = "../src/test_temp.rs"]
mod test_temp;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Synthetic `gh` reading canned PR JSON from `$GHSTUB_DIR`, keyed by the
/// `--head <branch>` value with `/` → `_`. Models three pieces of real `gh
/// pr list` semantics precisely, because a stub that accepts any flags
/// validates only that `gh` was invoked, never that it was invoked
/// correctly — a lenient stub would let a call missing `--repo` or
/// `--state` pass silently:
///
/// - **`--repo` is required.** These fixture repos carry an `origin` remote
///   (see `Fixture::new`) precisely so a fixed implementation has something
///   non-empty to derive `--repo` from, but the stub does not care what the
///   value IS — only that it is present. Real `gh`, run in a repo with no
///   configured remotes at all, fails with its own "no git remotes found"
///   rather than silently guessing; an absent `--repo` here fails the same
///   way, with a message that says exactly what was missing.
/// - **`--state` gates a `MERGED` fixture.** Real `gh pr list` defaults to
///   `--state open`, so a fixture whose canned state is `MERGED` must NOT be
///   returned unless the caller passed `--state all` (or `--state merged`).
///   With no `--state`, or `--state open`, the stub prints `[]`, exactly as
///   real `gh` does — this is a legitimate, non-error empty result, not a
///   wrong-invocation failure.
/// - **Unknown flags are rejected, not swallowed.** Real `gh` errors on a
///   flag it does not recognize; the previous `*) ;;` catch-all silently
///   accepted anything, which is how a caller could omit `--state` and
///   `--repo` entirely and still get back fixture data.
///
/// A missing fixture file (no PR for this branch) still prints `[]`, same as
/// real `gh pr list` — that is a correctly-resolved "no PR" answer, not a
/// wrong-invocation error, and must not look like one.
const GH_STUB_SCRIPT: &str = r#"#!/bin/sh
all_args="$*"
group="$1"
sub="$2"
shift 2 2>/dev/null || true

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
            echo "gh stub: unrecognized flag \"$1\" (real gh errors on an unknown flag; a permissive catch-all is how a wrong invocation went undetected) -- full invocation: gh $all_args" 1>&2
            exit 1
            ;;
    esac
    shift
done

if [ "$group" = "pr" ] && [ "$sub" = "list" ]; then
    if [ -z "$repo" ]; then
        echo "gh stub: --repo is required -- this fixture repo has no git remote for gh to infer one from, matching real gh's own failure in a repo with no remotes configured; full invocation: gh $all_args" 1>&2
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
        Self::build(None)
    }

    /// Like [`Fixture::new`], but the main repo's own git directory lives in a
    /// SEPARATE store via `git init --separate-git-dir=<store>`, so every
    /// linked worktree's admin dir resolves under `<store>/worktrees/<name>`
    /// rather than `<repo>/.git/worktrees/<name>`. A legitimate, git-native
    /// way to relocate metadata -- issue #144 finding 2's proposed
    /// containment fix (require the resolved git-dir to sit under the
    /// enumerating repo's own `<common-dir>/worktrees/`) must still accept
    /// this layout, since `<common-dir>` here is genuinely the store, not
    /// `<repo>/.git`.
    fn new_with_separate_git_dir() -> Self {
        let scratch = test_temp::tempdir().expect("scratch tempdir");
        let store = scratch.path().join("gitdir-store");
        Self::build_in(scratch, Some(&store))
    }

    fn build(separate_git_dir: Option<&Path>) -> Self {
        let scratch = test_temp::tempdir().expect("scratch tempdir");
        Self::build_in(scratch, separate_git_dir)
    }

    fn build_in(scratch: tempfile::TempDir, separate_git_dir: Option<&Path>) -> Self {
        let repo = scratch.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        match separate_git_dir {
            Some(store) => git(
                &repo,
                &[
                    "init",
                    "--initial-branch=main",
                    "--quiet",
                    &format!("--separate-git-dir={}", store.display()),
                ],
            ),
            None => git(&repo, &["init", "--initial-branch=main", "--quiet"]),
        }
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "seed\n").expect("write seed file");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "--quiet", "-m", "seed"]);
        // A real implementation needs a non-empty value to pass as `gh`'s
        // `--repo`; without a configured remote there is nothing to derive
        // one from, which would make defect 2's RED unfixable in this
        // harness rather than a defect to fix. The stub only checks that
        // `--repo` is non-empty, not its exact value, so any origin works.
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/test-org/test-repo.git",
            ],
        );

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

    /// Canned `gh pr list --head <branch>` reply. `headRepositoryOwner` is set
    /// to `"test-org"`, matching `Fixture::new`'s own `origin` remote
    /// (`test-org/test-repo`) -- so every existing caller of this helper keeps
    /// describing a legitimate same-repo merged PR once `resolve_pr_state`
    /// starts requiring the head repository owner to match (issue #144
    /// finding 2). Callers exercising a DIFFERENT owner (a genuinely
    /// different fork, or a worktree-scoped remote pointing elsewhere) use
    /// [`Fixture::set_pr_state_with_head_owner`] instead.
    fn set_pr_state(&self, branch: &str, state: &str) {
        self.set_pr_state_with_head_owner(branch, state, "test-org");
    }

    /// Same as [`Fixture::set_pr_state`], but with an explicit
    /// `headRepositoryOwner.login` value -- for a PR opened from a fork whose
    /// owner differs from this fixture's own `origin` remote.
    fn set_pr_state_with_head_owner(&self, branch: &str, state: &str, head_owner: &str) {
        let key = branch.replace('/', "_");
        let body = format!(
            r#"[{{"state":"{state}","headRefName":"{branch}","headRepositoryOwner":{{"login":"{head_owner}"}}}}]"#
        );
        std::fs::write(self.ghstub.join(format!("pr-{key}.json")), body).expect("write pr fixture");
    }

    /// Same as [`Fixture::set_pr_state`], but the canned reply carries NO
    /// `headRepositoryOwner` key at all -- the shape real `gh` can return
    /// when the head repository can no longer be resolved (e.g. a fork
    /// deleted after its PR merged). Used to pin the fail-closed path: an
    /// unverifiable owner must never be treated as a match.
    fn set_pr_state_no_owner_field(&self, branch: &str, state: &str) {
        let key = branch.replace('/', "_");
        let body = format!(r#"[{{"state":"{state}","headRefName":"{branch}"}}]"#);
        std::fs::write(self.ghstub.join(format!("pr-{key}.json")), body).expect("write pr fixture");
    }

    /// Give a linked worktree its OWN `origin` remote via
    /// `extensions.worktreeConfig`, resolvable through `git remote get-url
    /// origin` run inside that worktree.
    ///
    /// `remote.<name>.url` is a LIST-accumulating config variable, not a
    /// plain scalar (verified directly against git 2.55.0): when the common
    /// `$GIT_DIR/config` already defines `remote.origin.url`, a later
    /// `config.worktree` entry for the same key (read after common config,
    /// per `git-config(1)`'s FILES section) is appended as an ADDITIONAL
    /// (push-only) value, never a replacement — `git remote get-url origin`
    /// always returns the FIRST-defined value, so it keeps reporting the
    /// common config's URL regardless. The per-worktree override this
    /// function performs therefore only actually takes effect — is visible
    /// to `git remote get-url` — when the common config has NO `origin` at
    /// all; the caller MUST remove the shared `origin` first (`git remote
    /// remove origin` on `self.repo`) for this to produce a genuinely
    /// different resolvable remote for `worktree`.
    fn set_worktree_origin(&self, worktree: &Path, url: &str) {
        git(&self.repo, &["config", "extensions.worktreeConfig", "true"]);
        git(
            worktree,
            &["config", "--worktree", "remote.origin.url", url],
        );
    }

    /// Mark a worktree as deck-created. The marker lives in the worktree's OWN
    /// git metadata dir (`<repo>/.git/worktrees/<name>/`), which is outside the
    /// working tree — so it can never make the tree dirty, and it is removed
    /// along with the worktree by `git worktree remove`.
    fn mark_owned(&self, worktree: &Path) {
        let git_dir = worktree_git_dir(worktree);
        std::fs::write(git_dir.join("dot-agent-deck-owner"), "deck\n").expect("write owner marker");
    }

    /// Like [`Self::add_worktree_with_commit`], but for a worktree DIRECTORY
    /// NAME that may contain non-UTF-8 bytes. `name` is joined and passed to
    /// `git worktree add` as a raw `OsStr` via `Command::arg`, never through
    /// a `&str`/`to_string_lossy` conversion — a lossy round-trip here would
    /// corrupt the very bytes `worktree_reclaim_008` exists to exercise
    /// before git ever saw them.
    #[cfg(target_os = "linux")]
    fn add_worktree_with_commit_raw(&self, name: &std::ffi::OsStr, branch: &str) -> PathBuf {
        let path = self._scratch.path().join(name);
        let add = Command::new("git")
            .current_dir(&self.repo)
            .arg("worktree")
            .arg("add")
            .arg("-b")
            .arg(branch)
            .arg(&path)
            .arg("main")
            .output()
            .unwrap_or_else(|e| panic!("git worktree add (raw path) failed to spawn: {e}"));
        assert!(
            add.status.success(),
            "git worktree add (raw path) failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        std::fs::write(path.join("work.txt"), "work\n").expect("write work file");
        git(&path, &["add", "work.txt"]);
        git(&path, &["commit", "--quiet", "-m", "work"]);
        path
    }

    /// Like [`Self::mark_owned`], but resolves `git rev-parse --git-dir`'s
    /// stdout as raw bytes instead of `String::from_utf8_lossy`.
    /// `mark_owned`'s lossy conversion is harmless for every other fixture
    /// (ASCII names round-trip losslessly), but for a worktree whose name —
    /// and therefore whose `.git/worktrees/<name>` git-dir — contains a
    /// non-UTF-8 byte, it corrupts the resolved git-dir path into one that
    /// does not exist on disk, so the owner-marker write fails before the
    /// fixture is even set up. That would be a bug in this HARNESS, not the
    /// production code `worktree_reclaim_008` targets, hence the separate
    /// raw-byte-safe path.
    #[cfg(target_os = "linux")]
    fn mark_owned_raw(&self, worktree: &Path) {
        use std::os::unix::ffi::OsStrExt;

        let out = Command::new("git")
            .current_dir(worktree)
            .args(["rev-parse", "--git-dir"])
            .output()
            .expect("git rev-parse --git-dir");
        let mut bytes = out.stdout;
        while matches!(bytes.last(), Some(b'\n') | Some(b'\r')) {
            bytes.pop();
        }
        let git_dir = PathBuf::from(std::ffi::OsStr::from_bytes(&bytes));
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            worktree.join(git_dir)
        };
        std::fs::write(git_dir.join("dot-agent-deck-owner"), "deck\n").expect("write owner marker");
    }

    /// Like [`Fixture::mark_owned`], but writes the full fork #166 marker
    /// format -- `"deck\ncreated-by: <creator>\n"`, matching
    /// `src/worktree_reclaim.rs`'s `mark_worktree_owned` byte-for-byte --
    /// carrying an explicit `created-by:` identity rather than the bare
    /// legacy content. Written directly to the worktree's admin dir, not
    /// through any `dot-agent-deck` process: this stands in for a marker a
    /// PRIOR, now-exited orchestration process wrote, which is exactly the
    /// shape `--mine` must match against with no shared in-memory state.
    fn mark_owned_with_creator(&self, worktree: &Path, creator: &str) {
        let git_dir = worktree_git_dir(worktree);
        std::fs::write(
            git_dir.join("dot-agent-deck-owner"),
            format!("deck\ncreated-by: {creator}\n"),
        )
        .expect("write owner marker with creator");
    }

    /// The scratch tempdir's own root -- for fixtures that need to place a
    /// file OUTSIDE the repo/worktree tree, such as the forged admin dir
    /// below.
    fn scratch_root(&self) -> &Path {
        self._scratch.path()
    }

    /// Commit a `.gitignore` naming `pattern` to `main`. Any worktree created
    /// AFTER this call inherits it, since a new branch starts from `main`'s
    /// tip. Content later written under `pattern` is untracked-and-ignored,
    /// so `git status --porcelain` (which never reports ignored files) stays
    /// blind to it -- the exact gate issue #144 finding 1 measured as blind.
    fn write_gitignore(&self, pattern: &str) {
        std::fs::write(self.repo.join(".gitignore"), pattern).expect("write .gitignore");
        git(&self.repo, &["add", ".gitignore"]);
        git(&self.repo, &["commit", "--quiet", "-m", "add gitignore"]);
    }

    /// Forge `Ownership::Ours` for `worktree` WITHOUT calling
    /// [`Fixture::mark_owned`] -- auditor finding 2's working reproduction for
    /// issue #144. Copies the worktree's REAL admin dir to a location outside
    /// the repo (`evil`), drops the ownership marker into the copy, keeps
    /// `commondir` honest (pointing at the repo's real `.git`, so the
    /// containment fix under discussion has something legitimate to compare
    /// against), fixes the `gitdir` back-pointer to point at the worktree's
    /// own `.git` file, and finally overwrites that ONE regular file --
    /// `<worktree>/.git` -- to redirect there. `git status --porcelain` never
    /// reports `.git` itself, so the cleanliness gate stays blind to this.
    /// Requires only write access to a file inside the worktree's own
    /// directory, never to the repo's real `.git`.
    fn forge_ownership_via_git_dir_redirect(&self, worktree: &Path) {
        let real_admin_dir = worktree_git_dir(worktree);
        let evil = self.scratch_root().join(format!(
            "evil-{}",
            worktree.file_name().unwrap().to_string_lossy()
        ));
        copy_dir_all(&real_admin_dir, &evil);
        std::fs::write(evil.join("dot-agent-deck-owner"), "deck\n").expect("write forged marker");
        std::fs::write(
            evil.join("commondir"),
            format!("{}\n", self.repo.join(".git").display()),
        )
        .expect("write commondir");
        let worktree_git_file = worktree.join(".git");
        std::fs::write(
            evil.join("gitdir"),
            format!("{}\n", worktree_git_file.display()),
        )
        .expect("write gitdir back-pointer");
        std::fs::write(&worktree_git_file, format!("gitdir: {}\n", evil.display()))
            .expect("forge the worktree's own .git redirect");
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

    /// Like [`Fixture::run`], but explicitly controls
    /// `DOT_AGENT_DECK_WORKTREE_OWNER` on the spawned subprocess's
    /// environment rather than leaving it to whatever this test process
    /// happens to have -- `Some(v)` sets it to exactly `v`; `None` removes
    /// it, guaranteeing absence regardless of the ambient environment this
    /// test binary runs under. Each call spawns a genuinely fresh OS
    /// process (`Command::new` below), sharing no memory, no daemon, and no
    /// cache with this test process or with any earlier call -- the shape
    /// `--mine`'s restart-correctness claim depends on.
    fn run_with_owner(&self, args: &[&str], owner: Option<&str>) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"));
        cmd.current_dir(&self.repo)
            .args(args)
            .env("PATH", path)
            .env("GHSTUB_DIR", &self.ghstub);
        match owner {
            Some(v) => {
                cmd.env("DOT_AGENT_DECK_WORKTREE_OWNER", v);
            }
            None => {
                cmd.env_remove("DOT_AGENT_DECK_WORKTREE_OWNER");
            }
        }
        cmd.output().expect("run dot-agent-deck")
    }
}

/// Resolve a worktree's OWN git metadata dir via `git rev-parse --git-dir`,
/// mirroring `src/worktree_reclaim.rs`'s own `resolve_git_dir` -- used both by
/// [`Fixture::mark_owned`] (to write a legitimate marker) and by
/// [`Fixture::forge_ownership_via_git_dir_redirect`] (to locate the REAL admin
/// dir before it gets copied and the worktree's `.git` file is overwritten).
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

/// Recursively copy a directory tree -- `std` has no built-in for this.
fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create copy destination dir");
    for entry in std::fs::read_dir(src).expect("read source dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        let ty = entry.file_type().expect("read file type");
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
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
    let line = text
        .lines()
        .find(|l| l.contains("wt-merged"))
        .unwrap_or_else(|| {
            panic!("`worktree list` must name the worktree it examined; got:\n{text}")
        });
    assert!(
        line.contains("remove"),
        "a MERGED, clean, deck-owned worktree must be reported with a \"remove\" verdict, not \
         merely be named in the output -- got line:\n{line}\nfull output:\n{text}"
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
    // Deliberately NOT named with "dirty"/"uncommitted"/"untracked": the gh
    // stub's specific-failure messages echo back the full invocation,
    // including `--head <branch>`, so a branch or
    // worktree name containing one of those words would make this test pass
    // on ANY unresolvable-PR-state failure, not on the dirty check it names.
    let wt = fx.add_worktree_with_commit("wt-scratch-file", "feat/scratch-file");
    fx.set_pr_state("feat/scratch-file", "MERGED");
    fx.mark_owned(&wt);
    std::fs::write(wt.join("scratch-notes.txt"), "never committed\n").expect("write untracked");

    let out = fx.run(&["worktree", "reclaim", "--yes"]);
    // `wt.exists()` below would hold trivially even if `worktree reclaim` were
    // not a real subcommand at all — clap rejects it before touching the
    // filesystem, so the worktree is untouched either way. Rule that out
    // explicitly first, ruling out clap's own usage/parse-error exit code so
    // the RED signal is unambiguous rather than an accidental pass.
    assert_ne!(
        out.status.code(),
        Some(2),
        "exit code 2 is clap's own generic usage/parse-error code; an implemented `worktree \
         reclaim` correctly keeping this dirty worktree must use a code that does not collide \
         with it, or the absence of `wt-scratch-file`'s removal below is no evidence at all; \
         status={:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !combined(&out).contains("Usage:"),
        "stderr still carries clap's own subcommand-usage banner, meaning `worktree reclaim` \
         was not recognized as a real subcommand rather than being handled and correctly \
         deciding to keep this worktree; out={}",
        combined(&out)
    );
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
    // Without this, `wt.exists()` below is not evidence: it holds trivially
    // right now because `worktree reclaim` is not a real subcommand yet, so
    // clap rejects it and the filesystem is never touched — the exact same
    // "pass by doing nothing" trap `worktree_reclaim_003` guards against.
    // Rule out clap's own usage/parse-error exit code first, so the
    // assertion cannot pass merely because the subcommand was rejected.
    assert_ne!(
        out.status.code(),
        Some(2),
        "exit code 2 is clap's own generic usage/parse-error code; an implemented `worktree \
         reclaim` correctly keeping this ancestor-but-no-PR worktree must use a code that does \
         not collide with it, or the absence of `wt-scratch`'s removal below is no evidence at \
         all; status={:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !combined(&out).contains("Usage:"),
        "stderr still carries clap's own subcommand-usage banner, meaning `worktree reclaim` \
         was not recognized as a real subcommand rather than being handled and correctly \
         deciding to keep this worktree; out={}",
        combined(&out)
    );
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
    // As in `003`/`004`: `wt.exists()` below holds trivially today regardless
    // of whether the ask surface exists, since clap rejects the unrecognized
    // subcommand before touching the filesystem. Rule that out first so the
    // signal is pinned to the assertions that actually carry it.
    assert_ne!(
        out.status.code(),
        Some(2),
        "exit code 2 is clap's own generic usage/parse-error code; an implemented `worktree \
         reclaim` correctly asking about this foreign worktree must use a code that does not \
         collide with it; status={:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !combined(&out).contains("Usage:"),
        "stderr still carries clap's own subcommand-usage banner, meaning `worktree reclaim` \
         was not recognized as a real subcommand rather than being handled and raising a \
         pending decision; out={}",
        combined(&out)
    );
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
    let entries = doc
        .get("worktrees")
        .and_then(|w| w.as_array())
        .unwrap_or_else(|| {
            panic!("the JSON document must carry a `worktrees` array; got:\n{stdout}")
        });
    let entry = entries
        .iter()
        .find(|e| {
            e.get("path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| p.contains("wt-json"))
        })
        .unwrap_or_else(|| {
            panic!("the JSON document must include the examined worktree; got:\n{stdout}")
        });
    assert_eq!(
        entry.get("verdict").and_then(|v| v.as_str()),
        Some("remove"),
        "a MERGED, clean, deck-owned worktree's JSON entry must carry verdict \"remove\", not \
         merely be present in the document; got entry:\n{entry}\nfull document:\n{stdout}"
    );
}

/// Scenario: A linked worktree carries its OWN `origin` remote — via
/// `extensions.worktreeConfig`, after the main checkout's own `origin` is
/// removed entirely (see `set_worktree_origin`'s doc comment for why the
/// common config must have none for this to work at all) — naming a repo
/// whose PR for this exact branch IS merged. The main checkout itself now has
/// NO origin, so resolving PR state against ITS path — the pre-fix
/// `resolve_pr_state(repo_dir, ...)` argument — can never derive a `--repo`
/// and always fails closed to `keep`, regardless of the worktree's actual PR.
/// `worktree list` must resolve PR state against the worktree's OWN path
/// instead, finding the MERGED PR and reporting `remove`.
#[spec("worktree/reclaim/007")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_007_pr_state_resolved_against_worktree_own_remote_not_caller_cwd() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-own-remote", "feat/own-remote");
    fx.mark_owned(&wt); // clean by construction, owned: only PR state decides the verdict

    // The main checkout has NO origin at all: resolving PR state against ITS
    // path (the pre-fix `resolve_pr_state(repo_dir, ...)` argument) can never
    // derive a `--repo`, so it would always come back Unresolvable/keep, no
    // matter what this worktree's own PR actually is.
    git(&fx.repo, &["remote", "remove", "origin"]);
    fx.set_worktree_origin(&wt, "https://github.com/other-org/other-repo.git");
    // headRepositoryOwner must match the WORKTREE's own derived slug
    // ("other-org"), not this fixture's default ("test-org") -- the whole
    // point of this test is that PR state is resolved against the
    // worktree's own remote.
    fx.set_pr_state_with_head_owner("feat/own-remote", "MERGED", "other-org");

    let out = fx.run(&["worktree", "list"]);
    assert!(
        out.status.success(),
        "`worktree list` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    let text = combined(&out);
    let line = text
        .lines()
        .find(|l| l.contains("wt-own-remote"))
        .unwrap_or_else(|| {
            panic!("`worktree list` must name the worktree it examined; got:\n{text}")
        });
    let fields: Vec<&str> = line.split('\t').collect();
    assert_eq!(
        fields.len(),
        8,
        "unexpected `worktree list` row shape (fork #166 M2.3 added an OWNER column between \
         OWNED and VERDICT); got fields {fields:?} from line:\n{line}"
    );
    assert_eq!(
        fields[2], "merged",
        "PR state must be resolved against the worktree's OWN remote (which has a MERGED PR for \
         this branch) -- the main checkout has no `origin` at all, so resolving against ITS path \
         (the pre-fix behaviour) could never derive a `--repo` and would report `unresolvable`, \
         never `merged`; got PR column {:?} from line:\n{line}\nfull output:\n{text}",
        fields[2]
    );
    assert_eq!(
        fields[6], "remove",
        "a MERGED, clean, deck-owned worktree resolved against its OWN remote must be `remove` \
         -- resolving against the caller's cwd instead (no origin there at all) would fail \
         closed to `keep`, permanently refusing to reclaim a worktree that IS actually merged; \
         got verdict column {:?} from line:\n{line}\nfull output:\n{text}",
        fields[6]
    );
    assert_eq!(
        fields[7], "-",
        "a `remove` verdict carries no reason; anything else here would mean this row does not \
         actually carry the `remove` verdict the column above claims; got reason column {:?} \
         from line:\n{line}\nfull output:\n{text}",
        fields[7]
    );
}

/// Scenario: A local, UNMERGED worktree on branch `fix/typo` shares its name
/// with a DIFFERENT fork's already-merged PR whose head branch is also called
/// `fix/typo`. `resolve_pr_state` matches on `headRefName` alone, which is not
/// namespaced by head repository owner, so the other fork's MERGED state would
/// otherwise be attributed to this local branch. The worktree is deck-owned
/// and clean, so only the PR-state gate can save it: `worktree reclaim --yes`
/// must not remove it. Also pins reviewer finding NEW-2 (issue #144 follow-up):
/// this exact shape -- a genuine `headRefName` match rejected only on owner --
/// is the triangular (push-to-fork) workflow's every-branch case, so the
/// reported reason must name the real cause (an unresolvable head-repository
/// owner) rather than the misleading "no pull request found for this branch",
/// which would send a user hunting a PR that in fact exists.
#[spec("worktree/reclaim/009")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_009_cross_fork_branch_name_collision_is_not_treated_as_merged() {
    let fx = Fixture::new();
    // A NEW branch carrying its own commit: genuinely unmerged locally.
    let wt = fx.add_worktree_with_commit("wt-collision", "fix/typo");
    fx.mark_owned(&wt); // owned and clean: only PR state can still save it
    // A merged PR from a DIFFERENT fork ("other-fork", not this fixture's own
    // "test-org") whose head branch happens to be named identically.
    fx.set_pr_state_with_head_owner("fix/typo", "MERGED", "other-fork");

    let out = fx.run(&["worktree", "reclaim", "--yes"]);
    assert!(
        out.status.success(),
        "`worktree reclaim --yes` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        wt.exists(),
        "a same-named branch from a DIFFERENT fork's merged PR must not be attributed to this \
         local branch -- headRefName alone is not namespaced by head repository owner -- \
         {} is gone\n{}",
        wt.display(),
        combined(&out)
    );
    let text = combined(&out).to_lowercase();
    assert!(
        !text.contains("no pull request found for this branch"),
        "a branch whose headRefName genuinely matched a PR (from another fork) must not be \
         reported as though no PR exists at all -- that sends the user hunting a PR that is \
         really there (reviewer finding NEW-2); got:\n{}",
        combined(&out)
    );
    assert!(
        text.contains("owner"),
        "the reported reason must name the real cause -- the head repository owner could not be \
         confirmed -- not merely omit the misleading \"no pull request\" claim; got:\n{}",
        combined(&out)
    );
}

/// Scenario: Mirrors `009`, but the canned `gh` reply carries no
/// `headRepositoryOwner` field at all -- the shape real `gh` can return when
/// the head repository can no longer be resolved (e.g. a fork deleted after
/// its PR merged). The gate must fail closed to NOT-merged when it cannot
/// verify the head repository owner, exactly as every other unresolvable path
/// in this gate keeps rather than guesses.
#[spec("worktree/reclaim/010")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_010_missing_head_repository_owner_fails_closed_not_merged() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-no-owner-field", "fix/deleted-fork");
    fx.mark_owned(&wt); // owned and clean: only PR state can still save it
    // No `headRepositoryOwner` field in the canned reply at all.
    fx.set_pr_state_no_owner_field("fix/deleted-fork", "MERGED");

    let out = fx.run(&["worktree", "reclaim", "--yes"]);
    assert!(
        out.status.success(),
        "`worktree reclaim --yes` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        wt.exists(),
        "a PR reply missing `headRepositoryOwner` must fail closed -- an unverifiable owner \
         must never be treated as a match -- {} is gone\n{}",
        wt.display(),
        combined(&out)
    );
}

/// Scenario: A FOREIGN worktree (no ownership marker) that is merged and
/// clean is first left alone by a bare `reclaim` -- named in the pending list
/// -- and then IS removed once the user runs `reclaim --yes`. This pins the
/// batch-confirmation contract `--yes` actually has (issue #144 follow-up):
/// it removes an `Ask`-verdict worktree whose path was already shown to the
/// user, not only `Remove`-verdict (deck-owned) ones. An earlier version of
/// this suite asserted the opposite -- that a hand-made foreign worktree must
/// SURVIVE `--yes` -- which was withdrawn as a design mistake once this
/// module's own doc comment and `format_reclaim_human`'s "Run ... --yes to
/// remove them" message made clear that withholding removal here would leave
/// the `"ask" if yes` branch in `run_reclaim` unreachable dead code.
#[spec("worktree/reclaim/011")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_011_yes_removes_a_foreign_merged_clean_worktree_named_in_pending_first() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-foreign-yes", "feat/foreign-yes");
    fx.set_pr_state("feat/foreign-yes", "MERGED");
    // Deliberately NOT marked owned: `--yes` must still remove it, once its
    // path has been shown to the user via a bare `reclaim` first.

    let bare = fx.run(&["worktree", "reclaim"]);
    assert!(
        bare.status.success(),
        "a bare `worktree reclaim` must succeed; got {:?} out={}",
        bare.status,
        combined(&bare)
    );
    let bare_text = combined(&bare);
    assert!(
        bare_text.contains("wt-foreign-yes"),
        "the bare run must name this worktree's exact path in the pending list before --yes \
         is ever invoked; got:\n{bare_text}"
    );
    assert!(
        wt.exists(),
        "the bare run must not remove a foreign worktree on its own -- {} is gone\n{bare_text}",
        wt.display()
    );

    let yes = fx.run(&["worktree", "reclaim", "--yes"]);
    assert!(
        yes.status.success(),
        "`worktree reclaim --yes` must succeed; got {:?} out={}",
        yes.status,
        combined(&yes)
    );
    assert!(
        !wt.exists(),
        "`--yes` must remove a merged, clean, foreign worktree whose path was already shown to \
         the user by the prior bare run -- {} still exists\n{}",
        wt.display(),
        combined(&yes)
    );
}

/// Scenario: A deck-owned, MERGED, otherwise-clean worktree also holds a
/// directory matched by a committed `.gitignore` -- exactly the shape of a
/// real, worked-in deck worktree's `target/`. `git status --porcelain` never
/// reports ignored content, so the pre-fix gate treats it as clean and a bare
/// `reclaim` removes it (and the ignored content with it) with no path ever
/// shown and no `--yes` required. Ignored content must instead demote the
/// verdict from `Remove` to `Ask`, so the exact path is shown and removal
/// requires `--yes` (auditor finding F1, P1).
#[spec("worktree/reclaim/012")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_012_gitignored_content_demotes_bare_reclaim_to_ask() {
    let fx = Fixture::new();
    fx.write_gitignore("ignored/\n");
    let wt = fx.add_worktree_with_commit("wt-ignored", "feat/ignored");
    fx.set_pr_state("feat/ignored", "MERGED");
    fx.mark_owned(&wt);
    std::fs::create_dir_all(wt.join("ignored")).expect("create ignored dir");
    std::fs::write(wt.join("ignored").join("artifact.bin"), b"built stuff")
        .expect("write ignored content");

    // Fixture precondition: the ignored content must be genuinely invisible
    // to `git status --porcelain`, or this test is not exercising the gate
    // gap F1 exists for.
    let status = Command::new("git")
        .current_dir(&wt)
        .args(["status", "--porcelain"])
        .output()
        .expect("git status --porcelain");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "fixture precondition: `git status --porcelain` must report nothing for a worktree \
         holding only ignored content, or this test proves nothing about F1"
    );

    let out = fx.run(&["worktree", "reclaim"]);
    assert!(
        out.status.success(),
        "`worktree reclaim` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        wt.exists(),
        "a deck-owned, merged, clean-per-`git status` worktree that still holds gitignored \
         content must NOT be removed by a bare reclaim -- ignored content was never part of the \
         merged PR, and a bare reclaim prints nothing before acting -- {} is gone\n{}",
        wt.display(),
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        text.contains("wt-ignored"),
        "ignored content must demote the verdict to `Ask`, so the exact path is shown pending \
         `--yes` -- not merely survive some other way; got:\n{text}"
    );
}

/// Scenario: Pairs with `012` to prove the ignored-content demotion actually
/// discriminates: the SAME deck-owned/merged/clean shape, but with no ignored
/// content at all, must still be removed by a bare `reclaim`, unprompted.
/// Without this, a coder could satisfy `012` by demoting every worktree to
/// `Ask` regardless of content, which would make the ownership gate `012`
/// exists alongside pointless again. Expected GREEN from the start -- nothing
/// about the current (pre-F1-fix) code path treats this shape differently.
#[spec("worktree/reclaim/013")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_013_no_ignored_content_still_removed_bare_regression_guard() {
    let fx = Fixture::new();
    fx.write_gitignore("ignored/\n");
    let wt = fx.add_worktree_with_commit("wt-clean-no-ignored", "feat/clean-no-ignored");
    fx.set_pr_state("feat/clean-no-ignored", "MERGED");
    fx.mark_owned(&wt);
    // Deliberately no ignored content created.

    let out = fx.run(&["worktree", "reclaim"]);
    assert!(
        out.status.success(),
        "`worktree reclaim` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !wt.exists(),
        "a deck-owned, merged, clean worktree with NO ignored content must still be removed by a \
         bare reclaim, unprompted -- a fix that demotes every worktree to `Ask` regardless of \
         content would make the ownership gate pointless -- {} still exists\n{}",
        wt.display(),
        combined(&out)
    );
}

/// Scenario: One regular file inside a worktree's own directory -- its
/// `.git` file -- is overwritten to redirect to a copied admin dir that
/// carries a forged ownership marker, with `commondir` kept honest (pointing
/// at the real repo's `.git`) and the `gitdir` back-pointer fixed to match.
/// `git status --porcelain` never reports `.git` itself, so the cleanliness
/// gate stays blind to the tamper. The forged worktree must not resolve
/// `Ownership::Ours` -- it must land on `Ask`, not be removed unprompted by a
/// bare `reclaim` (auditor finding F2, P2; the #152-lineage class of trusting
/// *a* git-dir rather than *this worktree's own* git-dir).
#[spec("worktree/reclaim/014")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_014_forged_git_dir_redirect_is_not_treated_as_ours() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-forged", "feat/forged");
    fx.set_pr_state("feat/forged", "MERGED");
    // Deliberately NOT marked via `fx.mark_owned`: ownership is forged below
    // instead, by redirecting the worktree's own `.git` file.
    fx.forge_ownership_via_git_dir_redirect(&wt);

    // Fixture precondition: the redirect must actually fool
    // `git status --porcelain` into reporting nothing, or this test is not
    // exercising the forgery F2 exists for.
    let status = Command::new("git")
        .current_dir(&wt)
        .args(["status", "--porcelain"])
        .output()
        .expect("git status --porcelain");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "fixture precondition: the cleanliness gate must remain blind to the .git redirect, or \
         this test is not exercising F2's forgery"
    );

    let out = fx.run(&["worktree", "reclaim"]);
    assert!(
        out.status.success(),
        "`worktree reclaim` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        wt.exists(),
        "a forged `.git` redirect pointing at a copied admin dir carrying the ownership marker \
         must not resolve to `Ownership::Ours` -- the resolved git-dir must be required to sit \
         under the enumerating repo's own <common-dir>/worktrees/ -- {} is gone (removed by a \
         BARE reclaim, no path ever shown, no --yes)\n{}",
        wt.display(),
        combined(&out)
    );
    let text = combined(&out);
    // Deliberately more specific than "the worktree survived": survival alone
    // does not prove `Ownership::Foreign` was resolved -- it would ALSO hold
    // if ownership resolved `Ours` (verdict `Remove`) but the physical `git
    // worktree remove` step itself failed for some unrelated reason, landing
    // the report in `Kept` with a "removal failed: ..." reason instead of the
    // pending-ask list. Requiring the pending-list header (mirroring `005`'s
    // and `011`'s bare-run assertions) pins the SPECIFIC verdict this finding
    // is about, not merely "nothing bad happened this time".
    assert!(
        text.contains("reclaimable pending confirmation"),
        "the forged worktree must land specifically in the pending ASK list -- \
         `Ownership::Foreign`, not merely survive via some other keep/failure path; got:\n{text}"
    );
    assert!(
        text.contains("wt-forged"),
        "the pending ask list must name the forged worktree's exact path; got:\n{text}"
    );
}

/// Scenario: Pairs with `014` to prove the (not-yet-implemented) git-dir
/// containment fix discriminates rather than rejecting every worktree whose
/// admin dir lives outside `<repo>/.git/worktrees/`. The MAIN repo here uses
/// `git init --separate-git-dir=<store>`, a legitimate git-native metadata
/// relocation -- so linked worktrees resolve under `<store>/worktrees/<name>`
/// instead. A deck-owned, merged, clean worktree in this layout must still be
/// removed by a bare `reclaim`. Expected GREEN from the start -- nothing in
/// the current code treats a `--separate-git-dir` main repo differently.
#[spec("worktree/reclaim/015")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_015_separate_git_dir_layout_still_reclaimed_regression_guard() {
    let fx = Fixture::new_with_separate_git_dir();
    let wt = fx.add_worktree_with_commit("wt-separate", "feat/separate");
    fx.set_pr_state("feat/separate", "MERGED");
    fx.mark_owned(&wt);

    let out = fx.run(&["worktree", "reclaim"]);
    assert!(
        out.status.success(),
        "`worktree reclaim` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !wt.exists(),
        "a legitimately owned, merged, clean worktree in a `--separate-git-dir` main-repo layout \
         must still be removed by a bare reclaim -- a containment fix for F2 must discriminate, \
         not reject every worktree whose common dir lives outside `<repo>/.git` -- {} still \
         exists\n{}",
        wt.display(),
        combined(&out)
    );
}

/// Scenario: The canned `gh` reply's `headRepositoryOwner.login` differs from
/// this fixture's own `origin` owner ONLY in case (`"Test-Org"` vs.
/// `"test-org"`) -- exactly how a GitHub remote configured with capitalized
/// case resolves: `gh` follows the case-insensitive API and returns the
/// canonical login, but the byte-exact local comparison discards the match.
/// A deck-owned, otherwise-reclaimable worktree must still resolve `Merged`
/// and be removed by a bare `reclaim` (auditor finding F3 / reviewer NEW-1,
/// P2 -- GitHub logins are ASCII and case-insensitive).
#[spec("worktree/reclaim/016")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_016_case_variant_head_owner_still_resolves_merged() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-case-owner", "feat/case-owner");
    fx.mark_owned(&wt);
    // `Fixture::new`'s own `origin` owner is "test-org"; the canned reply
    // carries the case-variant "Test-Org" -- the shape GitHub itself would
    // return for a remote a user typed with capitals.
    fx.set_pr_state_with_head_owner("feat/case-owner", "MERGED", "Test-Org");

    let out = fx.run(&["worktree", "reclaim"]);
    assert!(
        out.status.success(),
        "`worktree reclaim` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !wt.exists(),
        "an owner that differs only in case from the local origin's owner must still resolve to \
         MERGED -- GitHub logins are case-insensitive -- {} still exists (a byte-exact \
         comparison silently discarded the only matching PR)\n{}",
        wt.display(),
        combined(&out)
    );
}

/// Scenario: A deck-owned worktree whose PR is MERGED and whose tree is clean
/// is fully reclaimable by every measure except one: its directory name
/// contains a byte that is not valid UTF-8. `examine_worktrees` lossy-converts
/// the parsed `PathBuf` into a `String` before `run_reclaim` hands it to `git
/// worktree remove`, so git is asked to remove a path that does not exist and
/// the worktree survives `reclaim --yes` untouched. Asserts the directory is
/// gone afterward, and that the report actually says so.
#[spec("worktree/reclaim/023")]
#[test]
#[cfg(target_os = "linux")]
fn worktree_reclaim_023_non_utf8_path_is_reclaimed() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fx = Fixture::new();
    let name = OsStr::from_bytes(b"wt-\xff-nonutf8");
    let branch = "feat/nonutf8";
    let wt = fx.add_worktree_with_commit_raw(name, branch);
    fx.set_pr_state(branch, "MERGED");
    fx.mark_owned_raw(&wt);

    // Fixture precondition: the directory must exist on disk with the EXACT
    // bytes intended, not a filesystem-normalised or -rejected stand-in.
    // Without this, a filesystem that silently rejected or altered the name
    // would make every assertion below pass for the wrong reason -- the
    // "pass by doing nothing" trap `worktree_reclaim_003`/`004`/`005` guard
    // against, one layer earlier: at fixture creation rather than at the
    // subcommand dispatch.
    let entries: Vec<_> = std::fs::read_dir(fx._scratch.path())
        .expect("read scratch dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    assert!(
        entries.iter().any(|e| e.as_bytes() == name.as_bytes()),
        "fixture precondition: the scratch dir must contain an entry whose raw bytes exactly \
         match {name:?} -- the filesystem may have normalised or rejected the non-UTF-8 name; \
         got entries: {entries:?}"
    );

    let out = fx.run(&["worktree", "reclaim", "--yes"]);
    // As in `003`/`004`/`005`: rule out clap's own usage/parse-error exit
    // code first, so a rejected/unrecognized subcommand cannot be mistaken
    // for a correctly-handled removal below.
    assert_ne!(
        out.status.code(),
        Some(2),
        "exit code 2 is clap's own generic usage/parse-error code; an implemented `worktree \
         reclaim` correctly removing this non-UTF-8-named worktree must use a code that does \
         not collide with it; status={:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !combined(&out).contains("Usage:"),
        "stderr still carries clap's own subcommand-usage banner, meaning `worktree reclaim` \
         was not recognized as a real subcommand; out={}",
        combined(&out)
    );

    let text = combined(&out);
    assert!(
        !text.contains("Removed: none"),
        "the command must actually REPORT this worktree as removed, not merely have its \
         directory absent for an unrelated reason -- e.g. the fixture never having created it \
         would also satisfy \"gone\" below without this defect ever being exercised; got:\n{text}"
    );
    assert!(
        text.contains("Removed:"),
        "the report must carry a non-empty \"Removed:\" section naming what was reclaimed; \
         got:\n{text}"
    );

    assert!(
        !wt.exists(),
        "a deck-owned, MERGED, clean worktree must be reclaimed even when its directory name \
         contains a non-UTF-8 byte, exactly like any other reclaimable worktree -- {} still \
         exists\n{}",
        wt.display(),
        text
    );
}

/// Scenario: Two clean, MERGED, *foreign* worktrees sit side by side, whose
/// directory names differ only in one raw byte that is not valid UTF-8
/// (`candidate-\xff` and `candidate-\xfe`). A bare `worktree reclaim` keeps
/// both and lists them as a pending decision. Because `to_string_lossy`
/// collapses every invalid sequence to `U+FFFD`, both bullets render as the
/// same line — so the operator reading the list cannot tell which of the two
/// directories the follow-up `--yes` would remove, even though the removal
/// acts on the distinct byte-exact paths. Asserts the two rendered bullets
/// differ from each other.
#[spec("worktree/reclaim/009")]
#[test]
#[cfg(target_os = "linux")]
fn worktree_reclaim_009_pending_bullets_never_alias_two_distinct_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fx = Fixture::new();
    let name_ff = OsStr::from_bytes(b"candidate-\xff");
    let name_fe = OsStr::from_bytes(b"candidate-\xfe");
    let wt_ff = fx.add_worktree_with_commit_raw(name_ff, "feat/cand-ff");
    let wt_fe = fx.add_worktree_with_commit_raw(name_fe, "feat/cand-fe");
    fx.set_pr_state("feat/cand-ff", "MERGED");
    fx.set_pr_state("feat/cand-fe", "MERGED");
    // Deliberately NOT marked owned, so both land on the `ask` surface --
    // the pending list, which is the surface whose entire purpose is to let
    // the operator decide what a following `--yes` will delete.

    // Fixture precondition, as in `008`: both directories must exist on disk
    // with the EXACT bytes intended. A filesystem that normalised or rejected
    // either name would leave one candidate (or two identical ones), and the
    // "two bullets differ" assertion below would then pass or fail for a
    // reason that has nothing to do with the rendering under test.
    let entries: Vec<_> = std::fs::read_dir(fx._scratch.path())
        .expect("read scratch dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    for name in [name_ff, name_fe] {
        assert!(
            entries.iter().any(|e| e.as_bytes() == name.as_bytes()),
            "fixture precondition: the scratch dir must contain an entry whose raw bytes \
             exactly match {name:?} -- the filesystem may have normalised or rejected the \
             non-UTF-8 name; got entries: {entries:?}"
        );
    }
    assert_ne!(
        name_ff.as_bytes(),
        name_fe.as_bytes(),
        "fixture precondition: the two candidate names must genuinely differ in their raw \
         bytes, or there is no aliasing to detect"
    );

    let out = fx.run(&["worktree", "reclaim"]);
    // As in `003`/`004`/`005`/`008`: rule out clap's own usage/parse-error
    // exit code first, so a rejected subcommand (which prints no pending
    // section at all) cannot be mistaken for a correctly-rendered one.
    assert_ne!(
        out.status.code(),
        Some(2),
        "exit code 2 is clap's own generic usage/parse-error code; status={:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !combined(&out).contains("Usage:"),
        "stderr still carries clap's own subcommand-usage banner, meaning `worktree reclaim` \
         was not recognized as a real subcommand rather than being handled and raising a \
         pending decision; out={}",
        combined(&out)
    );

    // Control: a bare `reclaim` must KEEP both foreign worktrees. Without
    // this, "two bullets" could be read off a report about directories that
    // were already gone -- the pending list is a decision still to be made,
    // not a post-mortem.
    assert!(
        wt_ff.exists() && wt_fe.exists(),
        "a bare `worktree reclaim` must keep both foreign worktrees pending, not remove them; \
         ff exists={} fe exists={}\n{}",
        wt_ff.exists(),
        wt_fe.exists(),
        combined(&out)
    );

    // Compare RAW BYTES, never `String::from_utf8_lossy`. `combined()` would
    // apply the very lossy conversion under test, so two byte-distinct
    // bullets could be reported as colliding purely because the harness
    // collapsed them.
    let bullets: Vec<&[u8]> = out
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| l.starts_with(b"  - "))
        .collect();
    let text = combined(&out);
    assert_eq!(
        bullets.len(),
        2,
        "both foreign worktrees must appear as their own pending bullet -- got {} bullet \
         line(s); full output:\n{text}",
        bullets.len()
    );
    assert_ne!(
        bullets[0],
        bullets[1],
        "the pending list exists so the operator can decide what `--yes` will delete, and the \
         removal acts on the distinct byte-exact paths -- so two directories that differ in \
         their raw bytes must never render as the same line. Got two identical bullets \
         {:?}; full output:\n{text}",
        String::from_utf8_lossy(bullets[0])
    );
}

/// Scenario: A worktree created through the deck's REAL creation path
/// (`issue_dispatch_run::create_worktree`, the only `git worktree add` in
/// `src/` — every dispatch and every issue-dispatch fire goes through it) is
/// recognised by `worktree list` as deck-owned, and — being MERGED and clean —
/// gets the unattended `remove` verdict rather than `ask`. A sibling worktree
/// created by a plain `git worktree add`, identical in every other respect,
/// is the control: it must still read as foreign. Pins issue #425, where the
/// marker was read but never written, so the deck's own worktrees could only
/// ever reach `ask`.
#[spec("worktree/reclaim/010")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_010_worktree_created_by_the_deck_reads_as_owned() {
    let fx = Fixture::new();

    // The subject: created the way production creates one. Deliberately NOT
    // `fx.mark_owned` — the whole point is that nothing marks it by hand.
    let deck_made = fx._scratch.path().join("wt-deck-made");
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(dot_agent_deck::issue_dispatch_run::create_worktree(
            &fx.repo,
            &deck_made,
            "feat/deck-made",
            false,
            dot_agent_deck::worktree_owner::Creator::dispatch("deck-made"),
        ))
        .expect("the deck's creation path must create the worktree");
    assert_eq!(
        outcome,
        dot_agent_deck::issue_dispatch_run::WorktreeCreation::Created,
        "fixture precondition: the worktree must have been genuinely CREATED here — an \
         already-claimed directory is somebody else's and is never marked"
    );
    assert!(deck_made.is_dir(), "fixture precondition: worktree on disk");
    fx.set_pr_state("feat/deck-made", "MERGED");

    // The control: same repo, same shape, same MERGED PR — created by a plain
    // `git worktree add`, which is what an orchestrator or a human runs. It
    // must stay foreign, or "owned" would be measuring nothing.
    let hand_made = fx.add_worktree_with_commit("wt-hand-made", "feat/hand-made");
    fx.set_pr_state("feat/hand-made", "MERGED");

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
    let entry_for = |needle: &str| -> serde_json::Value {
        doc.get("worktrees")
            .and_then(|w| w.as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|e| {
                        e.get("path")
                            .and_then(|p| p.as_str())
                            .is_some_and(|p| p.contains(needle))
                    })
                    .cloned()
            })
            .unwrap_or_else(|| panic!("no entry for {needle} in:\n{stdout}"))
    };

    let deck_entry = entry_for("wt-deck-made");
    assert_eq!(
        deck_entry.get("owned").and_then(|o| o.as_bool()),
        Some(true),
        "a worktree the deck created must read as deck-owned — the marker is written at \
         creation time; got entry:\n{deck_entry}\nfull document:\n{stdout}"
    );
    assert_eq!(
        deck_entry.get("verdict").and_then(|v| v.as_str()),
        Some("remove"),
        "MERGED + clean + deck-owned is the one combination that reclaims unattended; \
         got entry:\n{deck_entry}\nfull document:\n{stdout}"
    );

    let hand_entry = entry_for("wt-hand-made");
    assert_eq!(
        hand_entry.get("owned").and_then(|o| o.as_bool()),
        Some(false),
        "control: a worktree created by a plain `git worktree add` must stay foreign — the \
         deck never claims what it did not create; got entry:\n{hand_entry}\nfull \
         document:\n{stdout}"
    );
    assert_eq!(
        hand_entry.get("verdict").and_then(|v| v.as_str()),
        Some("ask"),
        "a foreign worktree, however reclaimable it otherwise looks, must require an \
         explicit confirmation; got entry:\n{hand_entry}\nfull document:\n{stdout}"
    );

    assert!(
        hand_made.is_dir() && deck_made.is_dir(),
        "`worktree list` must not remove anything"
    );
}

/// Scenario: Two worktrees of the same repo are marked owned by two DIFFERENT
/// orchestrations. With `DOT_AGENT_DECK_WORKTREE_OWNER` set to one of them,
/// `worktree list --mine` must list the worktree that orchestration owns and
/// must NOT list the other, same-repo worktree owned by a different
/// orchestration (fork #166 M3.0 -- the happy path the whole milestone is
/// for).
#[spec("worktree/reclaim/031")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_031_mine_keeps_own_and_excludes_other_owner() {
    let fx = Fixture::new();
    let wt_mine = fx.add_worktree_with_commit("wt-mine", "feat/mine");
    fx.set_pr_state("feat/mine", "OPEN");
    fx.mark_owned_with_creator(&wt_mine, "orchestration:me");

    let wt_other = fx.add_worktree_with_commit("wt-other-owned", "feat/other-owned");
    fx.set_pr_state("feat/other-owned", "OPEN");
    fx.mark_owned_with_creator(&wt_other, "orchestration:someone-else");

    let out = fx.run_with_owner(&["worktree", "list", "--mine"], Some("orchestration:me"));
    assert!(
        out.status.success(),
        "`worktree list --mine` must succeed once an owner identity is known via \
         DOT_AGENT_DECK_WORKTREE_OWNER; got {:?} out={}",
        out.status,
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        text.contains("wt-mine"),
        "`--mine` must list a worktree owned by the exact identity in \
         DOT_AGENT_DECK_WORKTREE_OWNER; got:\n{text}"
    );
    assert!(
        !text.contains("wt-other-owned"),
        "`--mine` must exclude a same-repo worktree owned by a DIFFERENT orchestration -- \
         handing it over would be the fork #74 collision this PRD exists to prevent; got:\n{text}"
    );
}

/// Scenario: A worktree's ownership marker is written directly to disk --
/// standing in for a PRIOR, now-exited orchestration process, since there is
/// no CLI command that creates a worktree and no daemon involved in `--mine`
/// at all. `worktree list --mine` is then run in a brand-new subprocess
/// (`Fixture::run_with_owner` always spawns a fresh OS process) that shares
/// no memory, no daemon, and no cache with whatever wrote the marker -- only
/// the on-disk marker and the env var are available to it. The fresh process
/// must still report the worktree as mine: this is M3.0's actual claim
/// ("correctly after a restart"), and restart-correctness must fall out of
/// matching two plain strings, never out of anything held in memory.
#[spec("worktree/reclaim/032")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_032_mine_matches_after_simulated_restart() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-restart", "feat/restart");
    fx.set_pr_state("feat/restart", "OPEN");

    // Written as a plain filesystem operation, not through any
    // dot-agent-deck process -- nothing here is "in memory" for a later
    // `--mine` invocation to have shared access to.
    fx.mark_owned_with_creator(&wt, "orchestration:restart-survivor");

    // A brand-new subprocess, spawned independently, with no daemon running
    // and no knowledge of when or how the marker above was written.
    let out = fx.run_with_owner(
        &["worktree", "list", "--mine"],
        Some("orchestration:restart-survivor"),
    );
    assert!(
        out.status.success(),
        "`worktree list --mine` must succeed in a fresh process against a marker written \
         earlier by a different process; got {:?} out={}",
        out.status,
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        text.contains("wt-restart"),
        "a fresh process must still match a marker written by an earlier, unrelated process -- \
         `--mine` must depend only on the on-disk marker and the env var, never on in-memory \
         state a restart would lose; got:\n{text}"
    );
}

/// Scenario: With `DOT_AGENT_DECK_WORKTREE_OWNER` entirely ABSENT, `worktree
/// list --mine` must fail loudly -- non-zero exit, and a message naming what
/// is missing. It must NOT fall back to listing every worktree (that would
/// hand one orchestration another's worktrees) and must NOT silently print an
/// empty list as if the answer were "none owned" (fork #166 M3.0 -- a wrong
/// answer here is worse than no answer).
#[spec("worktree/reclaim/033")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_033_mine_fails_loudly_when_owner_env_absent() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-someones", "feat/someones");
    fx.set_pr_state("feat/someones", "OPEN");
    fx.mark_owned_with_creator(&wt, "orchestration:someone");

    let out = fx.run_with_owner(&["worktree", "list", "--mine"], None);
    assert!(
        !out.status.success(),
        "`--mine` with DOT_AGENT_DECK_WORKTREE_OWNER absent must fail loudly (non-zero exit), \
         never succeed by falling back to \"everything\" or \"nothing\"; got {:?} out={}",
        out.status,
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        text.contains("DOT_AGENT_DECK_WORKTREE_OWNER"),
        "the failure must name the missing variable so the caller knows what to supply; \
         got:\n{text}"
    );
    assert!(
        !text.contains("wt-someones"),
        "an absent owner identity must never be treated as \"list everything\" -- got:\n{text}"
    );
}

/// Scenario: With `DOT_AGENT_DECK_WORKTREE_OWNER` set to the literal
/// `orchestration:unknown` sentinel -- the value `src/ui.rs` writes when no
/// typed name was available at spawn -- `worktree list --mine` must refuse
/// exactly as it does when the variable is absent entirely (`027`). The
/// sentinel is never a real identity: two nameless orchestrations must never
/// be treated as matching each other's worktrees just because they share the
/// same placeholder string (fork #166 M2.4/M3.0).
#[spec("worktree/reclaim/034")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_034_mine_refuses_the_unknown_sentinel() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-nameless", "feat/nameless");
    fx.set_pr_state("feat/nameless", "OPEN");
    // Written by a DIFFERENT nameless orchestration that also fell back to
    // the sentinel -- if `--mine` ever matched sentinel-to-sentinel, this is
    // the worktree it would wrongly hand over.
    fx.mark_owned_with_creator(&wt, ORCHESTRATION_UNKNOWN_SENTINEL);

    let out = fx.run_with_owner(
        &["worktree", "list", "--mine"],
        Some(ORCHESTRATION_UNKNOWN_SENTINEL),
    );
    assert!(
        !out.status.success(),
        "`--mine` with the owner env set to the `orchestration:unknown` sentinel must refuse \
         exactly as it does when the variable is absent -- a sentinel is never an identity; got \
         {:?} out={}",
        out.status,
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        text.contains("DOT_AGENT_DECK_WORKTREE_OWNER") || text.to_lowercase().contains("unknown"),
        "the failure must name what is wrong (the missing/sentinel identity), not fail silently; \
         got:\n{text}"
    );
    assert!(
        !text.contains("wt-nameless"),
        "two nameless orchestrations sharing the `orchestration:unknown` sentinel must never be \
         treated as matching -- got:\n{text}"
    );
}

/// Scenario: With `DOT_AGENT_DECK_WORKTREE_OWNER` exported but set to a
/// literal empty (`Some("")`) or whitespace-only string -- an ordinary way
/// to reach this via a wrapper script's `export VAR=` or an unset shell
/// variable interpolated into `VAR="$OTHER"` -- `worktree list --mine` must
/// refuse exactly as it does when the variable is absent entirely (`027`).
/// `std::env::var` returns `Ok("")` for this case, so without an explicit
/// refusal it falls into the "use it as the filter" arm and produces a
/// definitive-looking "no worktrees owned by" with a blank subject and exit
/// 0 -- a wrong answer arriving silently (fork #166 M3.0, PR #215 round-3
/// reviewer F4). It must also do the opposite correctly: a legitimate
/// identity carrying stray leading/trailing whitespace must still MATCH the
/// marker once both sides are sanitized identically, and the
/// `orchestration:unknown` sentinel must still be refused even carrying a
/// trailing control character that `trim` alone would not strip (round-4
/// fixup R4-1 -- before it, the filter used the raw env value while the
/// marker was always sanitized, so the padded identity matched nothing and
/// the control-suffixed sentinel slipped past the refusal).
#[spec("worktree/reclaim/035")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_035_mine_fails_loudly_when_owner_env_empty() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-someones", "feat/someones");
    fx.set_pr_state("feat/someones", "OPEN");
    fx.mark_owned_with_creator(&wt, "orchestration:someone");

    for empty in ["   ", ""] {
        let out = fx.run_with_owner(&["worktree", "list", "--mine"], Some(empty));
        assert!(
            !out.status.success(),
            "`--mine` with DOT_AGENT_DECK_WORKTREE_OWNER set to {empty:?} must fail loudly \
             (non-zero exit), never succeed by silently filtering on a value that can never \
             match anything; got {:?} out={}",
            out.status,
            combined(&out)
        );
        let text = combined(&out);
        assert!(
            text.contains("DOT_AGENT_DECK_WORKTREE_OWNER"),
            "the failure must name the empty variable so the caller knows what to supply; \
             got:\n{text}"
        );
        assert!(
            !text.contains("wt-someones"),
            "an empty owner identity must never be treated as \"list everything\" -- got:\n{text}"
        );
    }

    // Round-4 fixup (R4-1): a legitimate identity carrying stray whitespace
    // must still match the (always-sanitized) marker -- the raw filter used
    // to make this unmatchable even though the worktree genuinely is owned
    // by it.
    let padded = fx.run_with_owner(
        &["worktree", "list", "--mine"],
        Some(" orchestration:someone "),
    );
    assert!(
        padded.status.success(),
        "a stray-whitespace identity that matches the marker once sanitized must be treated as \
         a match, not silently filtered to nothing; got {:?} out={}",
        padded.status,
        combined(&padded)
    );
    assert!(
        combined(&padded).contains("wt-someones"),
        "the padded identity must name the worktree it owns; got:\n{}",
        combined(&padded)
    );

    // Round-4 fixup (R4-1): the sentinel refusal must fire on the SANITIZED
    // value, not the merely-trimmed one -- a trailing control character
    // survives `trim` but not `sanitize_marker_creator`.
    let sentinel_with_control = format!("{ORCHESTRATION_UNKNOWN_SENTINEL}\u{7}");
    let control_out = fx.run_with_owner(
        &["worktree", "list", "--mine"],
        Some(&sentinel_with_control),
    );
    assert!(
        !control_out.status.success(),
        "the sentinel with a trailing control character must still be refused, not treated as \
         an ordinary near-miss identity; got {:?} out={}",
        control_out.status,
        combined(&control_out)
    );
    assert!(
        !combined(&control_out).contains("wt-someones"),
        "must never fall through to listing the fixture's worktree; got:\n{}",
        combined(&control_out)
    );
}
