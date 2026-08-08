// The whole file drives real git worktrees and a generated `/bin/sh` `gh`
// stub, so it is Unix-only at the source level (POSIX shell + a shebang +
// chmod +x). `#![cfg(unix)]` makes the crate empty on the Windows
// cross-platform build target rather than failing to compile there.
#![cfg(unix)]

//! Guards `.claude/skills/dot-ai-tag-release/cleanup.sh` — fork issue #140.
//!
//! `cleanup.sh` is detection-only, but its output is the list a human or
//! agent then feeds straight into `git worktree remove` / `git branch -d` /
//! `git push origin --delete`. It went wrong for real during the v0.36.0
//! release: the release worker ran the detection step, saw the ROOT CHECKOUT
//! and the `fork-only` worktree both listed as removal candidates, and
//! stopped rather than acting on the output. Had the worker trusted the list
//! (as the skill's own step tells it to), it would have deleted the
//! long-lived checkout the running daemon and worker panes are attached to —
//! the exact incident CLAUDE.md rule 1 exists to prevent.
//!
//! Three independent defects, all pinned here:
//!   - D1: the worktree loop excludes only the worktree the script happens to
//!     be running from, not the default branch — so the root checkout is
//!     offered whenever the script runs from any OTHER worktree, which is
//!     precisely what rule 1 requires.
//!   - D2: `fork-only` is excluded nowhere. It is a trivial ancestor of
//!     `origin/main` immediately after every fork/upstream sync, so it reads
//!     as "merged" exactly when this skill is likely to run.
//!   - D3: both `gh pr list` calls omit `--repo`, so `gh` resolves against
//!     this repo's PARENT (`vfarcic/dot-agent-deck`) instead of the fork —
//!     the open-PR guard protects nothing on the fork's own open PRs, and
//!     every `gh` failure is swallowed by `|| true`, so a degraded run is
//!     indistinguishable from a clean one.
//!
//! Fast tier on purpose: no daemon, no PTY, no TUI — just git plus a stub
//! `gh` on `PATH`. Hermetic: `origin` is a local bare repo (no network), and
//! the stub `gh` replaces the real one so CI's authenticated, real `gh`
//! never gets a chance to answer with this repo's actual (irrelevant, and
//! non-deterministic) live PR state.

mod common;

use common::sh_quote_path;
use std::path::{Path, PathBuf};
use std::process::Command;

/// This fork's own slug — see `docs/develop/fork-sync-workflow.md`.
const FORK_SLUG: &str = "prageethw/dot-agent-deck";
/// The parent repo `gh` silently resolves to when no `--repo` is passed —
/// same doc, same trap this file is measuring against `cleanup.sh`.
const UPSTREAM_SLUG: &str = "vfarcic/dot-agent-deck";

fn cleanup_script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".claude/skills/dot-ai-tag-release/cleanup.sh")
}

/// Run `git` with `args` in `dir`, panicking with full stdout/stderr on
/// failure so a broken fixture is diagnosable instead of a mystifying
/// downstream test failure.
fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap_or_else(|e| panic!("spawn `git {args:?}` in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "`git {args:?}` in {dir:?} failed (status {:?})\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A synthetic git remote plus clone shaped like the real fork: a root
/// checkout on `main`, a `fork-only` branch that is a trivial ancestor of
/// `origin/main`, and a genuinely merged feature branch alongside an
/// unmerged one — each with its own worktree where the fixture needs one.
struct RepoFixture {
    // Held for the fixture's lifetime only to keep the tempdir alive — every
    // path derived from it is used via the fields below, not this handle
    // (docs/develop/red-confirmation.md: a `TempDir` dropped early is a
    // second, unrelated cause of RED hiding behind the intended one).
    _root: tempfile::TempDir,
    /// A worktree checked out on the long-lived `fork-only` branch.
    fork_only_worktree: PathBuf,
    /// A worktree checked out on a genuinely merged feature branch.
    feat_merged_worktree: PathBuf,
}

fn build_fixture() -> RepoFixture {
    let root = tempfile::tempdir().expect("tempdir for git fixture root");
    let origin = root.path().join("origin.git");
    let clone_dir = root.path().join("clone");
    let fork_only_worktree = root.path().join("wt-fork-only");
    let feat_merged_worktree = root.path().join("wt-feat-merged");
    let scratch_unmerged_worktree = root.path().join("wt-feat-unmerged-scratch");

    std::fs::create_dir_all(&clone_dir).expect("create clone dir");

    let origin_str = origin.to_str().expect("origin path is UTF-8");
    run_git(root.path(), &["init", "--bare", "-b", "main", origin_str]);

    run_git(&clone_dir, &["init", "-b", "main"]);
    run_git(&clone_dir, &["config", "commit.gpgsign", "false"]);
    run_git(&clone_dir, &["remote", "add", "origin", origin_str]);

    std::fs::write(clone_dir.join("README.md"), "fixture\n").expect("write seed file");
    run_git(&clone_dir, &["add", "README.md"]);
    run_git(&clone_dir, &["commit", "-m", "initial"]);
    run_git(&clone_dir, &["push", "origin", "main"]);
    run_git(&clone_dir, &["remote", "set-head", "origin", "main"]);

    // `fork-only` branches straight off `main`'s tip with no commits of its
    // own, so it is trivially an ancestor of `origin/main` — mirroring the
    // real branch immediately after a fork/upstream sync resets `main` onto
    // it (D2).
    run_git(&clone_dir, &["branch", "fork-only", "main"]);
    run_git(&clone_dir, &["push", "origin", "fork-only"]);

    // `feat/merged`: a real feature branch, fast-forward merged into `main`.
    let feat_merged_wt_str = feat_merged_worktree
        .to_str()
        .expect("feat/merged worktree path is UTF-8");
    run_git(
        &clone_dir,
        &[
            "worktree",
            "add",
            "-b",
            "feat/merged",
            feat_merged_wt_str,
            "main",
        ],
    );
    std::fs::write(feat_merged_worktree.join("feature.txt"), "merged\n")
        .expect("write feature file");
    run_git(&feat_merged_worktree, &["add", "feature.txt"]);
    run_git(
        &feat_merged_worktree,
        &["commit", "-m", "feat: merged work"],
    );
    run_git(&clone_dir, &["merge", "--ff-only", "feat/merged"]);
    run_git(&clone_dir, &["push", "origin", "main"]);
    run_git(&clone_dir, &["push", "origin", "feat/merged"]);

    // `feat/unmerged`: diverges from `main` and is never merged back — proves
    // the fix does not turn into "exclude everything".
    let scratch_wt_str = scratch_unmerged_worktree
        .to_str()
        .expect("feat/unmerged scratch worktree path is UTF-8");
    run_git(
        &clone_dir,
        &[
            "worktree",
            "add",
            "-b",
            "feat/unmerged",
            scratch_wt_str,
            "main",
        ],
    );
    std::fs::write(scratch_unmerged_worktree.join("wip.txt"), "wip\n").expect("write wip file");
    run_git(&scratch_unmerged_worktree, &["add", "wip.txt"]);
    run_git(
        &scratch_unmerged_worktree,
        &["commit", "-m", "wip: not merged"],
    );
    run_git(&clone_dir, &["push", "origin", "feat/unmerged"]);
    run_git(&clone_dir, &["worktree", "remove", scratch_wt_str]);

    let fork_only_wt_str = fork_only_worktree
        .to_str()
        .expect("fork-only worktree path is UTF-8");
    run_git(
        &clone_dir,
        &["worktree", "add", fork_only_wt_str, "fork-only"],
    );

    run_git(&clone_dir, &["fetch", "--prune", "--quiet", "origin"]);

    RepoFixture {
        _root: root,
        fork_only_worktree,
        feat_merged_worktree,
    }
}

/// A generated stub `gh` binary. Every scenario response starts empty, so a
/// test only has to set the response(s) its scenario actually needs.
struct GhStub {
    dir: tempfile::TempDir,
}

const GH_STUB_RESPONSE_FILES: &[&str] = &[
    "open_fork.tsv",
    "open_upstream.tsv",
    "open_norepo.tsv",
    "merged.tsv",
];

impl GhStub {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir for gh stub");
        for name in GH_STUB_RESPONSE_FILES {
            std::fs::write(dir.path().join(name), "")
                .unwrap_or_else(|e| panic!("seed empty {name}: {e}"));
        }
        std::fs::write(dir.path().join("exit_code"), "0").expect("seed exit code");

        let script_path = dir.path().join("gh");
        std::fs::write(&script_path, gh_stub_script(dir.path())).expect("write gh stub script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod gh stub script");
        }

        GhStub { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `gh pr list --state open ... --repo prageethw/dot-agent-deck` returns
    /// these branch names as `headRefName`s of open PRs on the fork.
    fn set_open_fork(&self, branches: &[&str]) {
        self.write_response("open_fork.tsv", &branches.join("\n"));
    }

    /// Same, but for `--repo vfarcic/dot-agent-deck` (upstream).
    fn set_open_upstream(&self, branches: &[&str]) {
        self.write_response("open_upstream.tsv", &branches.join("\n"));
    }

    /// Make every `gh` invocation exit non-zero, simulating an auth failure
    /// or an API outage.
    fn set_exit_code(&self, code: i32) {
        self.write_response("exit_code", &code.to_string());
    }

    fn write_response(&self, name: &str, body: &str) {
        std::fs::write(self.dir.path().join(name), body)
            .unwrap_or_else(|e| panic!("write gh stub response {name}: {e}"));
    }
}

/// Build the `/bin/sh` stub `gh` script. It reads its own `--state` and
/// `--repo` arguments and `cat`s the matching pre-seeded response file,
/// rather than trying to reimplement `--json`/`--jq` — the response files
/// already hold exactly what `cleanup.sh`'s own `--jq` filters expect to
/// read. `--repo` is deliberately matched by exact literal value (baked in
/// at generation time, never read from a shell variable) so the SAME
/// generated script tells "queried the fork" apart from "queried upstream"
/// apart from "queried neither" (today's bug, D3).
fn gh_stub_script(dir: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         set -eu\n\
         state=\"\"\n\
         repo=\"\"\n\
         prev=\"\"\n\
         for arg in \"$@\"; do\n\
         \x20\x20case \"$prev\" in\n\
         \x20\x20\x20\x20--state) state=\"$arg\" ;;\n\
         \x20\x20\x20\x20--repo) repo=\"$arg\" ;;\n\
         \x20\x20esac\n\
         \x20\x20prev=\"$arg\"\n\
         done\n\
         \n\
         code=$(cat {exit_code_path})\n\
         if [ \"$code\" != \"0\" ]; then\n\
         \x20\x20exit \"$code\"\n\
         fi\n\
         \n\
         case \"$state\" in\n\
         \x20\x20open)\n\
         \x20\x20\x20\x20case \"$repo\" in\n\
         \x20\x20\x20\x20\x20\x20{fork_slug}) cat {open_fork_path} ;;\n\
         \x20\x20\x20\x20\x20\x20{upstream_slug}) cat {open_upstream_path} ;;\n\
         \x20\x20\x20\x20\x20\x20\"\") cat {open_norepo_path} ;;\n\
         \x20\x20\x20\x20\x20\x20*) ;;\n\
         \x20\x20\x20\x20esac\n\
         \x20\x20\x20\x20;;\n\
         \x20\x20merged)\n\
         \x20\x20\x20\x20cat {merged_path}\n\
         \x20\x20\x20\x20;;\n\
         esac\n\
         exit 0\n",
        exit_code_path = sh_quote_path(&dir.join("exit_code")),
        fork_slug = FORK_SLUG,
        upstream_slug = UPSTREAM_SLUG,
        open_fork_path = sh_quote_path(&dir.join("open_fork.tsv")),
        open_upstream_path = sh_quote_path(&dir.join("open_upstream.tsv")),
        open_norepo_path = sh_quote_path(&dir.join("open_norepo.tsv")),
        merged_path = sh_quote_path(&dir.join("merged.tsv")),
    )
}

struct CleanupOutput {
    stdout: String,
    stderr: String,
}

impl CleanupOutput {
    /// Lines under a `HEADER:\n  entry\n  entry\n` block, trimmed of their
    /// two-space indent. Returns an empty vec if the header never appears
    /// (e.g. the script took its `NOTHING_TO_CLEAN=true` early exit).
    fn section(&self, header: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut in_section = false;
        for line in self.stdout.lines() {
            if line == header {
                in_section = true;
                continue;
            }
            if in_section {
                if let Some(entry) = line.strip_prefix("  ") {
                    out.push(entry.to_string());
                } else {
                    break;
                }
            }
        }
        out
    }

    /// Branch names offered in `WORKTREES:` (the `…|branch` suffix of each
    /// entry).
    fn worktree_branches(&self) -> Vec<String> {
        self.section("WORKTREES:")
            .into_iter()
            .filter_map(|line| line.rsplit('|').next().map(str::to_string))
            .collect()
    }

    fn local_branches(&self) -> Vec<String> {
        self.section("LOCAL_BRANCHES:")
    }

    fn remote_branches(&self) -> Vec<String> {
        self.section("REMOTE_BRANCHES:")
    }

    fn degraded(&self) -> bool {
        self.stdout
            .lines()
            .any(|line| line.trim() == "PR_STATE_DEGRADED=true")
    }
}

/// Run `cleanup.sh` with `cwd` as the working directory and `gh_stub_dir`
/// prepended to `PATH` so `command -v gh` resolves to the stub, never CI's
/// real, authenticated `gh`.
fn run_cleanup(cwd: &Path, gh_stub_dir: &Path) -> CleanupOutput {
    let script = cleanup_script_path();
    assert!(
        script.is_file(),
        "cleanup.sh not found at {script:?} — CARGO_MANIFEST_DIR resolution is broken"
    );

    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_var = std::ffi::OsString::from(gh_stub_dir);
    path_var.push(":");
    path_var.push(&inherited_path);

    let out = Command::new(&script)
        .current_dir(cwd)
        .env("PATH", path_var)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap_or_else(|e| panic!("spawn {script:?}: {e}"));

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // A sanity check independent of whatever a given test is pinning: every
    // successful run prints this header before anything else. Its absence
    // means the script crashed (or was killed) before producing any of the
    // sections a test would otherwise read as "empty", which would let an
    // assertion of the form `!contains(...)` pass while proving nothing —
    // exactly the "RED for the wrong reason" trap docs/develop/red-confirmation.md
    // warns about. Fail loudly here, with the exit status and stderr, instead
    // of letting that happen silently downstream.
    assert!(
        stdout.contains("DEFAULT_BRANCH="),
        "cleanup.sh did not print its DEFAULT_BRANCH= header — it crashed or \
         was killed before producing any output, in a way unrelated to \
         whatever this test is asserting. cwd: {cwd:?}, status: {:?}\nstdout: \
         {stdout}\nstderr: {stderr}",
        out.status.code(),
    );

    CleanupOutput { stdout, stderr }
}

/// Scenario: build the synthetic repo, run `cleanup.sh` from the
/// `feat/merged` worktree (never the root checkout — that is the condition
/// that exposes D1), and confirm `WORKTREES:` never lists the root checkout
/// (on `main`) or the `fork-only` worktree.
#[test]
fn release_cleanup_001_worktrees_exclude_root_checkout_and_fork_only() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    let branches = out.worktree_branches();
    assert!(
        !branches.contains(&"main".to_string()),
        "WORKTREES: must never offer the root checkout (branch `main`) for \
         removal — fork issue #140 (D1): the worktree loop guards only \
         against the CURRENT worktree, and `main` is trivially an ancestor \
         of itself. Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !branches.contains(&"fork-only".to_string()),
        "WORKTREES: must never offer the `fork-only` worktree for removal — \
         fork issue #140 (D2): `fork-only` is a trivial ancestor of \
         `origin/main` and is excluded nowhere. Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: run `cleanup.sh` from the `fork-only` worktree, so `feat/merged`
/// is an ordinary non-current worktree, and confirm the new long-lived-branch
/// guard does not over-exclude: a genuinely merged feature worktree must
/// still be offered.
#[test]
fn release_cleanup_002_worktrees_still_offer_a_genuinely_merged_feature() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.fork_only_worktree, gh.path());

    let branches = out.worktree_branches();
    assert!(
        branches.contains(&"feat/merged".to_string()),
        "WORKTREES: a genuinely merged feature worktree (`feat/merged`) must \
         still be offered — the long-lived guard is meant to exclude only \
         `main` and `fork-only`, not every worktree. Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: run `cleanup.sh` from the `feat/merged` worktree and confirm
/// both `LOCAL_BRANCHES:` and `REMOTE_BRANCHES:` exclude `fork-only`, even
/// though it is a plain ancestor of `origin/main`.
#[test]
fn release_cleanup_003_local_and_remote_branches_exclude_fork_only() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        !out.local_branches().contains(&"fork-only".to_string()),
        "LOCAL_BRANCHES: must exclude `fork-only` — fork issue #140 (D2). \
         Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.remote_branches().contains(&"fork-only".to_string()),
        "REMOTE_BRANCHES: must exclude `fork-only` — fork issue #140 (D2). \
         Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: a stub `gh` reports an open PR on the FORK
/// (`--repo prageethw/dot-agent-deck`) for `feat/merged`, an otherwise
/// ancestor-merged branch. Confirms it is excluded from `LOCAL_BRANCHES:` —
/// i.e. that the open-PR guard is genuinely consulted against the fork's own
/// repository, not left unpinned (D3). Today's script never passes `--repo`
/// at all, so this is expected to fail as "not implemented yet": the stub's
/// `open_fork.tsv` response is unreachable without the pin.
#[test]
fn release_cleanup_004_local_branch_excluded_when_fork_reports_open_pr() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    gh.set_open_fork(&["feat/merged"]);
    let out = run_cleanup(&fixture.fork_only_worktree, gh.path());

    assert!(
        !out.local_branches().contains(&"feat/merged".to_string()),
        "LOCAL_BRANCHES: `feat/merged` has an OPEN PR on the fork \
         ({FORK_SLUG}) per the stub `gh`, so it must be excluded even though \
         it is ancestor-merged. This requires the open-PR query to be pinned \
         with `--repo {FORK_SLUG}` (fork issue #140, D3) — today's script \
         passes no `--repo` at all, so this guard is a no-op against the \
         fork's own open PRs. Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: a stub `gh` reports an open PR on UPSTREAM
/// (`--repo vfarcic/dot-agent-deck`) whose head name matches local branch
/// `feat/merged`. Confirms it is still excluded from `LOCAL_BRANCHES:` — the
/// deliberate, explicit version of the protection D3 says today's script
/// gets only by accident. Expected to fail as "not implemented yet": this
/// pins the SECOND upstream query the coder needs to add, which does not
/// exist in the script today.
#[test]
fn release_cleanup_005_local_branch_excluded_when_upstream_reports_open_pr() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    gh.set_open_upstream(&["feat/merged"]);
    let out = run_cleanup(&fixture.fork_only_worktree, gh.path());

    assert!(
        !out.local_branches().contains(&"feat/merged".to_string()),
        "LOCAL_BRANCHES: an OPEN PR on upstream ({UPSTREAM_SLUG}) whose head \
         name matches local branch `feat/merged` must still exclude it. This \
         requires a SECOND, explicit query pinned to `--repo {UPSTREAM_SLUG}` \
         (fork issue #140, D3) — today's script has only the one, unpinned \
         query. Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: a stub `gh` exits non-zero on every call (an auth failure or API
/// outage). Confirms the output carries `PR_STATE_DEGRADED=true` rather than
/// looking identical to a clean run. Expected to fail as "not implemented
/// yet": today every `gh` failure is swallowed by `|| true` and no such
/// marker exists anywhere in the script's output.
#[test]
fn release_cleanup_006_degraded_marker_when_gh_fails() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    gh.set_exit_code(17);
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        out.degraded(),
        "a failing `gh` (stub exit 17) must produce `PR_STATE_DEGRADED=true` \
         so a degraded PR-state run is distinguishable from a clean one — \
         today every `gh` call is swallowed by `|| true` (fork issue #140, \
         D3; the same 'empty gate looks like a passed gate' class as \
         CLAUDE.md rule 8 / issue #146). Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}
