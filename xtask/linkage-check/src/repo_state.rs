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
//!     Decided FIRST, from [`current_worktree_missing`] on the already-
//!     resolved `repo_dir` path rather than by spawning git with
//!     `current_dir(repo_dir)` — that spawn's own `chdir` file action fails
//!     first when the directory is gone, which used to make [`check`]
//!     return empty before this check was ever consulted (issue #325's
//!     original bug). The more common real path into this same condition is
//!     one level up, in `main.rs`'s `repo_root()`: its own
//!     `std::env::current_dir()` call fails the same way when the process's
//!     cwd was removed out from under it before the binary even started
//!     (the shell-`cd`-then-someone-else-removes-it shape), and `main()`
//!     now turns that into this same check-12 diagnosis instead of a panic.
//!
//! # Why this must not fire in CI (and must not use an env-var escape hatch)
//!
//! [`is_gated`] keys off a structural property rather than an environment
//! variable: a CI runner clones fresh and has exactly one, non-linked
//! worktree, so it is exempt by construction — an `if CI { skip }` escape
//! hatch is exactly the kind of check that quietly stops running wherever it
//! matters (CLAUDE.md's running theme on empty gates), and this avoids one
//! entirely. On this fork the `build` job (the only job that runs `cargo
//! xtask linkage-check`) also happens to check out with `fetch-depth: 0`, so
//! it would not be shallow either way; upstream's jobs default to depth 1,
//! which is the scenario the structural gate is actually built to survive.
//! The damage this issue is about only exists once several worktrees share
//! one object store.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether the repository-state preflight applies at all: more than one
/// registered worktree, or the current checkout is itself a linked
/// worktree. A CI runner's fresh single-worktree clone satisfies neither and
/// is exempt by construction — see the module doc.
pub fn is_gated(worktree_count: usize, is_linked_worktree: bool) -> bool {
    worktree_count > 1 || is_linked_worktree
}

/// Parse `git worktree list --porcelain -z` output (NUL-separated records)
/// into the registered absolute worktree paths, in the order git reported
/// them. Each entry is a run of NUL-terminated `key<space>value` fields
/// (`worktree <path>`, `HEAD <sha>`, `branch <ref>`, `bare`, `detached`),
/// with an extra empty field ending each entry (in place of the blank line
/// the non-`-z` form uses as a separator).
///
/// `-z` (not the line-oriented porcelain form) is load-bearing: git does not
/// quote paths in porcelain output at all, so a worktree path containing a
/// raw newline is split across two porcelain *lines* and silently
/// mis-parsed — verified: `git worktree add "$(printf '../wt\nline')"`
/// prints the newline byte raw, and the non-`-z` parser used to read that as
/// two separate porcelain lines. NUL-separated fields have no such
/// ambiguity, since NUL cannot appear in a path.
pub fn parse_worktree_list_porcelain_z(output: &[u8]) -> Vec<PathBuf> {
    output
        .split(|&b| b == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .map(path_from_bytes)
        .collect()
}

/// Build a [`PathBuf`] from raw git output bytes without assuming UTF-8.
/// `String::from_utf8_lossy` would substitute `U+FFFD` for genuinely
/// non-UTF-8 path bytes, producing a `PathBuf` that can never match the real
/// on-disk path — turning check 11 permanently, unfixably red for a path
/// `git worktree add` accepted. On Unix, `OsStrExt::from_bytes` is an exact,
/// lossless round trip since Unix paths are just bytes. Windows paths are
/// UTF-16 at the OS level with no equivalent raw-byte constructor, and git's
/// porcelain output there is UTF-8 in practice, so the lossy path is kept
/// there — it only degrades the already-pathological non-UTF-8-bytes case,
/// not the newline case this function primarily exists to fix (that is
/// fixed on every platform by the `-z` split above).
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
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

/// Outcome of probing whether `repo_dir` is inside a git working tree at all
/// (`git rev-parse --is-inside-work-tree`). Three-way rather than a bare
/// `bool` so [`check`] can tell "genuinely not a git repo" (exempt, no
/// output — a synthetic fixture tree with no `.git`, e.g.
/// `tests/duplicate_catalog_id.rs`'s own fixture, which checks 1-9 exercise
/// directly against a bare directory) apart from "git could not be run, or
/// failed for some other reason" (git missing from `PATH`, a
/// `safe.directory` refusal, a permission error, a corrupt admin dir) — the
/// latter used to collapse into the same silent "exempt" outcome as the
/// former, which let an unrelated git failure disable checks 10-12 with zero
/// output while `main.rs` still printed a clean success line.
#[derive(Debug)]
enum WorkTreeProbe {
    Inside,
    NotAGitRepo,
    ProbeFailed(String),
}

fn probe_work_tree(repo_dir: &Path) -> WorkTreeProbe {
    match run_git_capture(repo_dir, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(s) if s == "true" => WorkTreeProbe::Inside,
        Ok(_) => WorkTreeProbe::NotAGitRepo,
        Err(e) if e.contains("not a git repository") => WorkTreeProbe::NotAGitRepo,
        Err(e) => WorkTreeProbe::ProbeFailed(e),
    }
}

/// Build `git <args>` to run in `repo_dir`, with `LC_ALL`/`LANG` pinned to
/// `C` so git's stderr/stdout comes back in the untranslated form
/// [`probe_work_tree`]'s `"not a git repository"` substring match (and any
/// future caller parsing git's text output) expects. Without this, a
/// non-English-locale machine or CI runner gets git's *localized* message
/// instead (verified: `LC_ALL=de_DE.UTF-8` → `Schwerwiegend: Kein
/// Git-Repository (…)`), the match silently fails, and a legitimate
/// non-git fixture takes the `ProbeFailed` error arm instead of the correct
/// exemption (S7/N1). `LC_ALL` alone is authoritative for glibc/git's
/// gettext lookup, but `LANG` is pinned too as the common idiom, in case
/// some environment's git build consults it directly.
fn git_command(repo_dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(repo_dir)
        .env("LC_ALL", "C")
        .env("LANG", "C");
    cmd
}

/// Run `git <args>` in `repo_dir`, returning trimmed stdout on success or a
/// message built from stderr (or the spawn error) on failure.
fn run_git_capture(repo_dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_command(repo_dir, args)
        .output()
        .map_err(|e| format!("invoke git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run `git <args>` in `repo_dir`, returning raw stdout bytes on success —
/// used only for `-z`-separated porcelain output, where the bytes must not
/// be assumed UTF-8 before `[path_from_bytes]` gets to them.
fn run_git_capture_bytes(repo_dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = git_command(repo_dir, args)
        .output()
        .map_err(|e| format!("invoke git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
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

/// `git worktree list --porcelain -z` in `repo_dir`, parsed into paths.
fn worktree_paths(repo_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let out = run_git_capture_bytes(repo_dir, &["worktree", "list", "--porcelain", "-z"])?;
    Ok(parse_worktree_list_porcelain_z(&out))
}

/// The remedy text for check 10: `git fetch --unshallow <remote>` for every
/// configured remote, not just the first alphabetically. `git rev-parse
/// --is-shallow-repository` is a repository-wide flag with no cheap way to
/// attribute it to one particular remote, and naming only the
/// alphabetically-first one (`origin` on this repo) can name the wrong
/// remote outright — issue #325's own incident was `upstream/main` shallowed
/// to depth 1, which `origin` would never have named. Naming every remote
/// means a worker who runs the given command(s) verbatim actually deepens
/// whichever remote was truncated, rather than guessing.
fn shallow_remedy(repo_dir: &Path) -> String {
    let remotes: Vec<String> = run_git_capture(repo_dir, &["remote"])
        .map(|out| out.lines().map(str::to_string).collect())
        .unwrap_or_default();
    if remotes.is_empty() {
        // A repository with no remotes at all cannot be genuinely shallow
        // via a normal clone, but `origin` is still the least surprising
        // fallback name to print.
        "`git fetch --unshallow origin`".to_string()
    } else {
        remotes
            .iter()
            .map(|r| format!("`git fetch --unshallow {r}`"))
            .collect::<Vec<_>>()
            .join(" and ")
    }
}

/// Result of [`check`]: the formatted failure strings (empty when clean or
/// exempt), plus whether checks 10-12 actually ran at all this time.
pub struct RepoStateResult {
    pub failures: Vec<String>,
    /// `None` when checks 10-12 fully executed. `Some(reason)` names why
    /// they were structurally exempt this run — read only by `main.rs`'s
    /// success line (issue #325 B2) so it can report the rule count that
    /// actually ran instead of a fixed `12 rules` on every run where three
    /// of them were skipped, which is every CI run and every single-
    /// worktree developer checkout (`prds/fork-340-work-type-vocabulary.md`
    /// names this exact success-line-overclaims shape as the empty-gate
    /// pattern this repo keeps documenting).
    pub skip_reason: Option<&'static str>,
}

/// Checks 10-12: the repository-state preflight. `repo_dir` is the
/// workspace root as [`crate::repo_root`] resolves it — the same directory
/// every other check in this binary already operates against.
pub fn check(repo_dir: &Path) -> RepoStateResult {
    let mut failures = Vec::new();

    // Check 12, decided FIRST and from a plain filesystem stat on the
    // already-resolved `repo_dir` — never by spawning git with
    // `current_dir(repo_dir)`, which fails at the `chdir` file action before
    // git ever runs once the directory is gone. See the module doc for why
    // this ordering is what makes the check reachable at all.
    if current_worktree_missing(repo_dir) {
        failures.push(format!(
            "[12] this worktree's own directory ({}) no longer exists on disk — something \
             removed it out from under the current process; remedy: `git worktree prune` from a \
             working directory that still exists",
            repo_dir.display()
        ));
        return RepoStateResult {
            failures,
            skip_reason: None,
        };
    }

    match probe_work_tree(repo_dir) {
        WorkTreeProbe::NotAGitRepo => {
            // Not a git checkout at all — exempt, not a failure. See
            // `WorkTreeProbe`'s doc for why this must not become
            // `[preflight] could not determine…`.
            return RepoStateResult {
                failures,
                skip_reason: Some("not a git work tree"),
            };
        }
        WorkTreeProbe::ProbeFailed(e) => {
            // An unexpected git failure (missing from PATH, a
            // `safe.directory` refusal, a permission error, …) must not read
            // as a clean pass the way "not a git repo" correctly does (S2).
            failures.push(format!(
                "[preflight] could not determine whether {} is inside a git work tree via `git \
                 rev-parse --is-inside-work-tree`: {e}",
                repo_dir.display()
            ));
            return RepoStateResult {
                failures,
                skip_reason: None,
            };
        }
        WorkTreeProbe::Inside => {}
    }

    let worktrees = match worktree_paths(repo_dir) {
        Ok(w) => w,
        Err(e) => {
            failures.push(format!(
                "[preflight] could not list worktrees via `git worktree list --porcelain -z` in \
                 {}: {e}",
                repo_dir.display()
            ));
            return RepoStateResult {
                failures,
                skip_reason: None,
            };
        }
    };
    let linked = match is_linked_worktree(repo_dir) {
        Ok(b) => b,
        Err(e) => {
            failures.push(format!(
                "[preflight] could not determine whether {} is a linked worktree via `git \
                 rev-parse --git-common-dir`: {e}",
                repo_dir.display()
            ));
            return RepoStateResult {
                failures,
                skip_reason: None,
            };
        }
    };

    if !is_gated(worktrees.len(), linked) {
        // A single, non-linked worktree — a fresh CI clone, or a lone
        // developer checkout — is exempt by construction. See the module
        // doc for why this is a structural test rather than an `if CI`
        // escape hatch.
        return RepoStateResult {
            failures,
            skip_reason: Some("single non-linked worktree"),
        };
    }

    // Check 10: not unexpectedly shallow.
    match is_shallow_repository(repo_dir) {
        Ok(true) => {
            let remedy = shallow_remedy(repo_dir);
            failures.push(format!(
                "[10] repository at {} is unexpectedly shallow while sharing an object store \
                 with other worktrees (`git rev-parse --is-shallow-repository` = true) — remedy: \
                 run {remedy}",
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
    // which check 12 already covered above).
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

    RepoStateResult {
        failures,
        skip_reason: None,
    }
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
    fn parse_worktree_list_porcelain_z_extracts_every_path() {
        let out = b"worktree /repo\0HEAD abc123\0branch refs/heads/main\0\0\
             worktree /repo-fix160\0HEAD def456\0branch refs/heads/fix/160\0\0\
             worktree /repo-detached\0HEAD 789abc\0detached\0";
        assert_eq!(
            parse_worktree_list_porcelain_z(out),
            vec![
                PathBuf::from("/repo"),
                PathBuf::from("/repo-fix160"),
                PathBuf::from("/repo-detached"),
            ]
        );
    }

    #[test]
    fn parse_worktree_list_porcelain_z_handles_a_single_entry() {
        let out = b"worktree /repo\0HEAD abc123\0branch refs/heads/main\0";
        assert_eq!(
            parse_worktree_list_porcelain_z(out),
            vec![PathBuf::from("/repo")]
        );
    }

    #[test]
    fn parse_worktree_list_porcelain_z_ignores_non_worktree_fields() {
        // Fields not prefixed with "worktree " (HEAD, branch, bare,
        // detached, the blank separator field) must never be mistaken for a
        // path.
        let out = b"bare\0worktree /only-real-entry\0HEAD abc123\0detached\0";
        assert_eq!(
            parse_worktree_list_porcelain_z(out),
            vec![PathBuf::from("/only-real-entry")]
        );
    }

    #[test]
    fn parse_worktree_list_porcelain_z_handles_a_path_containing_a_raw_newline() {
        // S1: the non-`-z` porcelain form does not quote paths at all, so a
        // path containing a literal newline byte used to split across two
        // porcelain lines and get mis-parsed as two separate entries. NUL
        // separation has no such ambiguity.
        let out = b"worktree /repo\nwith-newline\0HEAD abc123\0branch refs/heads/main\0\0";
        assert_eq!(
            parse_worktree_list_porcelain_z(out),
            vec![PathBuf::from("/repo\nwith-newline")]
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

    /// A bare directory with no `.git` — exactly the shape of
    /// `tests/duplicate_catalog_id.rs`'s own fixture — must be exempt, not a
    /// failure. Before this, `check` reported `[10] could not list
    /// worktrees…` for every such fixture, which broke that pre-existing
    /// test on every platform (`fatal: not a git repository`).
    #[test]
    fn check_is_exempt_on_a_directory_that_is_not_a_git_repository_at_all() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            probe_work_tree(tmp.path()),
            WorkTreeProbe::NotAGitRepo
        ));
        let result = check(tmp.path());
        assert!(result.failures.is_empty());
        assert_eq!(result.skip_reason, Some("not a git work tree"));
    }

    /// B1: check 12 must fire from `check()` itself when `repo_dir` no
    /// longer exists — decided from `current_worktree_missing`, never from
    /// spawning git in a directory that is gone. This is the case that used
    /// to return an empty failure list (the git spawn's `chdir` fails
    /// first), which is why `xtask/linkage-check/tests/repo_state_preflight.rs`
    /// has an end-to-end test reproducing the real-process shape of this
    /// same condition (a removed cwd inherited via `fork()`).
    #[test]
    fn check_reports_check_12_when_repo_dir_no_longer_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gone = tmp.path().join("never-created");
        let result = check(&gone);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].starts_with("[12]"));
        assert!(result.failures[0].contains("no longer exists on disk"));
        assert_eq!(result.skip_reason, None);
    }

    /// S7/N1: `probe_work_tree` must still classify a target as
    /// `NotAGitRepo` when the underlying `git` observes a non-English
    /// `LC_ALL` — proving the `git_command` locale override actually takes
    /// effect, rather than relying on a real localized git binary or a
    /// system locale package being installed (CI cannot be relied on to
    /// have either). A fake `git` script on `PATH` reports its "not a git
    /// repository" message in English when it sees `LC_ALL=C` (the override
    /// this fix installs) and in German otherwise, so if the override were
    /// ever removed this test would fail the same way a real German-locale
    /// contributor's machine does.
    #[cfg(unix)]
    #[test]
    fn probe_work_tree_is_locale_independent() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_bin_dir = tmp.path().join("fake-bin");
        std::fs::create_dir_all(&fake_bin_dir).expect("mkdir fake-bin");
        let fake_git = fake_bin_dir.join("git");
        std::fs::write(
            &fake_git,
            "#!/bin/sh\n\
             if [ \"$LC_ALL\" = \"C\" ]; then\n\
             echo 'fatal: not a git repository (or any of the parent directories): .git' >&2\n\
             else\n\
             echo 'Schwerwiegend: Kein Git-Repository (oder irgendein Elternverzeichnis): .git' >&2\n\
             fi\n\
             exit 128\n",
        )
        .expect("write fake git script");
        let mut perms = std::fs::metadata(&fake_git)
            .expect("stat fake git script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_git, perms).expect("chmod fake git script");

        let target = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&target).expect("mkdir not-a-repo");

        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = format!(
            "{}:{}",
            fake_bin_dir.display(),
            original_path.to_string_lossy()
        );
        // SAFETY: `std::env::set_var` is process-global, but CLAUDE.md rule
        // 5's fork addendum means every test run happens in CI via `cargo
        // nextest`, which runs each test in its OWN process — so no sibling
        // test in this module ever observes this mutation. The prior value
        // is restored below regardless, before this function returns.
        unsafe {
            std::env::set_var("PATH", &new_path);
            std::env::set_var("LC_ALL", "de_DE.UTF-8");
        }

        let result = probe_work_tree(&target);

        // SAFETY: see the comment on the previous unsafe block.
        unsafe {
            std::env::set_var("PATH", original_path);
            std::env::remove_var("LC_ALL");
        }

        assert!(
            matches!(result, WorkTreeProbe::NotAGitRepo),
            "expected the git_command locale override to force the English fatal message \
             regardless of the inherited LC_ALL, got {result:?}"
        );
    }
}
