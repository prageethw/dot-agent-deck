//! Repository-state preflight (checks 10-12, issue #325).
//!
//! `worktree_reclaim.rs`'s ownership gate only ever sees a `git worktree`
//! mutation that goes through the deck's own reclaim path — a raw `git
//! worktree remove` or `git fetch --depth=1` run from any agent's shell
//! bypasses it entirely, and neither failure leaves the ownership model
//! anything to check. Both are **bypass**, not **breakage**, so hardening
//! `worktree_reclaim.rs` buys nothing here (issue #325's diagnostic
//! comments). This module sits where the damage actually lands instead: the
//! shared object store and worktree registry any concurrent `git` command
//! can corrupt, checked at the one place every worktree already runs before
//! every commit (CLAUDE.md rule 2).
//!
//! # What it asserts, all as hard failures — never warnings, never auto-repair
//!
//! 10. The repository is not unexpectedly shallow
//!     (`git rev-parse --is-shallow-repository`). The remedy is named
//!     verbatim in the failure message (`git fetch --unshallow <remote>`) —
//!     silently fixing it would hide that something on this machine is
//!     doing destructive things to a shared clone, and that signal is the
//!     point.
//! 11. The worktree registry has no drift: every `git worktree list` entry
//!     (other than the current one, which check 12 owns) must still exist
//!     on disk. A stale entry means something removed a worktree without
//!     pruning it.
//! 12. The current worktree itself still exists — the degenerate case of
//!     11, given its own message so a worker hits a clear diagnosis rather
//!     than whatever confusing `no such file or directory` runs next.
//!
//! # Why this must not fire in CI (and must not use an env-var escape hatch)
//!
//! Every `actions/checkout` in this repo's CI defaults to depth 1 except
//! `sonarqube`, so `if shallow { fail }` would fail nearly every job. An
//! `if CI { skip }` escape hatch is exactly the kind of check that quietly
//! stops running wherever it matters (CLAUDE.md's running theme on empty
//! gates). [`is_gated`] instead keys off a structural property: a CI runner
//! clones fresh and has exactly one, non-linked worktree, so it is exempt by
//! construction rather than by trusting an environment variable. The damage
//! this issue is about only exists once several worktrees share one object
//! store.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether the repository-state preflight applies at all: more than one
/// registered worktree, or the current checkout is itself a linked
/// worktree. A CI runner's fresh single-worktree clone satisfies neither and
/// is exempt by construction — see the module doc.
pub fn is_gated(worktree_count: usize, is_linked_worktree: bool) -> bool {
    worktree_count > 1 || is_linked_worktree
}

/// Parse `git worktree list --porcelain` output into the registered
/// absolute worktree paths, in the order git reported them. Each entry
/// starts with a `worktree <path>` line; the following `HEAD`/`branch`/
/// `bare`/`detached`/blank lines are not needed here.
pub fn parse_worktree_list_porcelain(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect()
}

/// Every registered worktree path, other than `current`, that no longer
/// exists on disk — check 11's core logic, pure so it can be unit-tested
/// without a real git repo. `current` is excluded here because check 12
/// gives it its own, clearer message rather than folding it into "some
/// other worktree is stale."
pub fn stale_worktree_paths(worktrees: &[PathBuf], current: &Path) -> Vec<PathBuf> {
    worktrees
        .iter()
        .filter(|p| p.as_path() != current)
        .filter(|p| !p.exists())
        .cloned()
        .collect()
}

/// Check 12's core logic: does the current worktree's own directory still
/// exist? Pure so a nonexistent synthetic path can be fed directly in a
/// test, rather than needing to reproduce the underlying race live.
pub fn current_worktree_missing(current: &Path) -> bool {
    !current.exists()
}

/// Run `git <args>` in `repo_dir`, returning trimmed stdout on success or a
/// message built from stderr (or the spawn error) on failure.
fn run_git_capture(repo_dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git rev-parse --is-shallow-repository` in `repo_dir`.
fn is_shallow_repository(repo_dir: &Path) -> Result<bool, String> {
    Ok(run_git_capture(repo_dir, &["rev-parse", "--is-shallow-repository"])?.trim() == "true")
}

/// Whether `repo_dir` is a linked worktree: `git rev-parse
/// --git-common-dir`, run with `repo_dir` as the working directory, prints
/// exactly `.git` for the main checkout and an absolute path elsewhere for a
/// linked worktree (its `.git` is a file pointing at the shared admin
/// directory, not a directory of its own).
fn is_linked_worktree(repo_dir: &Path) -> Result<bool, String> {
    Ok(run_git_capture(repo_dir, &["rev-parse", "--git-common-dir"])?.trim() != ".git")
}

/// `git worktree list --porcelain` in `repo_dir`, parsed into paths.
fn worktree_paths(repo_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let out = run_git_capture(repo_dir, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list_porcelain(&out))
}

/// The remote to name in check 10's remedy message: the first line of `git
/// remote`, or `origin` if the command fails or lists nothing (a repository
/// with no remotes at all cannot be genuinely shallow via a normal clone,
/// but `origin` is still the least surprising fallback name to print).
fn default_remote(repo_dir: &Path) -> String {
    run_git_capture(repo_dir, &["remote"])
        .ok()
        .and_then(|out| out.lines().next().map(str::to_string))
        .unwrap_or_else(|| "origin".to_string())
}

/// Checks 10-12: the repository-state preflight, returning one formatted
/// failure string per violation (empty when everything is clean or the
/// preflight is exempt per [`is_gated`]). `repo_dir` is the workspace root
/// as [`crate::repo_root`] resolves it — the same directory every other
/// check in this binary already operates against.
pub fn check(repo_dir: &Path) -> Vec<String> {
    let mut failures = Vec::new();

    let worktrees = match worktree_paths(repo_dir) {
        Ok(w) => w,
        Err(e) => {
            failures.push(format!(
                "[10] could not list worktrees via `git worktree list --porcelain` in {}: {e}",
                repo_dir.display()
            ));
            return failures;
        }
    };
    let linked = match is_linked_worktree(repo_dir) {
        Ok(b) => b,
        Err(e) => {
            failures.push(format!(
                "[10] could not determine whether {} is a linked worktree via `git rev-parse \
                 --git-common-dir`: {e}",
                repo_dir.display()
            ));
            return failures;
        }
    };

    if !is_gated(worktrees.len(), linked) {
        // A single, non-linked worktree — a fresh CI clone, or a lone
        // developer checkout — is exempt by construction. See the module
        // doc for why this is a structural test rather than an `if CI`
        // escape hatch.
        return failures;
    }

    // Check 10: not unexpectedly shallow.
    match is_shallow_repository(repo_dir) {
        Ok(true) => {
            let remote = default_remote(repo_dir);
            failures.push(format!(
                "[10] repository at {} is unexpectedly shallow while sharing an object store \
                 with other worktrees (`git rev-parse --is-shallow-repository` = true) — remedy: \
                 `git fetch --unshallow {remote}`",
                repo_dir.display()
            ));
        }
        Ok(false) => {}
        Err(e) => failures.push(format!(
            "[10] could not determine shallow-ness of {} via `git rev-parse \
             --is-shallow-repository`: {e}",
            repo_dir.display()
        )),
    }

    // Check 11: worktree registry drift (excluding the current worktree,
    // which check 12 covers).
    let stale = stale_worktree_paths(&worktrees, repo_dir);
    if !stale.is_empty() {
        let paths = stale
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        failures.push(format!(
            "[11] `git worktree list` carries {} stale entr{} no longer on disk ({paths}) — \
             something removed a worktree without pruning; remedy: `git worktree prune`",
            stale.len(),
            if stale.len() == 1 { "y" } else { "ies" },
        ));
    }

    // Check 12: the current worktree itself — the degenerate case of 11.
    if current_worktree_missing(repo_dir) {
        failures.push(format!(
            "[12] this worktree's own directory ({}) no longer exists on disk — something \
             removed it out from under the current process; remedy: `git worktree prune` from a \
             working directory that still exists",
            repo_dir.display()
        ));
    }

    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_gated_exempts_a_single_non_linked_worktree() {
        assert!(!is_gated(1, false));
    }

    #[test]
    fn is_gated_fires_on_multiple_worktrees() {
        assert!(is_gated(2, false));
        assert!(is_gated(5, false));
    }

    #[test]
    fn is_gated_fires_when_the_current_checkout_is_linked() {
        // A linked worktree's own `git worktree list` may report just
        // itself plus the main checkout (2), but even a hypothetical
        // porcelain output reporting only 1 entry must still gate, because
        // `is_linked_worktree` is independently true.
        assert!(is_gated(1, true));
    }

    #[test]
    fn is_gated_is_false_only_when_both_conditions_are_false() {
        assert!(!is_gated(0, false));
    }

    #[test]
    fn parse_worktree_list_porcelain_extracts_every_path() {
        let out = "worktree /repo\nHEAD abc123\nbranch refs/heads/main\n\n\
             worktree /repo-fix160\nHEAD def456\nbranch refs/heads/fix/160\n\n\
             worktree /repo-detached\nHEAD 789abc\ndetached\n";
        assert_eq!(
            parse_worktree_list_porcelain(out),
            vec![
                PathBuf::from("/repo"),
                PathBuf::from("/repo-fix160"),
                PathBuf::from("/repo-detached"),
            ]
        );
    }

    #[test]
    fn parse_worktree_list_porcelain_handles_a_single_entry() {
        let out = "worktree /repo\nHEAD abc123\nbranch refs/heads/main\n";
        assert_eq!(
            parse_worktree_list_porcelain(out),
            vec![PathBuf::from("/repo")]
        );
    }

    #[test]
    fn parse_worktree_list_porcelain_ignores_non_worktree_lines() {
        // Lines not prefixed with "worktree " (HEAD, branch, bare,
        // detached, blank separators) must never be mistaken for a path.
        let out = "bare\nworktree /only-real-entry\nHEAD abc123\ndetached\n";
        assert_eq!(
            parse_worktree_list_porcelain(out),
            vec![PathBuf::from("/only-real-entry")]
        );
    }

    #[test]
    fn stale_worktree_paths_reports_missing_entries_excluding_current() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let current = tmp.path().join("current");
        std::fs::create_dir_all(&current).expect("mkdir current");
        let alive = tmp.path().join("alive");
        std::fs::create_dir_all(&alive).expect("mkdir alive");
        let gone = tmp.path().join("gone-worktree");
        // Deliberately never created on disk — stands in for "removed
        // without pruning".

        let worktrees = vec![current.clone(), alive.clone(), gone.clone()];
        let stale = stale_worktree_paths(&worktrees, &current);

        assert_eq!(stale, vec![gone]);
    }

    #[test]
    fn stale_worktree_paths_is_empty_when_every_entry_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let current = tmp.path().join("current");
        std::fs::create_dir_all(&current).expect("mkdir current");
        let alive = tmp.path().join("alive");
        std::fs::create_dir_all(&alive).expect("mkdir alive");

        let worktrees = vec![current.clone(), alive];
        assert!(stale_worktree_paths(&worktrees, &current).is_empty());
    }

    #[test]
    fn stale_worktree_paths_never_reports_the_current_worktree_itself() {
        // Even if the current worktree's own path is somehow missing on
        // disk, check 11 must not double-report it — check 12 owns that
        // message.
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing_current = tmp.path().join("missing-current");
        let worktrees = vec![missing_current.clone()];
        assert!(stale_worktree_paths(&worktrees, &missing_current).is_empty());
    }

    #[test]
    fn current_worktree_missing_is_false_for_an_existing_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!current_worktree_missing(tmp.path()));
    }

    #[test]
    fn current_worktree_missing_is_true_for_a_nonexistent_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gone = tmp.path().join("never-created");
        assert!(current_worktree_missing(&gone));
    }
}
