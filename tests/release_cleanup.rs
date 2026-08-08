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
//! Round 2 (PR #147) closes six test-side review/audit findings on the round
//! 1 suite and adds coverage for six more PRODUCTION target behaviours the
//! coder implements after this file lands red: root-checkout exclusion by
//! PATH identity rather than branch name (target 1), no hardcoded slug
//! fallback when a remote's slug cannot be derived (target 2), a missing
//! `upstream` remote NOT counting as a degradation while a present-but-
//! unparseable one DOES (target 3), `--limit` truncation detection (target
//! 4), a failed `git fetch` setting the degraded marker (target 5), and a
//! TAB (not `|`) separator in `WORKTREES:` lines (target 6). See the
//! individual test doc comments below for which finding or target behaviour
//! each one pins.
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

/// Shared prelude for every fixture below: a bare `origin` and a clone with
/// one commit on `main`, pushed, with `origin/HEAD` set. Returns `(origin
/// bare dir, clone dir)`; callers add whatever remotes, branches, and
/// worktrees their scenario needs on top.
///
/// `remote_url_override`, when set, configures `origin`'s remote URL to that
/// value (e.g. a GitHub-shaped URL) instead of the bare repo's own local
/// path, and adds a REPO-LOCAL (never `--global` — see the
/// `GIT_CONFIG_GLOBAL=/dev/null` isolation in [`run_cleanup`])
/// `url.<local path>.insteadOf <override>` mapping, so `git fetch`/`git
/// push` by the remote NAME still resolve to the local bare repo — no
/// network, fully hermetic.
///
/// IMPORTANT, and the reason this is spelled out here: `git remote get-url`
/// APPLIES `insteadOf` rewriting (verified against git 2.55.0 — it returns
/// the REWRITTEN local path, not the configured override), so a
/// `slug_from_remote` built on `git remote get-url` would see the local path
/// here, not the GitHub-shaped URL, and this fixture would silently fail to
/// exercise real derivation (reviewer F3 / fork issue #140 T1). `git config
/// --get remote.<name>.url` reads the raw configured value and is NOT
/// rewritten — that is what makes this technique work, and it is also the
/// more correct choice for any real insteadOf-configured mirror, where
/// `get-url` would derive the mirror's slug instead of the intended one.
fn init_repo(root: &Path, remote_url_override: Option<&str>) -> (PathBuf, PathBuf) {
    let origin_dir = root.join("origin.git");
    let clone_dir = root.join("clone");
    std::fs::create_dir_all(&clone_dir).expect("create clone dir");
    let origin_dir_str = origin_dir.to_str().expect("origin path is UTF-8");

    run_git(root, &["init", "--bare", "-b", "main", origin_dir_str]);
    run_git(&clone_dir, &["init", "-b", "main"]);
    run_git(&clone_dir, &["config", "commit.gpgsign", "false"]);

    let origin_remote_url = remote_url_override.unwrap_or(origin_dir_str);
    run_git(&clone_dir, &["remote", "add", "origin", origin_remote_url]);
    if let Some(url) = remote_url_override {
        run_git(
            &clone_dir,
            &["config", &format!("url.{origin_dir_str}.insteadOf"), url],
        );
    }

    std::fs::write(clone_dir.join("README.md"), "fixture\n").expect("write seed file");
    run_git(&clone_dir, &["add", "README.md"]);
    run_git(&clone_dir, &["commit", "-m", "initial"]);
    run_git(&clone_dir, &["push", "origin", "main"]);
    run_git(&clone_dir, &["remote", "set-head", "origin", "main"]);

    (origin_dir, clone_dir)
}

/// A synthetic git remote plus clone shaped like the real fork: a root
/// checkout on `main`, a `fork-only` branch that is a trivial ancestor of
/// `origin/main`, two genuinely merged feature branches (each with its own
/// worktree) and an unmerged one — `origin` is GitHub-shaped so
/// `slug_from_remote` derivation is genuinely exercised (fork issue #140
/// T1), not left pinning only the hardcoded fallback.
struct RepoFixture {
    // Held for the fixture's lifetime only to keep the tempdir alive — every
    // path derived from it is used via the fields below, not this handle
    // (docs/develop/red-confirmation.md: a `TempDir` dropped early is a
    // second, unrelated cause of RED hiding behind the intended one).
    _root: tempfile::TempDir,
    /// The bare `origin` repo's own filesystem path — deleted by the failed-
    /// `git fetch` test (fork issue #140 target 5) after the fixture's own
    /// initial fetch has already cached the remote-tracking refs it needs.
    origin_git_dir: PathBuf,
    /// A worktree checked out on the long-lived `fork-only` branch.
    fork_only_worktree: PathBuf,
    /// A worktree checked out on a genuinely merged feature branch.
    feat_merged_worktree: PathBuf,
    /// `feat/unmerged`'s current tip SHA — used to seed a genuine
    /// squash/rebase-merge match in `merged.tsv` (fork issue #140 reviewer
    /// F5 / T3).
    feat_unmerged_sha: String,
    /// `feat/merged`'s current tip SHA — a real, but for `feat/unmerged`
    /// STALE, sha used to pin the Renovate branch-reuse negative case.
    feat_merged_sha: String,
}

fn build_fixture() -> RepoFixture {
    let root = tempfile::tempdir().expect("tempdir for git fixture root");
    let (origin_git_dir, clone_dir) = init_repo(
        root.path(),
        Some(&format!("https://github.com/{FORK_SLUG}.git")),
    );

    let fork_only_worktree = root.path().join("wt-fork-only");
    let feat_merged_worktree = root.path().join("wt-feat-merged");
    let feat_merged_2_worktree = root.path().join("wt-feat-merged-2");
    let scratch_unmerged_worktree = root.path().join("wt-feat-unmerged-scratch");

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
    let feat_merged_sha = run_git(&clone_dir, &["rev-parse", "feat/merged"])
        .trim()
        .to_string();

    // `feat/merged-2`: a SECOND genuinely merged feature branch, on its own
    // worktree that stays alive for the fixture's lifetime — fork issue
    // #140 reviewer F6 / T4: with only `main`, `fork-only`, and the CURRENT
    // worktree in the fixture, every worktree/local-branch was excluded for
    // some OTHER reason, so the corresponding negative assertions passed
    // vacuously even if the offering logic broke outright. This gives both
    // `WORKTREES:` and `LOCAL_BRANCHES:` a genuine positive entry.
    let feat_merged_2_wt_str = feat_merged_2_worktree
        .to_str()
        .expect("feat/merged-2 worktree path is UTF-8");
    run_git(
        &clone_dir,
        &[
            "worktree",
            "add",
            "-b",
            "feat/merged-2",
            feat_merged_2_wt_str,
            "main",
        ],
    );
    std::fs::write(feat_merged_2_worktree.join("feature2.txt"), "merged2\n")
        .expect("write second feature file");
    run_git(&feat_merged_2_worktree, &["add", "feature2.txt"]);
    run_git(
        &feat_merged_2_worktree,
        &["commit", "-m", "feat: second merged work"],
    );
    run_git(&clone_dir, &["merge", "--ff-only", "feat/merged-2"]);
    run_git(&clone_dir, &["push", "origin", "main"]);
    run_git(&clone_dir, &["push", "origin", "feat/merged-2"]);

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
    let feat_unmerged_sha = run_git(&clone_dir, &["rev-parse", "feat/unmerged"])
        .trim()
        .to_string();
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
        origin_git_dir,
        fork_only_worktree,
        feat_merged_worktree,
        feat_unmerged_sha,
        feat_merged_sha,
    }
}

/// A fixture where the ROOT (administrative) checkout itself sits on a
/// merged feature branch, `feat/root-merged`, rather than `main` — the exact
/// condition target behaviour 1 (fork issue #140, the reviewer's headline
/// finding) must still exclude, by PATH identity, not by matching a branch
/// name.
struct RootOnMergedFixture {
    _root: tempfile::TempDir,
    /// A second worktree, on an unmerged scratch branch, that exists purely
    /// so the script can be invoked from somewhere OTHER than the root —
    /// running it from the root itself would trivially exclude the root as
    /// "the current worktree" and prove nothing about path identity
    /// specifically.
    runner_worktree: PathBuf,
}

fn build_fixture_root_on_merged_branch() -> RootOnMergedFixture {
    let root = tempfile::tempdir().expect("tempdir for git fixture root");
    let (_origin_git_dir, clone_dir) = init_repo(root.path(), None);

    run_git(&clone_dir, &["checkout", "-b", "feat/root-merged"]);
    std::fs::write(clone_dir.join("feature.txt"), "merged\n").expect("write feature file");
    run_git(&clone_dir, &["add", "feature.txt"]);
    run_git(&clone_dir, &["commit", "-m", "feat: merged work"]);
    run_git(&clone_dir, &["checkout", "main"]);
    run_git(&clone_dir, &["merge", "--ff-only", "feat/root-merged"]);
    run_git(&clone_dir, &["push", "origin", "main"]);
    run_git(&clone_dir, &["push", "origin", "feat/root-merged"]);

    // Move the root checkout itself onto the merged branch — the condition
    // under test.
    run_git(&clone_dir, &["checkout", "feat/root-merged"]);

    let runner_worktree = root.path().join("wt-runner");
    let runner_wt_str = runner_worktree
        .to_str()
        .expect("runner worktree path is UTF-8");
    run_git(
        &clone_dir,
        &[
            "worktree",
            "add",
            "-b",
            "scratch/runner",
            runner_wt_str,
            "main",
        ],
    );

    run_git(&clone_dir, &["fetch", "--prune", "--quiet", "origin"]);

    RootOnMergedFixture {
        _root: root,
        runner_worktree,
    }
}

/// A minimal repo (one commit on `main`, no extra branches or worktrees) for
/// the PR-state tests, which care about `slug_from_remote` and the `gh`
/// queries it feeds — not the worktree/branch offering logic already
/// covered by [`build_fixture`].
struct MinimalRepo {
    _root: tempfile::TempDir,
    dir: PathBuf,
}

/// `origin`'s remote URL is the bare repo's own local filesystem path —
/// multiple `/`s, so `is_owner_repo` rejects it outright. Pins target
/// behaviour 2's "no hardcoded slug fallback" path (fork issue #140
/// reviewer F3 / T1's second fixture variant).
fn build_fixture_unparseable_origin() -> MinimalRepo {
    let root = tempfile::tempdir().expect("tempdir for git fixture root");
    let (_origin_git_dir, clone_dir) = init_repo(root.path(), None);
    MinimalRepo {
        _root: root,
        dir: clone_dir,
    }
}

/// `origin` derives cleanly (GitHub-shaped + local `insteadOf`, same
/// technique as [`build_fixture`]), but `upstream` is a SECOND, present
/// remote whose URL does not parse as `owner/repo` — distinct from
/// `upstream` being simply unconfigured (target behaviour 3: a missing
/// `upstream` is not a degradation, but a present, unparseable one is).
fn build_fixture_unparseable_upstream() -> MinimalRepo {
    let root = tempfile::tempdir().expect("tempdir for git fixture root");
    let (_origin_git_dir, clone_dir) = init_repo(
        root.path(),
        Some(&format!("https://github.com/{FORK_SLUG}.git")),
    );
    run_git(
        &clone_dir,
        &["remote", "add", "upstream", "not-a-valid-owner-repo-url"],
    );
    MinimalRepo {
        _root: root,
        dir: clone_dir,
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
        std::fs::write(dir.path().join("invocations_counter"), "0")
            .expect("seed invocations counter");
        std::fs::create_dir_all(dir.path().join("invocations")).expect("create invocations dir");

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

    /// `merged.tsv` lines are "name<TAB>sha", matching exactly what real
    /// `gh`'s own `--jq '... | @tsv'` filter emits and what
    /// `merged_sha_matches` reads as its needle (fork issue #140 reviewer F5
    /// / T3): the squash/rebase path is only exercised through this, never
    /// through the ancestry test.
    fn set_merged(&self, entries: &[(&str, &str)]) {
        let body = entries
            .iter()
            .map(|(name, sha)| format!("{name}\t{sha}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.write_response("merged.tsv", &body);
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

    /// Every invocation's full `argv`, in call order — fork issue #140
    /// reviewer F4 / T2: the stub used to be identifiable only by WHICH
    /// canned response came back, which is exactly backwards for pinning a
    /// regression that points `--repo` at a wrong-but-still-valid-shaped
    /// slug. Assert against this instead of inferring from the response.
    fn invocations(&self) -> Vec<Vec<String>> {
        let inv_dir = self.dir.path().join("invocations");
        let mut entries: Vec<(u64, PathBuf)> = std::fs::read_dir(&inv_dir)
            .unwrap_or_else(|e| panic!("read gh stub invocations dir {inv_dir:?}: {e}"))
            .map(|e| e.expect("read gh stub invocation dir entry").path())
            .filter_map(|path| {
                let stem = path.file_stem()?.to_str()?.to_string();
                stem.parse::<u64>().ok().map(|n| (n, path))
            })
            .collect();
        entries.sort_by_key(|(n, _)| *n);
        entries
            .into_iter()
            .map(|(_, path)| {
                std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read gh stub invocation file {path:?}: {e}"))
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .collect()
    }
}

/// True if `argv` contains `flag` immediately followed by `value` — e.g.
/// `argv_has(&argv, "--repo", FORK_SLUG)`.
fn argv_has(argv: &[String], flag: &str, value: &str) -> bool {
    argv.windows(2).any(|w| w[0] == flag && w[1] == value)
}

/// Build the `/bin/sh` stub `gh` script. It logs every invocation's full
/// argv to its own `invocations/<n>.args` file (one arg per line, in call
/// order — see [`GhStub::invocations`]), then reads its own `--state` and
/// `--repo` arguments and `cat`s the matching pre-seeded response file,
/// rather than trying to reimplement `--json`/`--jq` — the response files
/// already hold exactly what `cleanup.sh`'s own `--jq` filters expect to
/// read.
///
/// `--repo` is matched by exact literal value (baked in at generation time,
/// never read from a shell variable) so the SAME generated script tells
/// "queried the fork" apart from "queried upstream" apart from "queried
/// neither" (D3). An unrecognised `--repo` value exits non-zero with a
/// message on stderr, faithfully mirroring real `gh pr list --repo
/// owner/does-not-exist` — fork issue #140 reviewer F4 / T2: the previous
/// `*) ;;` arm silently fell through to `exit 0` with an empty response,
/// which is exactly what a regression pointing `--repo` at a
/// wrong-but-valid-shaped slug would ALSO produce, making that regression
/// indistinguishable from a clean, empty result.
fn gh_stub_script(dir: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         set -eu\n\
         invdir={invdir}\n\
         counter={counter}\n\
         count=$(cat \"$counter\" 2>/dev/null || echo 0)\n\
         count=$((count + 1))\n\
         printf '%s' \"$count\" > \"$counter\"\n\
         out=\"$invdir/${{count}}.args\"\n\
         : > \"$out\"\n\
         state=\"\"\n\
         repo=\"\"\n\
         prev=\"\"\n\
         for arg in \"$@\"; do\n\
         \x20\x20printf '%s\\n' \"$arg\" >> \"$out\"\n\
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
         case \"$repo\" in\n\
         \x20\x20{fork_slug})\n\
         \x20\x20\x20\x20case \"$state\" in\n\
         \x20\x20\x20\x20\x20\x20open) cat {open_fork_path} ;;\n\
         \x20\x20\x20\x20\x20\x20merged) cat {merged_path} ;;\n\
         \x20\x20\x20\x20esac\n\
         \x20\x20\x20\x20;;\n\
         \x20\x20{upstream_slug})\n\
         \x20\x20\x20\x20case \"$state\" in\n\
         \x20\x20\x20\x20\x20\x20open) cat {open_upstream_path} ;;\n\
         \x20\x20\x20\x20esac\n\
         \x20\x20\x20\x20;;\n\
         \x20\x20\"\")\n\
         \x20\x20\x20\x20case \"$state\" in\n\
         \x20\x20\x20\x20\x20\x20open) cat {open_norepo_path} ;;\n\
         \x20\x20\x20\x20esac\n\
         \x20\x20\x20\x20;;\n\
         \x20\x20*) echo \"unknown repo: $repo\" >&2; exit 1 ;;\n\
         esac\n\
         exit 0\n",
        invdir = sh_quote_path(&dir.join("invocations")),
        counter = sh_quote_path(&dir.join("invocations_counter")),
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

    /// Branch names offered in `WORKTREES:` (the `…<TAB>branch` suffix of
    /// each entry — fork issue #140 target 6: `|` is a legal refname
    /// character, so a TAB is the unambiguous separator the destructive
    /// consumer needs).
    fn worktree_branches(&self) -> Vec<String> {
        self.section("WORKTREES:")
            .into_iter()
            .filter_map(|line| line.rsplit('\t').next().map(str::to_string))
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
        // Fork issue #140 reviewer F12 / T5: `run_git` above already scrubs
        // system config; the SCRIPT UNDER TEST needs the same isolation from
        // BOTH system and global config, or a developer's or runner's
        // `url.*.insteadOf` / `fetch.prune` / `checkout.defaultRemote` could
        // make results differ between machines. (Interacts with `init_repo`
        // above: its `insteadOf` mapping is written to the REPO-LOCAL
        // config, never `--global`, so it survives `GIT_CONFIG_GLOBAL`
        // being pointed at `/dev/null` here.)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

/// Scenario: build the synthetic repo (now with a SECOND merged feature
/// worktree, `feat/merged-2`, alongside the current one), run `cleanup.sh`
/// from the `feat/merged` worktree, and confirm `WORKTREES:` is EXACTLY
/// `{feat/merged-2}` — not just "doesn't contain `main` or `fork-only`".
/// Fork issue #140 reviewer F6: with only `main`, `fork-only`, and the
/// CURRENT worktree in the fixture, every worktree was excluded for some
/// OTHER reason, so the old two negative assertions passed even if the
/// worktree loop broke outright and offered nothing at all.
#[test]
fn release_cleanup_001_worktrees_exclude_root_checkout_and_fork_only() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    let mut branches = out.worktree_branches();
    branches.sort();
    assert_eq!(
        branches,
        vec!["feat/merged-2".to_string()],
        "WORKTREES: expected EXACTLY the other genuinely merged feature \
         worktree (`feat/merged-2`) — never the root checkout (branch \
         `main`, fork issue #140 D1), never `fork-only` (D2), and never the \
         CURRENT worktree (`feat/merged`) itself. Full stdout:\n{}\nstderr:\n{}",
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
/// `LOCAL_BRANCHES:` positively offers the OTHER merged feature branch
/// (`feat/merged-2`, non-current) while still excluding `fork-only` from
/// both `LOCAL_BRANCHES:` and `REMOTE_BRANCHES:`. Fork issue #140 reviewer
/// F6: previously `feat/merged` itself was the only merged local branch in
/// the fixture, and it is excluded here as the CURRENT branch, so the old
/// local assertion passed with an empty `LOCAL_BRANCHES:` list and proved
/// nothing.
#[test]
fn release_cleanup_003_local_and_remote_branches_exclude_fork_only() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        out.local_branches().contains(&"feat/merged-2".to_string()),
        "LOCAL_BRANCHES: the OTHER genuinely merged feature branch \
         (`feat/merged-2`, not the current `feat/merged`) must be offered — \
         otherwise this section being empty makes the negative assertions \
         below vacuous (fork issue #140 reviewer F6). Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
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
/// repository, not left unpinned (D3) — AND that the query which produced
/// that exclusion genuinely carried `--repo {FORK_SLUG}`, rather than the
/// assertion merely happening to pass because the stub's repo-less fallback
/// response was reached (fork issue #140 reviewer F3/F4 / T1/T2). Today's
/// script never passes `--repo` at all, so this is expected to fail as "not
/// implemented yet": the stub's `open_fork.tsv` response is unreachable
/// without the pin.
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

    let invocations = gh.invocations();
    assert!(
        invocations
            .iter()
            .any(|argv| argv_has(argv, "--state", "open") && argv_has(argv, "--repo", FORK_SLUG)),
        "expected an invocation of `gh pr list --state open --repo {FORK_SLUG} \
         ...` — i.e. that the open-PR-on-the-fork query genuinely ran with \
         the DERIVED slug, not that the assertion above merely happened to \
         pass via the stub's repo-less fallback response (fork issue #140 \
         reviewer F3/F4). Recorded invocations:\n{invocations:#?}"
    );
}

/// Scenario: a stub `gh` reports an open PR on UPSTREAM
/// (`--repo vfarcic/dot-agent-deck`) whose head name matches local branch
/// `feat/merged`. Confirms it is still excluded from `LOCAL_BRANCHES:` — the
/// deliberate, explicit version of the protection D3 says today's script
/// gets only by accident — AND that the query carried `--repo
/// {UPSTREAM_SLUG}` specifically (fork issue #140 reviewer F3/F4 / T1/T2).
/// Expected to fail as "not implemented yet": this pins the SECOND upstream
/// query the coder needs to add, which does not exist in the script today.
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

    let invocations = gh.invocations();
    assert!(
        invocations.iter().any(
            |argv| argv_has(argv, "--state", "open") && argv_has(argv, "--repo", UPSTREAM_SLUG)
        ),
        "expected an invocation of `gh pr list --state open --repo \
         {UPSTREAM_SLUG} ...` — i.e. that the open-PR-on-upstream query \
         genuinely ran with the DERIVED upstream slug (fork issue #140 \
         reviewer F3/F4). Recorded invocations:\n{invocations:#?}"
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

/// Scenario: the fixture's ROOT checkout itself sits on `feat/root-merged`
/// (a merged, non-`main` branch) rather than `main`. Run `cleanup.sh` from a
/// SEPARATE worktree (so the root is not merely "the current worktree") and
/// confirm the root is still never offered in `WORKTREES:` — target
/// behaviour 1 (fork issue #140, the reviewer's headline finding): today's
/// D1 fix excludes the root only by matching branch name `main`/`fork-only`,
/// so it is offered whenever the root happens to be on any OTHER branch,
/// which is the routine post-merge state CLAUDE.md rule 1 describes.
#[test]
fn release_cleanup_007_root_checkout_excluded_even_on_merged_feature_branch() {
    let fixture = build_fixture_root_on_merged_branch();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.runner_worktree, gh.path());

    assert_eq!(
        out.worktree_branches(),
        Vec::<String>::new(),
        "WORKTREES: this fixture has exactly two branches — the root \
         checkout on merged `feat/root-merged`, and the unmerged \
         `scratch/runner` this script was run from — so the fully correct \
         output offers NEITHER. A non-empty result means the root checkout \
         leaked in because it is excluded only by branch NAME, not by being \
         the root's own path (fork issue #140 target behaviour 1). Full \
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: `origin`'s remote URL is the bare repo's own local filesystem
/// path, which does not parse as `owner/repo`. Confirm the run reports
/// `PR_STATE_DEGRADED=true` and never invokes `gh` at all — target behaviour
/// 2 (fork issue #140): no hardcoded fallback, a genuinely undetermined slug
/// degrades and skips the query instead of guessing.
#[test]
fn release_cleanup_008_unparseable_origin_degrades_without_hardcoded_fallback() {
    let fixture = build_fixture_unparseable_origin();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.dir, gh.path());

    assert!(
        out.degraded(),
        "an `origin` remote whose URL cannot be parsed as `owner/repo` must \
         produce `PR_STATE_DEGRADED=true` (fork issue #140 target behaviour \
         2). Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );

    let invocations = gh.invocations();
    assert!(
        invocations.is_empty(),
        "with `origin`'s slug undetermined (and no `upstream` remote \
         configured either), `gh` must not be invoked at all — today's \
         script instead substitutes the hardcoded fallback `{FORK_SLUG}` and \
         queries it anyway, which is indistinguishable from a genuine fork \
         repo and hides the fact that derivation never worked. Recorded \
         invocations:\n{invocations:#?}"
    );
}

/// Scenario: the default fixture never configures an `upstream` remote at
/// all. Confirm a clean run (no other failure injected) reports NO
/// `PR_STATE_DEGRADED=true` — target behaviour 3 (fork issue #140): a
/// missing `upstream` is legitimately optional, not a degradation.
#[test]
fn release_cleanup_009_missing_upstream_remote_does_not_degrade() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        !out.degraded(),
        "this fixture has no `upstream` remote configured at all, which is \
         a legitimate, routine state (not every checkout carries one) — it \
         must not set `PR_STATE_DEGRADED=true` (fork issue #140 target \
         behaviour 3). Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: `origin` derives cleanly, but `upstream` is a SECOND, present
/// remote whose URL does not parse as `owner/repo`. Confirm this DOES
/// degrade — distinguishing "missing" (test 009, not a degradation) from
/// "present but unparseable" (this test, a genuine degradation) — target
/// behaviour 3 (fork issue #140).
#[test]
fn release_cleanup_010_present_but_unparseable_upstream_degrades() {
    let fixture = build_fixture_unparseable_upstream();
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.dir, gh.path());

    assert!(
        out.degraded(),
        "an `upstream` remote that EXISTS but whose URL cannot be parsed as \
         `owner/repo` must produce `PR_STATE_DEGRADED=true` — distinct from \
         a MISSING `upstream`, which must not (fork issue #140 target \
         behaviour 3). Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: the stub `gh` returns exactly 200 merged-PR rows — a full page
/// for `--limit 200`. Confirm this sets `PR_STATE_DEGRADED=true`, since a
/// full page can never be distinguished from a page that was silently
/// truncated — target behaviour 4 (fork issue #140).
#[test]
fn release_cleanup_011_full_page_merged_query_degrades() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    let full_page: Vec<(String, String)> = (0..200)
        .map(|i| (format!("synthetic/{i}"), format!("{i:040x}")))
        .collect();
    let full_page_refs: Vec<(&str, &str)> = full_page
        .iter()
        .map(|(name, sha)| (name.as_str(), sha.as_str()))
        .collect();
    gh.set_merged(&full_page_refs);
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        out.degraded(),
        "a `gh pr list --state merged --limit 200 ...` response with exactly \
         200 rows — a full page — must produce `PR_STATE_DEGRADED=true`, \
         since later merged PRs on a truncated page would silently leave \
         their branches unoffered (fork issue #140 target behaviour 4). Full \
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: the fixture's `origin` bare repo is deleted from disk AFTER
/// setup (so cached remote-tracking refs from the fixture's OWN initial
/// fetch remain usable), so `cleanup.sh`'s own `git fetch --prune` fails at
/// runtime while slug derivation (which only reads git CONFIG, not the
/// network) is unaffected. Confirm this sets `PR_STATE_DEGRADED=true` —
/// target behaviour 5 (fork issue #140).
#[test]
fn release_cleanup_012_failed_git_fetch_degrades() {
    let fixture = build_fixture();
    std::fs::remove_dir_all(&fixture.origin_git_dir).unwrap_or_else(|e| {
        panic!(
            "remove origin bare repo {:?} to force a fetch failure: {e}",
            fixture.origin_git_dir
        )
    });
    let gh = GhStub::new();
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        out.degraded(),
        "cleanup.sh's own `git fetch --prune --quiet origin` failing (the \
         origin repo no longer exists on disk) must produce \
         `PR_STATE_DEGRADED=true` instead of being silently swallowed by \
         `|| true` (fork issue #140 target behaviour 5). Full \
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: `merged.tsv` carries an exact `name<TAB>sha` line for
/// `feat/unmerged` using ITS REAL current tip SHA, even though that branch
/// genuinely diverges from `main` (fails the ancestry test). Confirm it
/// becomes offered anyway — the squash/rebase detection path (fork issue
/// #140 reviewer F5): `merged_shas_lines` was previously only ever empty, so
/// this path had zero coverage.
#[test]
fn release_cleanup_013_squash_merged_branch_offered_via_exact_sha_match() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    gh.set_merged(&[("feat/unmerged", fixture.feat_unmerged_sha.as_str())]);
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        out.local_branches().contains(&"feat/unmerged".to_string()),
        "LOCAL_BRANCHES: `feat/unmerged` diverges from `main` (fails the \
         ancestry test) but its EXACT current tip SHA appears in the merged \
         PRs list — a squash/rebase merge — so it must still be offered \
         (fork issue #140 reviewer F5). Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.remote_branches().contains(&"feat/unmerged".to_string()),
        "REMOTE_BRANCHES: same squash/rebase match, for \
         `origin/feat/unmerged`. Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

/// Scenario: `merged.tsv` carries a STALE `name<TAB>sha` line for
/// `feat/unmerged` — a real SHA, but not this branch's current tip (the
/// Renovate branch-reuse case: an old PR's merged SHA lingers after the
/// branch was recreated or advanced). Confirm it stays unoffered.
#[test]
fn release_cleanup_014_stale_merged_sha_does_not_offer_recreated_branch() {
    let fixture = build_fixture();
    let gh = GhStub::new();
    gh.set_merged(&[("feat/unmerged", fixture.feat_merged_sha.as_str())]);
    let out = run_cleanup(&fixture.feat_merged_worktree, gh.path());

    assert!(
        !out.local_branches().contains(&"feat/unmerged".to_string()),
        "LOCAL_BRANCHES: the merged-PRs list carries a STALE sha for \
         `feat/unmerged` (a real sha, just not this branch's current tip) — \
         the Renovate branch-reuse case the script's own comment describes. \
         `feat/unmerged` must stay unoffered. Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.remote_branches().contains(&"feat/unmerged".to_string()),
        "REMOTE_BRANCHES: same stale-sha check, for `origin/feat/unmerged`. \
         Full stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}
