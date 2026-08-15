//! `cargo xtask work-type-check` — derive a change's work type
//! (`bug | prd | doc | chore`) from the diff, and refuse to guess when it
//! cannot (PRD fork#340, R0).
//!
//! # RED-round scaffolding (M3, tester delegation)
//!
//! Every function body here is `todo!()`. The coder delegation that follows
//! this one fills them in against the contract `mod tests` below pins. A
//! stub that returned a fixed `Err` instead would make "neither supplier →
//! failure" pass vacuously — the worst possible RED for a PRD about empty
//! gates — so `todo!()` is deliberate, not a placeholder left by accident.
//!
//! `#![allow(dead_code)]` covers the whole module: nothing in `main.rs`
//! calls into it yet — wiring the `work-type-check` subcommand arm into the
//! multiplexer is the coder's job, not this RED round's — so every item
//! here is reachable only from `mod tests` below, which does not exist in
//! the non-test build `clippy --all-targets` also lints.
//!
//! # Supplier order — two tiers, then failure (CLAUDE.md rule 16)
//!
//! - **Tier 1** — the changelog fragment suffix added in this diff,
//!   `changelog.d/<stem>.<suffix>.md`, mapped via [`suffix_to_work_type`].
//!   A fragment whose suffix does not map (a retired alias like `fix` or
//!   `added`, or anything else) fails immediately here — it does **not**
//!   fall through to tier 2. Regression-pins the v0.24.3 incident: seven
//!   `.fix.md` fragments were silently ignored, leaving that release's
//!   notes empty.
//! - **Tier 2** — the branch name's work-type prefix, mapped via
//!   [`branch_prefix_to_work_type`], for the majority of PRs carrying no
//!   fragment.
//! - **Tier 3** — failure, naming both ways to fix it
//!   ([`WorkTypeError::NoSupplier`]) — the vacuity guard (E3): R0 must be
//!   unfailable-proof, or once every branch is prefixed derivation goes
//!   untested in production.
//!
//! If both tiers supply and disagree, that is *also* a failure
//! ([`WorkTypeError::FragmentBranchDisagree`]) — the fragment must not
//! silently win over a mismatched branch name, or vice versa.
//!
//! # The base-ref guard (E1)
//!
//! `ci.yml:132` checks out at depth 1, and on a `pull_request` event there
//! is no `origin/main` ref at all, so a bare `git merge-base HEAD
//! origin/main` fails. [`resolve_base`] must never turn that into a silent
//! success, and [`run_in`] must exit non-zero with a *distinct* code
//! ([`EXIT_BASE_UNRESOLVABLE`]) rather than the generic rule-violation code
//! ([`EXIT_RULE_VIOLATION`]) — otherwise this gate is empty on day one.

#![allow(dead_code)]

use std::path::Path;
use std::process::ExitCode;

/// `git merge-base HEAD <base>` target when `--base` is not given.
pub const DEFAULT_BASE: &str = "origin/main";

/// Exit code for an ordinary R0 rejection (no supplier, conflicting
/// fragments, fragment/branch disagreement, an unknown suffix).
pub const EXIT_RULE_VIOLATION: u8 = 1;

/// Exit code reserved for "the base ref itself could not be resolved" (E1)
/// — kept distinct from [`EXIT_RULE_VIOLATION`] so a CI log can tell the
/// two apart without parsing prose.
pub const EXIT_BASE_UNRESOLVABLE: u8 = 3;

/// The four work types this gate derives. `bug | prd | doc | chore` per the
/// vocabulary reconciled in `docs/develop/work-types.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkType {
    Bug,
    Prd,
    Doc,
    Chore,
}

/// One `changelog.d/<stem>.<suffix>.md` fragment added in this diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedFragment {
    /// Repo-relative path, e.g. `changelog.d/341.feature.md` — carried in
    /// error messages so a conflict names the actual files, not just the
    /// types they resolved to.
    pub path: String,
    /// The suffix alone, e.g. `feature`.
    pub suffix: String,
}

/// Which tier supplied the derived type — echoed in the gate's success line
/// (PRD fork#340: "the success line names the derived type and the
/// supplying tier"; E3's `--self-test` needs this to prove which tier
/// actually fired, not just that some tier did).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supplier {
    Fragment,
    BranchPrefix,
}

/// The successful R0 result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derivation {
    pub work_type: WorkType,
    pub supplier: Supplier,
}

/// Why R0 rejected a diff. Every variant is worded so the caller can print
/// "both ways to fix it" rather than a bare rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkTypeError {
    /// Neither tier supplied a type: no recognized fragment suffix was
    /// added, and `branch` carries none of the four recognized prefixes.
    /// The vacuity guard (E3) — this is R0's only unconditionally-reachable
    /// failure, so it must stay reachable even once every branch is
    /// prefixed correctly.
    NoSupplier { branch: String },
    /// A fragment was added, but `suffix` is not one of the five real types
    /// (`bugfix`, `feature`, `breaking`, `doc`, `misc`) — including every
    /// retired alias (`fix`, `added`, `changed`, `fixed`, `removed`).
    /// Deliberately NOT folded into [`Self::NoSupplier`]: an unrecognized
    /// suffix must fail loudly, never fall through to tier 2 and be
    /// silently ignored (the v0.24.3 incident).
    UnknownSuffix { path: String, suffix: String },
    /// Two (or more) fragments added in this diff resolved to different
    /// work types. Both are named so neither silently wins.
    ConflictingFragments {
        first: (String, WorkType),
        second: (String, WorkType),
    },
    /// Both tiers supplied a type and they disagree — e.g. a `fix/` branch
    /// carrying a `.feature.md` fragment. Both are named.
    FragmentBranchDisagree {
        fragment: (String, WorkType),
        branch: (String, WorkType),
    },
    /// `git merge-base HEAD <base>` failed to resolve — `base` is the ref
    /// that was tried (`--base`'s value, or [`DEFAULT_BASE`]), `detail` is
    /// git's own error text. Must map to [`EXIT_BASE_UNRESOLVABLE`], never
    /// to a zero exit (E1).
    BaseUnresolvable { base: String, detail: String },
}

/// Tier 1: map a changelog fragment suffix to a work type.
///
/// `None` for anything outside the five real types declared in
/// `pyproject.toml` — including every retired alias (`fix`, `added`,
/// `changed`, `fixed`, `removed`) and any other spelling. The caller
/// ([`derive_work_type`]) is responsible for turning a fragment-present,
/// `None`-mapped suffix into [`WorkTypeError::UnknownSuffix`] rather than
/// silently treating it as "tier 1 supplied nothing."
pub fn suffix_to_work_type(suffix: &str) -> Option<WorkType> {
    todo!("work_type::suffix_to_work_type({suffix:?})")
}

/// Tier 2: map a branch name's prefix to a work type.
///
/// `None` when `branch` carries no `/` at all, or a prefix outside the four
/// recognized ones (`fix/`, `feat/`, `docs/`, `chore/`) — both cases mean
/// "tier 2 supplies nothing," which [`derive_work_type`] turns into
/// [`WorkTypeError::NoSupplier`] when tier 1 is also empty.
pub fn branch_prefix_to_work_type(branch: &str) -> Option<WorkType> {
    todo!("work_type::branch_prefix_to_work_type({branch:?})")
}

/// R0: derive the work type for one diff from its added fragments and
/// branch name, following the two-tier order documented on the module.
/// Never guesses — every path that cannot cleanly resolve to exactly one
/// work type is an `Err`.
pub fn derive_work_type(
    fragments: &[AddedFragment],
    branch: &str,
) -> Result<Derivation, WorkTypeError> {
    todo!("work_type::derive_work_type({fragments:?}, {branch:?})")
}

/// Resolve `--base` (an explicit ref if given, else [`DEFAULT_BASE`]) via
/// `git merge-base HEAD <base>`, run with `repo_dir` as the working
/// directory. Returns the resolved commit SHA, or
/// [`WorkTypeError::BaseUnresolvable`] — never a silent success — when the
/// ref does not exist in `repo_dir` (E1: `ci.yml:132`'s depth-1
/// `pull_request` checkout has no `origin/main` ref at all).
pub fn resolve_base(explicit: Option<&str>, repo_dir: &Path) -> Result<String, WorkTypeError> {
    todo!("work_type::resolve_base({explicit:?}, {repo_dir:?})")
}

/// The testable core of `cargo xtask work-type-check`: parses `args`,
/// derives the work type for the diff in `repo_dir` against the resolved
/// base, and returns the process exit code — [`ExitCode::SUCCESS`],
/// [`EXIT_RULE_VIOLATION`], or [`EXIT_BASE_UNRESOLVABLE`]. Split from
/// [`run`] so a test can point it at a scratch git repo instead of the real
/// one.
pub fn run_in(args: &[String], repo_dir: &Path) -> ExitCode {
    todo!("work_type::run_in({args:?}, {repo_dir:?})")
}

/// `cargo xtask work-type-check`'s entry point — [`run_in`] against the
/// current directory. Wired into the subcommand multiplexer by the coder
/// delegation that follows this RED round; not called from `main.rs` yet.
pub fn run(args: &[String]) -> ExitCode {
    todo!("work_type::run({args:?})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Run `git <args>` in `dir`, panicking with git's own stderr on
    /// failure — these fixtures are the test's own setup, not the thing
    /// under test, so a setup failure should look like a setup failure.
    fn git(args: &[&str], dir: &Path) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("run git {args:?} in {dir:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} in {dir:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // -- Tier 1: fragment suffix ------------------------------------------

    #[test]
    fn suffix_to_work_type_maps_all_five_real_suffixes() {
        assert_eq!(suffix_to_work_type("bugfix"), Some(WorkType::Bug));
        assert_eq!(suffix_to_work_type("feature"), Some(WorkType::Prd));
        assert_eq!(suffix_to_work_type("breaking"), Some(WorkType::Prd));
        assert_eq!(suffix_to_work_type("doc"), Some(WorkType::Doc));
        assert_eq!(suffix_to_work_type("misc"), Some(WorkType::Chore));
    }

    #[test]
    fn retired_suffixes_do_not_resolve() {
        for suffix in ["fix", "added", "changed", "fixed", "removed", "docs"] {
            assert_eq!(
                suffix_to_work_type(suffix),
                None,
                "retired/unrecognized suffix {suffix:?} must not resolve to a work type"
            );
        }
    }

    #[test]
    fn a_retired_suffix_fragment_fails_derivation_rather_than_falling_through() {
        // The v0.24.3 incident, pinned: `.fix.md` must fail loudly, not be
        // silently ignored (which is what left that release's notes empty).
        let fragments = [AddedFragment {
            path: "changelog.d/103.fix.md".to_string(),
            suffix: "fix".to_string(),
        }];
        let err = derive_work_type(&fragments, "fix/103-thing")
            .expect_err("a retired suffix must fail, not resolve");
        assert_eq!(
            err,
            WorkTypeError::UnknownSuffix {
                path: "changelog.d/103.fix.md".to_string(),
                suffix: "fix".to_string(),
            },
            "a retired suffix must fail as UnknownSuffix, not silently fall through to tier 2"
        );
    }

    // -- Tier 2: branch prefix --------------------------------------------

    #[test]
    fn branch_prefix_to_work_type_maps_all_four_prefixes() {
        assert_eq!(
            branch_prefix_to_work_type("fix/123-thing"),
            Some(WorkType::Bug)
        );
        assert_eq!(
            branch_prefix_to_work_type("feat/123-thing"),
            Some(WorkType::Prd)
        );
        assert_eq!(
            branch_prefix_to_work_type("docs/123-thing"),
            Some(WorkType::Doc)
        );
        assert_eq!(
            branch_prefix_to_work_type("chore/123-thing"),
            Some(WorkType::Chore)
        );
    }

    #[test]
    fn unknown_branch_prefix_fails_derivation() {
        for branch in ["wip/something", "release-2026-08", "main"] {
            assert_eq!(
                branch_prefix_to_work_type(branch),
                None,
                "branch {branch:?} carries no recognized work-type prefix"
            );
            let err = derive_work_type(&[], branch)
                .expect_err("an unrecognized/absent branch prefix with no fragment must fail");
            assert_eq!(
                err,
                WorkTypeError::NoSupplier {
                    branch: branch.to_string()
                }
            );
        }
    }

    // -- Tier 3: neither supplier (the vacuity guard, E3) ------------------

    #[test]
    fn neither_supplier_present_fails_derivation() {
        let err = derive_work_type(&[], "some-unprefixed-branch-name")
            .expect_err("no fragment and no recognized branch prefix must fail (E3)");
        assert_eq!(
            err,
            WorkTypeError::NoSupplier {
                branch: "some-unprefixed-branch-name".to_string()
            }
        );
    }

    // -- Conflicts within and across tiers ----------------------------------

    #[test]
    fn two_fragments_mapping_to_different_types_fail_naming_both() {
        let fragments = [
            AddedFragment {
                path: "changelog.d/200.bugfix.md".to_string(),
                suffix: "bugfix".to_string(),
            },
            AddedFragment {
                path: "changelog.d/201.feature.md".to_string(),
                suffix: "feature".to_string(),
            },
        ];
        let err = derive_work_type(&fragments, "fix/200-thing")
            .expect_err("two fragments resolving to different types must fail");
        match err {
            WorkTypeError::ConflictingFragments { first, second } => {
                assert_eq!(
                    first,
                    ("changelog.d/200.bugfix.md".to_string(), WorkType::Bug)
                );
                assert_eq!(
                    second,
                    ("changelog.d/201.feature.md".to_string(), WorkType::Prd)
                );
            }
            other => panic!("expected ConflictingFragments, got {other:?}"),
        }
    }

    #[test]
    fn fragment_and_branch_disagreement_fails_naming_both() {
        // A `fix/` branch carrying a `.feature.md` fragment — the PRD's own
        // example of "either a mislabelled branch or a feature about to
        // ship as a patch release."
        let fragments = [AddedFragment {
            path: "changelog.d/202.feature.md".to_string(),
            suffix: "feature".to_string(),
        }];
        let err = derive_work_type(&fragments, "fix/202-thing")
            .expect_err("fragment and branch disagreeing must fail, not let the fragment win");
        match err {
            WorkTypeError::FragmentBranchDisagree { fragment, branch } => {
                assert_eq!(
                    fragment,
                    ("changelog.d/202.feature.md".to_string(), WorkType::Prd)
                );
                assert_eq!(branch, ("fix/202-thing".to_string(), WorkType::Bug));
            }
            other => panic!("expected FragmentBranchDisagree, got {other:?}"),
        }
    }

    // -- The base-ref guard (E1) ---------------------------------------------

    #[test]
    fn unresolvable_base_produces_a_distinct_nonzero_exit_code() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // The exact shape of `ci.yml:132`'s depth-1 `pull_request` checkout:
        // a repo with commits but no `origin/main` ref at all.
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        git(&["commit", "-q", "--allow-empty", "-m", "init"], tmp.path());

        let code = run_in(&[], tmp.path());
        assert_ne!(
            code,
            ExitCode::SUCCESS,
            "must never exit 0 when the base ref cannot be resolved (E1)"
        );
        assert_eq!(
            code,
            ExitCode::from(EXIT_BASE_UNRESOLVABLE),
            "must use the distinct base-unresolvable exit code, not the generic \
             rule-violation one (E1) — so a CI log can tell them apart without \
             parsing prose"
        );
    }
}
