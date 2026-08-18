//! `cargo xtask work-type-check` — derive a change's work type
//! (`bug | prd | doc | chore`) from the diff, and refuse to guess when it
//! cannot (PRD fork#340, R0).
//!
//! Wired into the subcommand multiplexer in `main.rs` beside `list-tests`
//! and `clean-e2e-tmp`. M3 ships R0 only — the two-tier derivation below,
//! `--self-test`, and the base-ref guard (E1). R1–R4 are M4.
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
//!
//! # The merged-base skip (post-merge review finding B1)
//!
//! `ci.yml` also runs `build` on `push: [main]` (post-merge verification —
//! see that trigger's own comment for why). On that event the resolved base
//! and `HEAD` are the same commit: there is no diff, so there is nothing to
//! classify. [`run_in`] treats `base_sha == HEAD` as a **printed skip**
//! returning [`ExitCode::SUCCESS`], deliberately kept out of
//! [`EXEMPT_BRANCH_PREFIXES`] — `main` is not a work-type-exempt branch, it
//! is a diff with nothing left in it. Without this, every merge to `main`
//! trips [`WorkTypeError::NoSupplier`] and the post-merge signal this repo
//! depends on to catch a broken `main` (`ci.yml`'s own twelve-line
//! rationale, naming the 2026-07-29 incident) trains everyone to ignore it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::list_tests::TestEntry;

/// Branch prefixes that skip the gate outright — Renovate PRs automerge
/// without a human (`ci.yml:14-16`), so a gate that blocked them would get
/// disabled rather than obeyed. `sync/` and `upstream/` carry fork-sync
/// commits, which are equally not a human's PR to label.
///
/// **This is advisory, not a security boundary (F3).** It is matched
/// against the branch *name*, and any PR author can rename their branch to
/// one of these prefixes to skip the gate — the unforgeable signal would be
/// the PR *author* (`ci.yml`'s `changes` job already reads exactly that for
/// Renovate), which this gate does not, because re-plumbing it to read the
/// CI event payload would couple the binary to CI env and break the
/// local-invocation property `CONTRIBUTING.md:55` protects. A PR still
/// needs an approving human review to merge either way.
const EXEMPT_BRANCH_PREFIXES: [&str; 3] = ["renovate/", "sync/", "upstream/"];

/// The single source for the "N rules" count in [`describe_success`]'s
/// success line and [`self_test`]'s case array length (N1) — one literal
/// for one fact, rather than the two that used to drift independently.
const RULE_COUNT: usize = 5;

/// The one changelog suffix that maps to [`WorkType::Prd`] via
/// [`suffix_to_work_type`] but is a severity axis, not a work type of its
/// own (fork#451) — the single fact four sites must agree on:
/// [`suffix_to_work_type`]'s match arm, [`derive_work_type`]'s
/// `fragment_all_breaking` tracking and its `named_path` selection, and
/// [`collect_bug_diff`]'s `No-Test:` filter. One literal for one fact
/// ([`RULE_COUNT`]'s shape), so the frozen five-suffix set gaining another
/// `Prd`-mapping severity suffix cannot silently miss one of the sites that
/// must agree on it.
const BREAKING_SUFFIX: &str = "breaking";

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

impl WorkType {
    /// The label used in the gate's own output — `docs/develop/work-types.md`'s
    /// vocabulary, not the changelog fragment suffix or branch prefix that
    /// derived it.
    fn label(self) -> &'static str {
        match self {
            WorkType::Bug => "bug",
            WorkType::Prd => "prd",
            WorkType::Doc => "doc",
            WorkType::Chore => "chore",
        }
    }
}

impl fmt::Display for WorkType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
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

impl fmt::Display for WorkTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkTypeError::NoSupplier { branch } => write!(
                f,
                "no work type could be derived: no recognized changelog fragment suffix \
                 was added in this diff, and branch {branch:?} carries none of the \
                 recognized prefixes (fix/, feat/, docs/, chore/). Fix either: add a \
                 changelog.d/<n>.<suffix>.md fragment, or rename the branch with one of \
                 those prefixes."
            ),
            WorkTypeError::UnknownSuffix { path, suffix } => write!(
                f,
                "{path} has suffix {suffix:?}, which is not one of the five recognized \
                 work-type suffixes (bugfix, feature, breaking, doc, misc) — including \
                 every retired alias. Rename the fragment to use a recognized suffix."
            ),
            WorkTypeError::ConflictingFragments { first, second } => write!(
                f,
                "two changelog fragments added in this diff resolve to different work \
                 types: {} -> '{}' and {} -> '{}'. Make them agree, or remove one.",
                first.0, first.1, second.0, second.1
            ),
            WorkTypeError::FragmentBranchDisagree { fragment, branch } => write!(
                f,
                "the changelog fragment {} resolves to work type '{}', but branch \
                 {:?} resolves to '{}'. Fix either: correct the fragment's suffix, or \
                 rename the branch to match — this is either a mislabelled branch or a \
                 feature about to ship as a patch release.",
                fragment.0, fragment.1, branch.0, branch.1
            ),
            WorkTypeError::BaseUnresolvable { base, detail } => write!(
                f,
                "base ref {base:?} could not be resolved via `git merge-base HEAD {base}`: \
                 {detail}"
            ),
        }
    }
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
    match suffix {
        "bugfix" => Some(WorkType::Bug),
        "feature" => Some(WorkType::Prd),
        BREAKING_SUFFIX => Some(WorkType::Prd),
        "doc" => Some(WorkType::Doc),
        "misc" => Some(WorkType::Chore),
        _ => None,
    }
}

/// Whether `fragment` maps to [`WorkType::Prd`] via [`suffix_to_work_type`]
/// but is not the [`BREAKING_SUFFIX`] severity axis — i.e. a genuine
/// feature-shaped fragment, as opposed to a `.breaking.md` whose `Prd`
/// mapping is only ever a no-branch-signal fallback. Shared by
/// [`derive_work_type`]'s `any_feature_fragment` precomputation and its
/// `named_path` selection so a future non-`breaking` `Prd`-mapping suffix
/// is caught by both sites rather than only one (`BREAKING_SUFFIX`'s doc
/// comment names this as one of the four sites that must agree).
fn is_non_breaking_prd_fragment(fragment: &AddedFragment) -> bool {
    suffix_to_work_type(&fragment.suffix) == Some(WorkType::Prd)
        && fragment.suffix != BREAKING_SUFFIX
}

/// Tier 2: map a branch name's prefix to a work type.
///
/// `None` when `branch` carries no `/` at all, or a prefix outside the four
/// recognized ones (`fix/`, `feat/`, `docs/`, `chore/`) — both cases mean
/// "tier 2 supplies nothing," which [`derive_work_type`] turns into
/// [`WorkTypeError::NoSupplier`] when tier 1 is also empty.
pub fn branch_prefix_to_work_type(branch: &str) -> Option<WorkType> {
    let (prefix, _rest) = branch.split_once('/')?;
    match prefix {
        "fix" => Some(WorkType::Bug),
        "feat" => Some(WorkType::Prd),
        "docs" => Some(WorkType::Doc),
        "chore" => Some(WorkType::Chore),
        _ => None,
    }
}

/// R0: derive the work type for one diff from its added fragments and
/// branch name, following the two-tier order documented on the module.
/// Never guesses — every path that cannot cleanly resolve to exactly one
/// work type is an `Err`.
///
/// `breaking` is a severity axis, not a work type
/// (`docs/develop/work-types.md`): a `.breaking.md` fragment maps to
/// [`WorkType::Prd`] via [`suffix_to_work_type`] only as its
/// no-branch-signal fallback. When the branch supplies a signal too, this
/// function — not `suffix_to_work_type`, which has no branch to consult —
/// resolves a `.breaking.md`-only fragment supply against the branch:
/// `fix/` re-resolves it to [`WorkType::Bug`] rather than erroring, while
/// `docs/`/`chore/` still disagree and error as before. This stays
/// deliberately narrow to "every added Prd-mapping fragment is
/// specifically `breaking`" (tracked below as `fragment_all_breaking`) —
/// a genuine `.feature.md` fragment (alone, or alongside a `.breaking.md`
/// with no [`WorkTypeError::ConflictingFragments`] between them, since both
/// map to `Prd`) must still disagree with a `fix/` branch exactly as
/// today, because that combination is a real feature, not a breaking bug
/// fix.
///
/// This carve-out returns [`Supplier::BranchPrefix`], not
/// [`Supplier::Fragment`] — the branch's `Bug` signal is what actually won
/// this resolution, so that is also the more honest label for
/// [`describe_success`] and `--self-test` to report.
///
/// **Accepted trade-off, not a new hole:** routing a `.breaking.md`-only
/// fragment to `Bug` here means it skips [`check_prd_fragments`] (R4),
/// which only runs for a `WorkType::Prd` derivation — so a `prd`-class
/// breaking change can ship classified as `bug`, without R4's `prds/`
/// linkage check, purely by naming the branch `fix/`. This is consistent
/// with [`EXEMPT_BRANCH_PREFIXES`]'s existing threat model (the branch name
/// is not a trust signal, and a PR still needs an approving human review to
/// merge); it is called out here so a reader does not mistake R4 for an
/// unconditional guarantee.
pub fn derive_work_type(
    fragments: &[AddedFragment],
    branch: &str,
) -> Result<Derivation, WorkTypeError> {
    // fork#453: a `.breaking.md` fragment's `Prd` mapping is only ever a
    // no-branch-signal fallback (see the doc above) — so it is not a real
    // conflict against a genuine `.bugfix.md` (`Bug`) fragment, in either
    // order, as long as no genuine `.feature.md` is also present in the
    // set. `.feature.md` alongside `.bugfix.md` is a real, non-breaking
    // conflict and must still fail below exactly as before
    // (`bugfix_plus_feature_fragments_still_conflict_regression_guard`) —
    // precomputed once, order-independently, over the whole fragment set,
    // so an exemption never fires just because the feature fragment
    // happens to sort after the bugfix/breaking pair.
    let any_feature_fragment = fragments.iter().any(is_non_breaking_prd_fragment);

    // Tier 1: every added fragment must map, and every mapped fragment must
    // agree — an unrecognized suffix fails immediately (does not fall
    // through to tier 2), and a disagreement between two fragments fails
    // before tier 2 is even consulted, except for the `.bugfix.md` +
    // `.breaking.md` exemption above, which is deferred to tier 2 instead.
    let mut fragment_supply: Option<(String, WorkType)> = None;
    let mut fragment_all_breaking = true;
    for fragment in fragments {
        let work_type =
            suffix_to_work_type(&fragment.suffix).ok_or_else(|| WorkTypeError::UnknownSuffix {
                path: fragment.path.clone(),
                suffix: fragment.suffix.clone(),
            })?;
        if fragment.suffix != BREAKING_SUFFIX {
            fragment_all_breaking = false;
        }

        let Some((first_path, first_type)) = fragment_supply.clone() else {
            fragment_supply = Some((fragment.path.clone(), work_type));
            continue;
        };

        if first_type == work_type {
            continue;
        }

        let is_breaking_bug_pair = (first_type == WorkType::Bug
            && fragment.suffix == BREAKING_SUFFIX)
            || (first_type == WorkType::Prd && !any_feature_fragment && work_type == WorkType::Bug);
        if is_breaking_bug_pair {
            if work_type == WorkType::Bug {
                // A genuine `.bugfix.md` fragment supersedes a
                // breaking-only `Prd` fallback as the authoritative
                // established type, so a later real conflict (e.g. a
                // `.feature.md`) still compares against `Bug` and fails.
                fragment_supply = Some((fragment.path.clone(), WorkType::Bug));
            }
            continue;
        }

        return Err(WorkTypeError::ConflictingFragments {
            first: (first_path, first_type),
            second: (fragment.path.clone(), work_type),
        });
    }

    let branch_supply = branch_prefix_to_work_type(branch);

    match (fragment_supply, branch_supply) {
        (Some((path, fragment_type)), Some(branch_type)) => {
            if fragment_type == branch_type {
                Ok(Derivation {
                    work_type: fragment_type,
                    supplier: Supplier::Fragment,
                })
            } else if fragment_type == WorkType::Prd
                && fragment_all_breaking
                && branch_type == WorkType::Bug
            {
                // Every Prd-mapping fragment here is `.breaking.md`, none is
                // `.feature.md`, and the branch says `fix/` — the fragment's
                // Prd mapping was only ever the no-branch-signal fallback,
                // so the branch's Bug signal wins instead of disagreeing.
                // `supplier` is `BranchPrefix`, not `Fragment` (F1): the
                // branch is what actually supplied `Bug` here.
                Ok(Derivation {
                    work_type: WorkType::Bug,
                    supplier: Supplier::BranchPrefix,
                })
            } else {
                // F3: when this disagreement is a Prd-mapping mismatch
                // (the guard above declined because a genuine, non-breaking
                // Prd fragment is also present), name that fragment rather
                // than whichever one happened to sort first into
                // `fragment_supply` — a `.breaking.md` sorting first would
                // otherwise get blamed for a suffix this very carve-out
                // makes legal, pointing the author at the wrong file. Falls
                // back to `path` when no non-`breaking` Prd fragment exists
                // — the breaking-only case, which reaches this arm whenever
                // the branch is `docs/`/`chore/` rather than `fix/` (the
                // `fix/` case is taken by the carve-out above). Naming the
                // `.breaking.md` is correct there: it is the actual
                // offender. Pinned by
                // `breaking_fragment_on_docs_branch_still_disagrees`.
                let named_path = if fragment_type == WorkType::Prd {
                    fragments
                        .iter()
                        .find(|f| is_non_breaking_prd_fragment(f))
                        .map(|f| f.path.clone())
                        .unwrap_or(path)
                } else {
                    path
                };
                Err(WorkTypeError::FragmentBranchDisagree {
                    fragment: (named_path, fragment_type),
                    branch: (branch.to_string(), branch_type),
                })
            }
        }
        (Some((_path, fragment_type)), None) => Ok(Derivation {
            work_type: fragment_type,
            supplier: Supplier::Fragment,
        }),
        (None, Some(branch_type)) => Ok(Derivation {
            work_type: branch_type,
            supplier: Supplier::BranchPrefix,
        }),
        (None, None) => Err(WorkTypeError::NoSupplier {
            branch: branch.to_string(),
        }),
    }
}

/// Resolve `--base` (an explicit ref if given, else [`DEFAULT_BASE`]) via
/// `git merge-base HEAD <base>`, run with `repo_dir` as the working
/// directory. Returns the resolved commit SHA, or
/// [`WorkTypeError::BaseUnresolvable`] — never a silent success — when the
/// ref does not exist in `repo_dir` (E1: `ci.yml:132`'s depth-1
/// `pull_request` checkout has no `origin/main` ref at all).
pub fn resolve_base(explicit: Option<&str>, repo_dir: &Path) -> Result<String, WorkTypeError> {
    let base = explicit.unwrap_or(DEFAULT_BASE).to_string();
    let to_err = |detail: String| WorkTypeError::BaseUnresolvable {
        base: base.clone(),
        detail,
    };

    let out = Command::new("git")
        .args(["merge-base", "HEAD", &base])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| to_err(format!("invoke git merge-base HEAD {base}: {e}")))?;
    if !out.status.success() {
        return Err(to_err(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(to_err("git merge-base returned empty output".to_string()));
    }
    Ok(sha)
}

/// `git rev-parse HEAD` in `repo_dir` — used only by the B1 merged-base skip
/// in [`run_in`], which compares this against the resolved base to detect
/// "nothing left in this diff to classify."
fn current_head_sha(repo_dir: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git rev-parse HEAD: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err("git rev-parse HEAD returned empty output".to_string());
    }
    Ok(sha)
}

/// The testable core of `cargo xtask work-type-check`: parses `args`,
/// derives the work type for the diff in `repo_dir` against the resolved
/// base, and returns the process exit code — [`ExitCode::SUCCESS`],
/// [`EXIT_RULE_VIOLATION`], or [`EXIT_BASE_UNRESOLVABLE`]. Split from
/// [`run`] so a test can point it at a scratch git repo instead of the real
/// one.
///
/// `branch_override`, when `Some`, is used verbatim instead of consulting
/// [`current_branch`] (B2: a test built against a scratch repo must not
/// have its branch silently overridden by `GITHUB_HEAD_REF`/`GITHUB_REF_NAME`
/// — those are set from the *real* PR whenever this runs in CI, which is
/// exactly the trap [`run_pipeline_for_self_test`]'s own doc comment
/// already names). Production (`run`) always passes `None`.
pub fn run_in(args: &[String], repo_dir: &Path, branch_override: Option<&str>) -> ExitCode {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("xtask work-type-check: {msg}");
            print_usage();
            return ExitCode::from(2);
        }
    };
    if parsed.help {
        print_usage();
        return ExitCode::SUCCESS;
    }
    if parsed.self_test {
        return self_test();
    }

    let branch = match branch_override {
        Some(branch) => branch.to_string(),
        None => match current_branch(repo_dir) {
            Ok(branch) => branch,
            Err(e) => {
                eprintln!("work-type-check: could not determine the current branch: {e}");
                return ExitCode::from(EXIT_RULE_VIOLATION);
            }
        },
    };

    if is_exempt_branch(&branch) {
        println!("work-type-check: skipped (branch '{branch}' is exempt)");
        return ExitCode::SUCCESS;
    }

    let base_sha = match resolve_base(parsed.base.as_deref(), repo_dir) {
        Ok(sha) => sha,
        Err(WorkTypeError::BaseUnresolvable { base, detail }) => {
            eprintln!("work-type-check: base ref {base:?} could not be resolved: {detail}");
            return ExitCode::from(EXIT_BASE_UNRESOLVABLE);
        }
        Err(other) => {
            // resolve_base only ever produces BaseUnresolvable; kept
            // exhaustive rather than unreachable!() so a future variant
            // added to WorkTypeError fails to compile here instead of
            // panicking at runtime.
            eprintln!("work-type-check: {other}");
            return ExitCode::from(EXIT_RULE_VIOLATION);
        }
    };

    // B1: a merged base has nothing left to classify — this is the exact
    // shape of a post-merge `push: [main]` run (`resolve_base` walks to
    // `HEAD` itself once `origin/main` already includes it). Kept distinct
    // from `is_exempt_branch` above: `main` is not being treated as an
    // exempt branch name, it is a diff with no content. A `current_head_sha`
    // failure here is not this check's concern — it falls through and
    // surfaces (correctly) wherever the pipeline below next needs `HEAD`.
    if let Ok(head_sha) = current_head_sha(repo_dir)
        && head_sha == base_sha
    {
        println!(
            "work-type-check: skipped (a merged base has no work type to derive — branch '{branch}')"
        );
        return ExitCode::SUCCESS;
    }

    let fragments = match collect_added_fragments(repo_dir, &base_sha) {
        Ok(fragments) => fragments,
        Err(e) => {
            eprintln!("work-type-check: could not read added changelog fragments: {e}");
            return ExitCode::from(EXIT_RULE_VIOLATION);
        }
    };

    let derivation = match derive_work_type(&fragments, &branch) {
        Ok(derivation) => derivation,
        Err(e) => {
            eprintln!("work-type-check: {e}");
            return ExitCode::from(EXIT_RULE_VIOLATION);
        }
    };

    if let Err(e) = check_rule(&derivation, repo_dir, &base_sha, &fragments) {
        eprintln!("work-type-check: {e}");
        return ExitCode::from(EXIT_RULE_VIOLATION);
    }

    println!(
        "work-type-check: ok ({})",
        describe_success(&derivation, &branch, &base_sha, &fragments)
    );
    ExitCode::SUCCESS
}

/// `cargo xtask work-type-check`'s entry point — [`run_in`] against the
/// current directory, with no branch override (production always derives
/// the branch from the environment/git, per [`current_branch`]).
pub fn run(args: &[String]) -> ExitCode {
    let repo_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_in(args, &repo_dir, None)
}

struct ParsedArgs {
    help: bool,
    self_test: bool,
    base: Option<String>,
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut parsed = ParsedArgs {
        help: false,
        self_test: false,
        base: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => parsed.help = true,
            "--self-test" => parsed.self_test = true,
            "--base" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--base requires a value".to_string())?;
                parsed.base = Some(value.clone());
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
        i += 1;
    }
    Ok(parsed)
}

fn print_usage() {
    println!("usage: cargo xtask work-type-check [--base <ref>] [--self-test]");
    println!();
    println!("Derives this diff's work type (bug | prd | doc | chore) from the added");
    println!("changelog.d fragment suffix, else the branch name's work-type prefix, and");
    println!("fails if neither supplies one or the two disagree (PRD fork#340, R0).");
}

/// Whether `branch` is exempt from the gate outright — see
/// [`EXEMPT_BRANCH_PREFIXES`].
fn is_exempt_branch(branch: &str) -> bool {
    EXEMPT_BRANCH_PREFIXES
        .iter()
        .any(|prefix| branch.starts_with(prefix))
}

/// The branch under test. GitHub Actions checks out `pull_request` events at
/// a **detached** HEAD (the merge commit), so `git rev-parse --abbrev-ref
/// HEAD` there answers literally `HEAD` — not the PR's source branch — which
/// would silently starve tier 2 for every PR carrying no changelog fragment.
/// `GITHUB_HEAD_REF` (pull_request) and `GITHUB_REF_NAME` (push) are the
/// values GitHub Actions sets for exactly this; git is the fallback for a
/// local invocation, where neither is set.
fn current_branch(repo_dir: &Path) -> Result<String, String> {
    if let Ok(head_ref) = std::env::var("GITHUB_HEAD_REF")
        && !head_ref.is_empty()
    {
        return Ok(head_ref);
    }
    if let Ok(ref_name) = std::env::var("GITHUB_REF_NAME")
        && !ref_name.is_empty()
    {
        return Ok(ref_name);
    }

    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git rev-parse --abbrev-ref HEAD: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() {
        return Err("git rev-parse --abbrev-ref HEAD returned empty output".to_string());
    }
    Ok(branch)
}

/// Every `changelog.d/*.md` fragment **added** (not modified, not deleted)
/// between `base_sha` and `HEAD` — tier 1's supply. `--diff-filter=AR` is
/// load-bearing on the `A` half: a fragment merely touched by this diff
/// (e.g. a rebase conflict resolution) is not "added in this diff" and must
/// not count. `R` is included and resolved to its *destination* path (F2):
/// git's default rename detection reports a fragment suffix correction
/// (`git mv 194.bugfix.md 500.feature.md`) as `R100`, not `A`, so `A` alone
/// let a renamed-into-existence fragment through invisibly.
fn collect_added_fragments(repo_dir: &Path, base_sha: &str) -> Result<Vec<AddedFragment>, String> {
    let out = Command::new("git")
        .args([
            "diff",
            "-z",
            "--name-status",
            "--diff-filter=AR",
            base_sha,
            "HEAD",
            "--",
            "changelog.d",
        ])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git diff --name-status {base_sha} HEAD: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }

    let mut fragments: Vec<AddedFragment> = parse_name_status_z(&out.stdout)
        .into_iter()
        .filter_map(|path| {
            if !path.ends_with(".md") {
                return None;
            }
            let file_name = Path::new(&path).file_name()?.to_str()?.to_string();
            // "<stem>.<suffix>.md" — the suffix is whatever sits immediately
            // before the trailing ".md", however many dots the stem itself
            // carries (towncrier's `<issue>.<counter>.<suffix>.md` form).
            let mut segments = file_name.rsplitn(3, '.');
            segments.next()?; // "md"
            let suffix = segments.next()?.to_string();
            Some(AddedFragment { path, suffix })
        })
        .collect();
    fragments.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(fragments)
}

/// Parse `git diff -z --name-status ...`'s NUL-separated output into the
/// resulting path of each change — the *destination* path for a rename/copy
/// (a status starting with `R`/`C`, which carries the old path then the new
/// one), the sole path otherwise. `-z` is what makes this parseable at all
/// (F1): git's default newline-separated `--name-status` C-quotes any
/// non-ASCII or control byte in a path (`core.quotePath`, on by default),
/// wrapping it in `"..."` with octal escapes — every caller here used to
/// read that quoted form verbatim, so `starts_with`/`ends_with` against a
/// bare prefix or suffix silently never matched and the file fell out of
/// the gate entirely. `-z` disables the quoting.
fn parse_name_status_z(stdout: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(stdout);
    let mut fields = text.split('\0');
    let mut paths = Vec::new();
    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue; // trailing NUL after the last record
        }
        let Some(path) = fields.next() else { break };
        if status.starts_with('R') || status.starts_with('C') {
            let Some(dest) = fields.next() else { break };
            paths.push(dest.to_string());
        } else {
            paths.push(path.to_string());
        }
    }
    paths
}

/// The gate's own success line — names the derived type, the tier that
/// supplied it, and the base it derived against, so a green result is
/// readable rather than bare (`empty-gates.md`'s thesis).
fn describe_success(
    derivation: &Derivation,
    branch: &str,
    base_sha: &str,
    fragments: &[AddedFragment],
) -> String {
    let supplier = match derivation.supplier {
        Supplier::Fragment => {
            let path = fragments
                .iter()
                .find(|f| suffix_to_work_type(&f.suffix) == Some(derivation.work_type))
                .map(|f| f.path.as_str())
                .unwrap_or("?");
            format!("changelog fragment '{path}'")
        }
        Supplier::BranchPrefix => {
            let prefix = branch.split_once('/').map_or(branch, |(p, _)| p);
            format!("branch prefix '{prefix}/'")
        }
    };
    format!(
        "work type '{}' from {supplier}, base {base_sha}, {RULE_COUNT} rules",
        derivation.work_type
    )
}

/// Run `git <args>` in `dir`, collapsing failure to a single message —
/// [`self_test`]'s own scratch-repo setup, not the thing under test.
///
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` are pinned to `/dev/null` (S4): a
/// developer's own `~/.gitconfig` — `commit.gpgsign = true`, a global
/// `core.hooksPath`, an `init.templateDir` — would otherwise apply inside
/// these scratch repos too and make `--self-test` fail (or run hooks) for
/// reasons having nothing to do with the gate. CI carries neither file, so
/// this is a no-op there; it only isolates the local-invocation property
/// `CONTRIBUTING.md:55` protects. Each repo's own local `user.email`/
/// `user.name` (set immediately after `git init` below) is unaffected — it
/// lives in the scratch repo's own `.git/config`, never in the global/system
/// files this points away from.
fn run_git(args: &[&str], dir: &Path) -> Result<(), String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .map_err(|e| format!("invoke git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// One `--self-test` case's outcome against the *production* pipeline
/// ([`resolve_base`] + [`collect_added_fragments`] + [`derive_work_type`] +
/// [`check_rule`]) — distinct from [`WorkTypeError`]/[`RuleCheckError`] so a
/// case can also fail on the successful-but-wrong-reason path without
/// forcing every case's caller to match both error enums.
enum PipelineError {
    Derivation(WorkTypeError),
    Rule(RuleCheckError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineError::Derivation(e) => write!(f, "{e}"),
            PipelineError::Rule(e) => write!(f, "{e}"),
        }
    }
}

/// Run the real production pipeline against `dir`'s already-checked-out
/// `branch`. `branch` is passed explicitly rather than read via
/// [`current_branch`] — a `--self-test` run in CI inherits `GITHUB_HEAD_REF`/
/// `GITHUB_REF_NAME` from the *real* PR it is running inside, and
/// `current_branch` prefers those over the scratch repo's own checkout, so
/// calling it here would silently test the wrong branch.
fn run_pipeline_for_self_test(dir: &Path, branch: &str) -> Result<Derivation, PipelineError> {
    let base_sha = resolve_base(Some("origin/main"), dir).map_err(PipelineError::Derivation)?;
    let fragments = collect_added_fragments(dir, &base_sha)
        .map_err(|e| PipelineError::Rule(RuleCheckError::Collection(e)))?;
    let derivation = derive_work_type(&fragments, branch).map_err(PipelineError::Derivation)?;
    check_rule(&derivation, dir, &base_sha, &fragments).map_err(PipelineError::Rule)?;
    Ok(derivation)
}

/// Shared scratch-repo bootstrap for every `--self-test` case below: init,
/// set a throwaway identity, commit `README.md`, and fake `origin/main` at
/// that commit so [`resolve_base`] succeeds — each case's violation lives in
/// what it commits *after* this, never in whether the repo has a resolvable
/// base (that is E1's separately-pinned case).
fn init_self_test_repo(dir: &Path) -> Result<(), String> {
    run_git(&["init", "-q"], dir)?;
    run_git(
        &[
            "config",
            "user.email",
            "work-type-self-test@example.invalid",
        ],
        dir,
    )?;
    run_git(&["config", "user.name", "work-type-check --self-test"], dir)?;
    std::fs::write(
        dir.join("README.md"),
        "work-type-check self-test scratch repo\n",
    )
    .map_err(|e| format!("write README.md: {e}"))?;
    run_git(&["add", "README.md"], dir)?;
    run_git(&["commit", "-q", "-m", "base"], dir)?;
    run_git(&["update-ref", "refs/remotes/origin/main", "HEAD"], dir)?;
    Ok(())
}

/// `--self-test`: construct a genuinely violating case **per rule** (R0
/// through R4) and assert the real production pipeline rejects each one for
/// its *specific* reason — not just any `Err`. Follows
/// `scripts/check-symlinks.sh --self-test` in shape: the same code paths are
/// shown failing on broken cases immediately before the real invocation runs
/// (`ci.yml:325-326`'s pattern for `work-type-check`).
///
/// E5: none of these may decay into `assert!(derive("").is_err())` — each
/// case matches a *specific* [`PipelineError`] variant, so a wrong-reason
/// rejection reports FAILED rather than passing vacuously. **Only
/// `self_test_r0` additionally checks its own precondition** (that its
/// scratch branch still carries no recognized prefix) before trusting the
/// result; R1–R4 rely on the variant match above for their protection
/// instead. That is real and it is not nothing — a break in one rule's
/// extraction glue surfaces as the *wrong* variant, not a silent pass — but
/// it is weaker than R0's belt-and-braces check, and it does not mean each
/// case isolates exactly the limb it names (see B4/R1's own fix, below, for
/// a case that did not).
fn self_test() -> ExitCode {
    let cases: [fn() -> Result<String, String>; RULE_COUNT] = [
        self_test_r0,
        self_test_r1,
        self_test_r2,
        self_test_r3,
        self_test_r4,
    ];

    let mut all_ok = true;
    for case in cases {
        match case() {
            Ok(msg) => println!("work-type-check --self-test: ok ({msg})"),
            Err(msg) => {
                eprintln!("work-type-check --self-test: FAILED — {msg}");
                all_ok = false;
            }
        }
    }

    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// R0: a diff adding no changelog fragment, on a branch carrying no
/// recognized work-type prefix, must be rejected as `NoSupplier` — the
/// vacuity guard (E3).
fn self_test_r0() -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("could not create a scratch dir: {e}"))?;
    let dir = tmp.path();
    init_self_test_repo(dir)?;

    let branch = "self-test-r0-no-supplier";
    run_git(&["checkout", "-q", "-b", branch], dir)?;
    // N6: a real change with no fragment and no branch prefix — an empty
    // diff is a degenerate stand-in for the realistic violation this rule
    // actually defends against ("real changes shipped with neither
    // supplier"), not a proof of it.
    std::fs::write(
        dir.join("stray.txt"),
        "R0 self-test: a real change with no changelog fragment and no branch prefix.\n",
    )
    .map_err(|e| format!("write stray.txt: {e}"))?;
    run_git(&["add", "stray.txt"], dir)?;
    run_git(
        &[
            "commit",
            "-q",
            "-m",
            "r0 self-test: real change, no fragment",
        ],
        dir,
    )?;

    if branch_prefix_to_work_type(branch).is_some() {
        return Err(format!(
            "R0: scratch branch {branch:?} now carries a recognized work-type prefix; this no \
             longer builds a violating case"
        ));
    }

    match run_pipeline_for_self_test(dir, branch) {
        Err(PipelineError::Derivation(WorkTypeError::NoSupplier { .. })) => Ok(
            "R0: a diff with no added changelog fragment and an unprefixed branch was \
             correctly rejected as NoSupplier"
                .to_string(),
        ),
        Ok(derivation) => Err(format!(
            "R0: the violating case was accepted as {derivation:?} instead of being rejected"
        )),
        Err(other) => Err(format!(
            "R0: the violating case was rejected, but not for the NoSupplier reason: {other}"
        )),
    }
}

/// R1: a `doc`-typed diff (supplied by the `docs/` branch prefix) that adds
/// a `#[spec(` occurrence must be rejected as `DocAddsSpecTest` — the narrow
/// negative limb.
///
/// **B4 fix:** the fixture's `#[spec(` line lives in `src/`, not `tests/`,
/// and the diff also adds a `docs/` page — so this trips exactly the one
/// limb it names (`adds_spec_attr`) rather than all three of R1's limbs at
/// once (`touches_tests` would also fire from a `tests/` fixture, and
/// `touches_doc_paths` would stay false with no `docs/` page in the diff at
/// all). Before this fix the case only "passed" because `check_r1_doc`
/// happens to test `adds_spec_attr` first — it could not tell "this limb
/// works" apart from "some limb works." B3's fix (restricting the
/// `adds_spec_attr` search to `.rs` files) is why the fixture must stay a
/// `.rs` file rather than moving to Markdown.
fn self_test_r1() -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("could not create a scratch dir: {e}"))?;
    let dir = tmp.path();
    init_self_test_repo(dir)?;

    let branch = "docs/999001-self-test-r1";
    run_git(&["checkout", "-q", "-b", branch], dir)?;
    std::fs::create_dir_all(dir.join("src")).map_err(|e| format!("mkdir src: {e}"))?;
    std::fs::create_dir_all(dir.join("docs")).map_err(|e| format!("mkdir docs: {e}"))?;
    std::fs::write(
        dir.join("src/self_test_r1_fixture.rs"),
        "#[spec(\"fixture/self-test/r1\")]\nfn placeholder() {}\n",
    )
    .map_err(|e| format!("write src/self_test_r1_fixture.rs: {e}"))?;
    std::fs::write(
        dir.join("docs/self-test-r1.md"),
        "R1 self-test fixture doc page — supplies the positive doc-paths limb.\n",
    )
    .map_err(|e| format!("write docs/self-test-r1.md: {e}"))?;
    run_git(
        &["add", "src/self_test_r1_fixture.rs", "docs/self-test-r1.md"],
        dir,
    )?;
    run_git(&["commit", "-q", "-m", "r1 self-test violation"], dir)?;

    match run_pipeline_for_self_test(dir, branch) {
        Err(PipelineError::Rule(RuleCheckError::Violation(RuleViolation::DocAddsSpecTest))) => Ok(
            "R1: a 'doc' diff adding a #[spec( occurrence was correctly rejected as \
                DocAddsSpecTest"
                .to_string(),
        ),
        Ok(derivation) => Err(format!(
            "R1: the violating case was accepted as {derivation:?} instead of being rejected"
        )),
        Err(other) => Err(format!(
            "R1: the violating case was rejected, but not for the DocAddsSpecTest reason: {other}"
        )),
    }
}

/// R2: a `chore`-typed diff (supplied by the `chore/` branch prefix) that
/// adds a new CLI flag must be rejected as `ChoreAddsCliFlag` — a chore must
/// never add user-facing surface.
fn self_test_r2() -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("could not create a scratch dir: {e}"))?;
    let dir = tmp.path();
    init_self_test_repo(dir)?;

    let branch = "chore/999002-self-test-r2";
    run_git(&["checkout", "-q", "-b", branch], dir)?;
    std::fs::create_dir_all(dir.join("src")).map_err(|e| format!("mkdir src: {e}"))?;
    std::fs::write(
        dir.join("src/self_test_r2_fixture.rs"),
        "#[arg(long = \"self_test_flag\")]\npub flag: bool,\n",
    )
    .map_err(|e| format!("write src/self_test_r2_fixture.rs: {e}"))?;
    run_git(&["add", "src/self_test_r2_fixture.rs"], dir)?;
    run_git(&["commit", "-q", "-m", "r2 self-test violation"], dir)?;

    match run_pipeline_for_self_test(dir, branch) {
        Err(PipelineError::Rule(RuleCheckError::Violation(RuleViolation::ChoreAddsCliFlag))) => Ok(
            "R2: a 'chore' diff adding a CLI flag was correctly rejected as \
                ChoreAddsCliFlag"
                .to_string(),
        ),
        Ok(derivation) => Err(format!(
            "R2: the violating case was accepted as {derivation:?} instead of being rejected"
        )),
        Err(other) => Err(format!(
            "R2: the violating case was rejected, but not for the ChoreAddsCliFlag reason: {other}"
        )),
    }
}

/// R3: a `bug`-typed diff (supplied by the `fix/` branch prefix) with no
/// `#[spec]` test delta and no fragment (so no `No-Test:` escape hatch
/// either) must be rejected as `BugMissingSpecTestDelta`.
fn self_test_r3() -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("could not create a scratch dir: {e}"))?;
    let dir = tmp.path();
    init_self_test_repo(dir)?;

    let branch = "fix/999003-self-test-r3";
    run_git(&["checkout", "-q", "-b", branch], dir)?;
    std::fs::write(
        dir.join("README.md"),
        "work-type-check self-test scratch repo (r3 change)\n",
    )
    .map_err(|e| format!("rewrite README.md: {e}"))?;
    run_git(&["add", "README.md"], dir)?;
    run_git(&["commit", "-q", "-m", "r3 self-test violation"], dir)?;

    match run_pipeline_for_self_test(dir, branch) {
        Err(PipelineError::Rule(RuleCheckError::Violation(
            RuleViolation::BugMissingSpecTestDelta,
        ))) => Ok(
            "R3: a 'bug' diff with no #[spec] test delta and no No-Test: escape hatch was \
             correctly rejected as BugMissingSpecTestDelta"
                .to_string(),
        ),
        Ok(derivation) => Err(format!(
            "R3: the violating case was accepted as {derivation:?} instead of being rejected"
        )),
        Err(other) => Err(format!(
            "R3: the violating case was rejected, but not for the BugMissingSpecTestDelta \
             reason: {other}"
        )),
    }
}

/// R4: a `.feature.md` fragment whose numeric stem has no matching `prds/`
/// file anywhere on the filesystem must be rejected as `PrdNoMatchingFile`.
fn self_test_r4() -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("could not create a scratch dir: {e}"))?;
    let dir = tmp.path();
    init_self_test_repo(dir)?;

    let branch = "feat/999004-self-test-r4";
    run_git(&["checkout", "-q", "-b", branch], dir)?;
    std::fs::create_dir_all(dir.join("changelog.d"))
        .map_err(|e| format!("mkdir changelog.d: {e}"))?;
    std::fs::write(
        dir.join("changelog.d/999004.feature.md"),
        "Self-test fixture fragment; deliberately has no matching prds/ file.\n",
    )
    .map_err(|e| format!("write changelog.d/999004.feature.md: {e}"))?;
    run_git(&["add", "changelog.d/999004.feature.md"], dir)?;
    run_git(&["commit", "-q", "-m", "r4 self-test violation"], dir)?;

    match run_pipeline_for_self_test(dir, branch) {
        Err(PipelineError::Rule(RuleCheckError::Violation(RuleViolation::PrdNoMatchingFile {
            ..
        }))) => Ok(
            "R4: a 'prd' fragment with no matching prds/ file was correctly rejected as \
             PrdNoMatchingFile"
                .to_string(),
        ),
        Ok(derivation) => Err(format!(
            "R4: the violating case was accepted as {derivation:?} instead of being rejected"
        )),
        Err(other) => Err(format!(
            "R4: the violating case was rejected, but not for the PrdNoMatchingFile reason: \
             {other}"
        )),
    }
}

// ---------------------------------------------------------------------------
// M4 — rules R1-R4 (PRD fork#340). R0 above is the spine and is unaffected.
//
// Each rule is a pure function over a small, directly-constructible struct —
// the same shape as `derive_work_type` taking `&[AddedFragment]` rather than
// reading git itself. The git-diff-to-struct extraction glue that fills in
// these booleans from a real diff (`collect_doc_diff`, `collect_chore_diff`,
// `collect_bug_diff`, `check_prd_fragments`, further down) is untested by
// Tier A by design — the PRD's tester round deliberately left it unstubbed
// so an invented signature would not need unwinding. `check_rule` wires it
// into `run_in`'s pipeline.
// ---------------------------------------------------------------------------

/// Why R1-R4 rejected a diff. Kept as one enum (rather than one per rule)
/// because callers that reject a whole PR want to match across rules
/// uniformly, the same reasoning `WorkTypeError` already applies to R0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleViolation {
    /// R1 (`doc`): the diff added a `#[spec(` — a synthetic test, which
    /// `doc` work must never ship (that is what `bug`/`prd` fragments are
    /// for). The *narrow negative* limb of the refined rule.
    DocAddsSpecTest,
    /// R1 (`doc`): the diff changed something under `tests/**`. The other
    /// half of the narrow negative limb.
    DocTouchesTests,
    /// R1 (`doc`): the diff touched none of `docs/**`, `site/**`, a root
    /// `*.md`, or `prds/**` — the *positive* limb. Catches a feature shipped
    /// as `doc`, which produces no version bump at all.
    DocMissingDocPaths,
    /// R2 (`chore`): the diff added a new CLI flag (`#[arg(long = "`).
    ChoreAddsCliFlag,
    /// R2 (`chore`): the diff added a new `Commands::` variant.
    ChoreAddsCommandVariant,
    /// R2 (`chore`): the diff added a *new* page under `docs/`.
    ChoreAddsNewDocsPage,
    /// R3 (`bug`): the diff added or modified no `#[spec]`-annotated test
    /// (body-fingerprinted, so whitespace does not count), and no
    /// `No-Test: <reason>` escape hatch with a non-empty reason was present
    /// in the fragment either.
    BugMissingSpecTestDelta,
    /// R4 (`prd`): E9 — the fragment's stem is not numeric. M4's decision:
    /// a numeric stem is required for `feature`/`breaking` fragments (see
    /// [`check_r4_prd`]'s doc comment for the rationale).
    PrdNonNumericStem { fragment_path: String, stem: String },
    /// R4 (`prd`): no `prds/<stem>-*.md`, `prds/fork-<stem>-*.md`, or
    /// either under `prds/done/` exists on the filesystem — checked by
    /// existence, never by diff membership (a PRD spans many PRs, and only
    /// the first touches the file).
    PrdNoMatchingFile { fragment_path: String, stem: String },
}

impl fmt::Display for RuleViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleViolation::DocAddsSpecTest => write!(
                f,
                "R1: a 'doc' change must not add a #[spec(...)] test — that is bug/prd work"
            ),
            RuleViolation::DocTouchesTests => {
                write!(f, "R1: a 'doc' change must not touch tests/**")
            }
            RuleViolation::DocMissingDocPaths => write!(
                f,
                "R1: a 'doc' change must touch docs/**, site/**, a root *.md, or prds/**"
            ),
            RuleViolation::ChoreAddsCliFlag => write!(
                f,
                "R2: a 'chore' change must not add a new CLI flag (#[arg(long = \"...)"
            ),
            RuleViolation::ChoreAddsCommandVariant => write!(
                f,
                "R2: a 'chore' change must not add a new Commands:: variant"
            ),
            RuleViolation::ChoreAddsNewDocsPage => {
                write!(
                    f,
                    "R2: a 'chore' change must not add a new page under docs/"
                )
            }
            RuleViolation::BugMissingSpecTestDelta => write!(
                f,
                "R3: a 'bug' change must add or modify at least one #[spec] test, or carry a \
                 No-Test: <reason> line in its fragment"
            ),
            RuleViolation::PrdNonNumericStem {
                fragment_path,
                stem,
            } => write!(
                f,
                "R4: {fragment_path} has non-numeric stem {stem:?} — a numeric stem is \
                 required so it can be matched against prds/<n>-*.md"
            ),
            RuleViolation::PrdNoMatchingFile {
                fragment_path,
                stem,
            } => write!(
                f,
                "R4: {fragment_path} has no matching prds/{stem}-*.md, prds/fork-{stem}-*.md, \
                 or either under prds/done/"
            ),
        }
    }
}

/// R1's inputs — booleans over the diff, decoupled from how they were
/// computed (see the module-level note above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocDiff {
    /// The diff touches `docs/**`, `site/**`, a root `*.md`, or `prds/**`.
    pub touches_doc_paths: bool,
    /// The diff added a `#[spec(` occurrence anywhere.
    pub adds_spec_attr: bool,
    /// The diff changed something under `tests/**`.
    pub touches_tests: bool,
}

/// R1 — refined: rustdoc lives in `src/` and is genuinely edited as doc
/// work, so "zero diff under `src/`" was rejected. Instead: must touch one
/// of `docs/**`/`site/**`/root `*.md`/`prds/**` (positive), and must not add
/// a `#[spec(` or touch `tests/**` (narrow negative).
pub fn check_r1_doc(diff: &DocDiff) -> Result<(), RuleViolation> {
    if diff.adds_spec_attr {
        return Err(RuleViolation::DocAddsSpecTest);
    }
    if diff.touches_tests {
        return Err(RuleViolation::DocTouchesTests);
    }
    if !diff.touches_doc_paths {
        return Err(RuleViolation::DocMissingDocPaths);
    }
    Ok(())
}

/// R2's inputs — booleans over the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChoreDiff {
    /// The diff added a new CLI flag (`#[arg(long = "`).
    pub adds_cli_flag: bool,
    /// The diff added a new `Commands::` variant.
    pub adds_command_variant: bool,
    /// The diff added a *new* page under `docs/`.
    pub adds_new_docs_page: bool,
}

/// R2 — refined: `test:` is 43 of the last 400 commits and a pure-chore
/// sweep can touch all of `tests/`, so "zero diff under `tests/`" was
/// rejected outright. Instead: must not add a new user-facing CLI surface.
pub fn check_r2_chore(diff: &ChoreDiff) -> Result<(), RuleViolation> {
    if diff.adds_cli_flag {
        return Err(RuleViolation::ChoreAddsCliFlag);
    }
    if diff.adds_command_variant {
        return Err(RuleViolation::ChoreAddsCommandVariant);
    }
    if diff.adds_new_docs_page {
        return Err(RuleViolation::ChoreAddsNewDocsPage);
    }
    Ok(())
}

/// Whether this diff adds or modifies (by body fingerprint) at least one
/// `#[spec]`-annotated test — pure reuse of `list_tests`'s Created/Modified
/// machinery (`compute_created`/`compute_modified`), which is exactly what
/// the PRD calls for rather than a new mechanism.
///
/// A modification counts only when the fingerprinted body actually changed
/// — `compute_modified` also flags a Scenario-only edit, which must NOT
/// count here (prose, not a test), and a purely-whitespace body edit
/// leaves the token-stream fingerprint unchanged so it is already excluded
/// by construction. That is the mechanism that proves R3's "whitespace
/// does not count" claim rather than merely asserting it.
pub fn spec_test_delta(
    base: &BTreeMap<String, TestEntry>,
    head: &BTreeMap<String, TestEntry>,
) -> bool {
    if !crate::list_tests::compute_created(base, head).is_empty() {
        return true;
    }
    crate::list_tests::compute_modified(base, head)
        .iter()
        .any(|row| row.body_changed)
}

/// Parse a `No-Test: <reason>` directive out of a changelog fragment's file
/// content — R3's escape hatch, mirroring `m2.allowlist`'s
/// documented-exception pattern and deliberately kept visible in the
/// release-notes source. `None` when no line begins with `No-Test:`.
/// `Some(reason)` when one does, where `reason` is the trimmed text after
/// the colon — which may be empty; see [`check_r3_bug`] for what an empty
/// reason means for the escape hatch.
pub fn parse_no_test_directive(fragment_body: &str) -> Option<String> {
    fragment_body.lines().find_map(|line| {
        line.strip_prefix("No-Test:")
            .map(|rest| rest.trim().to_string())
    })
}

/// R3's inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugDiff {
    /// [`spec_test_delta`]'s result for this diff.
    pub spec_test_delta: bool,
    /// [`parse_no_test_directive`]'s result for the fragment this diff
    /// added, if any. `None` when no fragment was added or it carries no
    /// `No-Test:` line.
    pub no_test_reason: Option<String>,
}

/// R3 — sharpened: "a file under `tests/` changed" is too weak (a
/// whitespace edit to `CATALOG.md` satisfies it). Requires adding or
/// modifying at least one `#[spec]`-annotated test, body-fingerprinted so
/// whitespace does not count — the mechanical form of the existing
/// `reproduce-first` skill, not a new demand.
///
/// **Decision (M4 tester call): an empty `No-Test:` reason does NOT count
/// as the escape hatch.** The whole point of the directive is that it is
/// "deliberately kept visible in the release-notes source" so a human
/// reading the changelog can see *why* a bug shipped untested — an empty
/// reason gives that reader nothing, and would make `No-Test:` an
/// unaccountable bypass string rather than a documented exception. So
/// `no_test_reason: Some(reason)` only satisfies R3 when `reason`, trimmed,
/// is non-empty.
pub fn check_r3_bug(diff: &BugDiff) -> Result<(), RuleViolation> {
    if diff.spec_test_delta {
        return Ok(());
    }
    if let Some(reason) = &diff.no_test_reason
        && !reason.trim().is_empty()
    {
        return Ok(());
    }
    Err(RuleViolation::BugMissingSpecTestDelta)
}

/// The fragment's stem — the portion of the filename before the first `.`,
/// e.g. `"341"` for `changelog.d/341.feature.md`, or `"clickable-hyperlinks"`
/// for the real historical fragment `changelog.d/clickable-hyperlinks.feature.md`
/// (E9: a `.feature.md` with a non-numeric stem — seven historical fragments
/// used this shape). `None` when `path`'s file name carries no `.` at all.
pub fn fragment_stem(path: &str) -> Option<&str> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    if !file_name.contains('.') {
        return None;
    }
    file_name.split('.').next()
}

/// Whether a `prds/` file matches fragment stem `stem` —
/// `prds/<stem>-*.md`, `prds/fork-<stem>-*.md`, or either under
/// `prds/done/`. Existence on the filesystem, never diff membership — R4's
/// whole point: a PRD spans many milestones and many PRs, and only the
/// first touches the file, so requiring the file *in the diff* would fail
/// most legitimate PRD PRs.
pub fn matching_prds_file_exists(prds_dir: &Path, stem: &str) -> Result<bool, String> {
    let prefixes = [format!("{stem}-"), format!("fork-{stem}-")];
    if dir_has_prefixed_md_file(prds_dir, &prefixes)? {
        return Ok(true);
    }
    dir_has_prefixed_md_file(&prds_dir.join("done"), &prefixes)
}

/// Whether `dir` (if it exists) directly contains a `.md` file whose name
/// starts with one of `prefixes` — [`matching_prds_file_exists`]'s shared
/// scan, run once against `prds/` and once against `prds/done/`.
fn dir_has_prefixed_md_file(dir: &Path, prefixes: &[String]) -> Result<bool, String> {
    if !dir.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read_dir entry in {}: {e}", dir.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.ends_with(".md") && prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// R4 — refined: check existence on the filesystem (via
/// [`matching_prds_file_exists`]), not membership of the diff. A
/// `.feature.md`/`.breaking.md` fragment named `changelog.d/<stem>.*` must
/// have a matching `prds/<stem>-*.md`, `prds/fork-<stem>-*.md`, or either
/// under `prds/done/`.
///
/// **Decision (M4 tester call, E9): a numeric stem is required.** The PRD
/// records this as free right now because `changelog.d/` carries no fragment
/// that would need migrating, and it is the recommended direction — every
/// `prds/` naming convention (`prds/<n>-*.md`, `prds/fork-<n>-*.md`) is
/// keyed on the issue number, so a non-numeric stem can never be matched
/// against one by construction; asserting that explicitly here (rather than
/// leaving it to fall out of `matching_prds_file_exists` returning `false`)
/// makes E9's failure name itself instead of reading as an ordinary
/// no-match.
pub fn check_r4_prd(
    fragment_path: &str,
    stem: &str,
    prds_file_exists: bool,
) -> Result<(), RuleViolation> {
    if stem.is_empty() || !stem.chars().all(|c| c.is_ascii_digit()) {
        return Err(RuleViolation::PrdNonNumericStem {
            fragment_path: fragment_path.to_string(),
            stem: stem.to_string(),
        });
    }
    if !prds_file_exists {
        return Err(RuleViolation::PrdNoMatchingFile {
            fragment_path: fragment_path.to_string(),
            stem: stem.to_string(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The git-diff-to-struct extraction glue — untested by Tier A by design (see
// the module-level note above `RuleViolation`). Walks the actual diff
// between `base_sha` and `HEAD` in `repo_dir` to fill in each rule's input
// struct, then dispatches to the matching `check_r*` function.
// ---------------------------------------------------------------------------

/// Either a rule genuinely rejected the diff, or the glue could not even
/// compute the rule's inputs (a `git` invocation failed, a file could not be
/// read). Kept distinct from [`RuleViolation`] so a caller — and
/// `--self-test`'s per-rule cases — can tell "the diff is bad" apart from
/// "something broke while checking it".
#[derive(Debug)]
enum RuleCheckError {
    Violation(RuleViolation),
    Collection(String),
}

impl From<RuleViolation> for RuleCheckError {
    fn from(v: RuleViolation) -> Self {
        RuleCheckError::Violation(v)
    }
}

impl From<String> for RuleCheckError {
    fn from(s: String) -> Self {
        RuleCheckError::Collection(s)
    }
}

impl fmt::Display for RuleCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleCheckError::Violation(v) => write!(f, "{v}"),
            RuleCheckError::Collection(s) => {
                write!(f, "could not evaluate the rule for this diff: {s}")
            }
        }
    }
}

/// Dispatch to whichever rule matches `derivation.work_type`, having walked
/// the real diff between `base_sha` and `HEAD` in `repo_dir` to build that
/// rule's input struct. `fragments` is R0's own already-collected added
/// fragments, reused here rather than re-walking `changelog.d`.
fn check_rule(
    derivation: &Derivation,
    repo_dir: &Path,
    base_sha: &str,
    fragments: &[AddedFragment],
) -> Result<(), RuleCheckError> {
    match derivation.work_type {
        WorkType::Doc => {
            let diff = collect_doc_diff(repo_dir, base_sha)?;
            check_r1_doc(&diff)?;
        }
        WorkType::Chore => {
            let diff = collect_chore_diff(repo_dir, base_sha)?;
            check_r2_chore(&diff)?;
        }
        WorkType::Bug => {
            let diff = collect_bug_diff(repo_dir, base_sha, fragments)?;
            check_r3_bug(&diff)?;
        }
        WorkType::Prd => check_prd_fragments(repo_dir, fragments)?,
    }
    Ok(())
}

/// Every path changed (any status) between `base_sha` and `HEAD`. `-z`
/// (F1) so a non-ASCII path is not C-quoted by `core.quotePath` — a quoted
/// path begins and ends with `"`, so every `starts_with`/`ends_with` check
/// downstream (`is_doc_path`, `is_test_path`) would otherwise silently fail
/// to match and the file would fall out of R1 invisibly.
fn changed_files(repo_dir: &Path, base_sha: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["diff", "-z", "--name-only", base_sha, "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git diff --name-only {base_sha} HEAD: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

/// Every path newly *added* between `base_sha` and `HEAD` — R2's "new page
/// under docs/" must not count a file that already existed and was merely
/// edited. `-z` (F1), same reasoning and same NUL-safe parse as
/// [`collect_added_fragments`].
fn added_files(repo_dir: &Path, base_sha: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args([
            "diff",
            "-z",
            "--name-status",
            "--diff-filter=A",
            base_sha,
            "HEAD",
        ])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| {
            format!("invoke git diff --name-status --diff-filter=A {base_sha} HEAD: {e}")
        })?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(parse_name_status_z(&out.stdout))
}

/// Every line *added* by the diff between `base_sha` and `HEAD`, restricted
/// to hunks under `.rs` files (a unified-diff `+` line, excluding the `+++`
/// file header), concatenated — R1's `adds_spec_attr` and R2's
/// `adds_cli_flag` are both "did this diff add a line matching X" checks
/// over that text, and both only care about Rust *source*, never prose that
/// merely quotes the same syntax (B3: eight files on this branch alone quote
/// `#[spec(` in Markdown — `CLAUDE.md`, `CONTRIBUTING.md`, `prds/*.md` — all
/// on R1's positive `docs/**`/`prds/**` limb, so an unrestricted search
/// rejected an ordinary docs PR as `DocAddsSpecTest`).
fn added_diff_text(repo_dir: &Path, base_sha: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["diff", base_sha, "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git diff {base_sha} HEAD: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut in_rs_file = false;
    Ok(text
        .lines()
        .filter(|l| {
            if let Some(path) = l.strip_prefix("+++ b/") {
                in_rs_file = path.ends_with(".rs");
                return false;
            }
            if l.starts_with("+++ ") {
                // "+++ /dev/null" — a deleted file, never a `.rs` addition.
                in_rs_file = false;
                return false;
            }
            in_rs_file && l.starts_with('+') && !l.starts_with("+++")
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// R1's positive limb: `docs/**`, `site/**`, a root `*.md`, or `prds/**`.
fn is_doc_path(path: &str) -> bool {
    path.starts_with("docs/")
        || path.starts_with("site/")
        || path.starts_with("prds/")
        || (!path.contains('/') && path.ends_with(".md"))
}

/// R1's narrow negative limb, the `tests/**` half. Excludes `tests/**/*.md`
/// (S5): `tests/CATALOG.md` is prose documentation of the test suite, edited
/// as doc work, and R3's own design already treats a CATALOG.md edit as not
/// evidence of a test change ("a whitespace edit to CATALOG.md satisfies
/// it" is the exact weakness R3 was sharpened to reject) — counting it as
/// test-touching here would be inconsistent with that.
fn is_test_path(path: &str) -> bool {
    path.starts_with("tests/") && !path.ends_with(".md")
}

fn collect_doc_diff(repo_dir: &Path, base_sha: &str) -> Result<DocDiff, String> {
    let files = changed_files(repo_dir, base_sha)?;
    let touches_doc_paths = files.iter().any(|p| is_doc_path(p));
    let touches_tests = files.iter().any(|p| is_test_path(p));
    let adds_spec_attr = added_diff_text(repo_dir, base_sha)?.contains("#[spec(");
    Ok(DocDiff {
        touches_doc_paths,
        adds_spec_attr,
        touches_tests,
    })
}

fn collect_chore_diff(repo_dir: &Path, base_sha: &str) -> Result<ChoreDiff, String> {
    let adds_cli_flag = added_diff_text(repo_dir, base_sha)?.contains("#[arg(long = \"");
    let adds_command_variant = command_variant_added(repo_dir, base_sha)?;
    let adds_new_docs_page = added_files(repo_dir, base_sha)?
        .iter()
        .any(|p| p.starts_with("docs/") && p.ends_with(".md"));
    Ok(ChoreDiff {
        adds_cli_flag,
        adds_command_variant,
        adds_new_docs_page,
    })
}

/// Whether `src/main.rs`'s `enum Commands` gained a variant between
/// `base_sha` and `HEAD`. Compares the two refs' variant name *sets* rather
/// than scanning added diff lines for something variant-shaped, because a
/// line-based scan cannot tell a genuinely new variant apart from an added
/// doc comment or attribute on an existing one. A ref where `src/main.rs`
/// does not exist (or carries no `enum Commands`) yields an empty set rather
/// than an error — the only realistic case is a scratch repo that never had
/// the file, not a `git` failure worth propagating.
fn command_variant_added(repo_dir: &Path, base_sha: &str) -> Result<bool, String> {
    let base_source = git_show_in(repo_dir, base_sha, "src/main.rs").unwrap_or_default();
    let head_source = git_show_in(repo_dir, "HEAD", "src/main.rs").unwrap_or_default();
    let base_variants = extract_enum_variant_names(&base_source, "Commands");
    let head_variants = extract_enum_variant_names(&head_source, "Commands");
    Ok(head_variants.difference(&base_variants).next().is_some())
}

/// The names of `enum <enum_name> {`'s top-level variants in `source` —
/// e.g. `Hook`, `Hooks`, `Config`, ... for `src/main.rs`'s `enum Commands`.
/// Depth-tracks braces rather than parsing the enum with `syn`, since this
/// only needs variant *names*, not their field shapes.
fn extract_enum_variant_names(source: &str, enum_name: &str) -> BTreeSet<String> {
    let marker = format!("enum {enum_name} {{");
    let Some(start) = source.find(&marker) else {
        return BTreeSet::new();
    };
    let body = &source[start + marker.len()..];

    let mut names = BTreeSet::new();
    let mut depth = 1i32; // just inside the enum's own opening brace
    for line in body.lines() {
        if depth == 0 {
            break;
        }
        let trimmed = line.trim_start();
        if depth == 1
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with('/')
        {
            let name: String = trimmed
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                names.insert(name);
            }
        }
        for ch in line.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
    }
    names
}

fn collect_bug_diff(
    repo_dir: &Path,
    base_sha: &str,
    fragments: &[AddedFragment],
) -> Result<BugDiff, String> {
    let base_tests = collect_spec_sources_at(repo_dir, base_sha)?;
    let head_tests = collect_spec_sources_at(repo_dir, "HEAD")?;
    let delta = spec_test_delta(&base_tests, &head_tests);

    // N3: deliberately the working tree, not `git show HEAD:<path>` like
    // every other input in this module — `repo_dir` is always checked out
    // at `HEAD` in both CI and `--self-test`'s scratch repos, so the two
    // are identical in practice, and `.ok()?` fails *closed* on a missing
    // file (no escape hatch found, rather than one silently assumed).
    //
    // F2: also admit a `.breaking.md` fragment, not only `.bugfix.md`'s
    // exact-type match. This function only runs when `derive_work_type`
    // already resolved the diff to `WorkType::Bug` (`check_rule`'s dispatch
    // is the sole call site), so a `breaking`-suffixed fragment reaching
    // here can only be one the branch-aware carve-out re-resolved to `Bug`
    // — never a `feat/`-branch `.breaking.md`, which stays `Prd` and never
    // reaches `collect_bug_diff` at all. Without this, a breaking bug fix
    // that legitimately touches no `#[spec]` test has no fragment R3 will
    // read its `No-Test:` line from.
    let no_test_reason = fragments
        .iter()
        .filter(|f| {
            suffix_to_work_type(&f.suffix) == Some(WorkType::Bug) || f.suffix == BREAKING_SUFFIX
        })
        .find_map(|f| {
            let body = std::fs::read_to_string(repo_dir.join(&f.path)).ok()?;
            parse_no_test_directive(&body)
        });

    Ok(BugDiff {
        spec_test_delta: delta,
        no_test_reason,
    })
}

/// R4 only has something to check when a `feature`/`breaking` fragment was
/// actually added — a `prd` derivation supplied purely by the branch prefix
/// (tier 2, no fragment) has nothing on disk to validate against.
fn check_prd_fragments(repo_dir: &Path, fragments: &[AddedFragment]) -> Result<(), RuleCheckError> {
    let prds_dir = repo_dir.join("prds");
    for fragment in fragments {
        if suffix_to_work_type(&fragment.suffix) != Some(WorkType::Prd) {
            continue;
        }
        let Some(stem) = fragment_stem(&fragment.path) else {
            continue;
        };
        let stem = stem.to_string();
        let exists = matching_prds_file_exists(&prds_dir, &stem)?;
        check_r4_prd(&fragment.path, &stem, exists)?;
    }
    Ok(())
}

/// Repo-dir-parameterized sibling of `list_tests::collect_tests_at_ref` —
/// duplicated rather than reused because that function (and the `git_show`/
/// `git_ls_tree` it calls) always runs against the process's own cwd, with
/// no `repo_dir` parameter to thread through; widening that private,
/// well-tested function's signature was judged out of scope for glue this
/// round's tests do not exercise. [`spec_test_delta`] itself — the part the
/// PRD calls out by name — DOES reuse `list_tests::compute_created` /
/// `compute_modified` rather than reimplementing the delta logic.
fn collect_spec_sources_at(
    repo_dir: &Path,
    reference: &str,
) -> Result<BTreeMap<String, TestEntry>, String> {
    let mut sources: Vec<(String, String)> = Vec::new();
    for f in git_ls_tree_in(repo_dir, reference, "tests")? {
        if !f.ends_with(".rs") || f == "tests/common/mod.rs" {
            continue;
        }
        let body = git_show_in(repo_dir, reference, &f)?;
        sources.push((f, body));
    }
    for f in git_ls_tree_in(repo_dir, reference, "src")? {
        if !f.ends_with(".rs") {
            continue;
        }
        let body = git_show_in(repo_dir, reference, &f)?;
        if !body.contains("#[spec(") {
            continue;
        }
        sources.push((f, body));
    }
    crate::list_tests::collect_tests_from_sources(&sources)
}

fn git_ls_tree_in(repo_dir: &Path, reference: &str, path: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", reference, path])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git ls-tree {reference} {path}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-tree {reference} {path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect())
}

fn git_show_in(repo_dir: &Path, reference: &str, path: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["show", &format!("{reference}:{path}")])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("invoke git show {reference}:{path}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git show {reference}:{path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list_tests::collect_tests_from_sources;
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

    // -- `.breaking.md` is a severity axis, not a work type (fork#451) ------

    #[test]
    fn breaking_fragment_on_fix_branch_resolves_to_bug() {
        let fragments = [AddedFragment {
            path: "changelog.d/451.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        }];
        let derivation = derive_work_type(&fragments, "fix/451-thing")
            .expect("a breaking bugfix on a fix/ branch must not disagree");
        assert_eq!(
            derivation,
            Derivation {
                work_type: WorkType::Bug,
                // F1: the branch's Bug signal is what actually supplied
                // this, not the fragment (whose own mapping is Prd) — see
                // `describe_success_names_branch_prefix_for_the_breaking_carve_out`
                // for the success-line consequence of getting this wrong.
                supplier: Supplier::BranchPrefix,
            }
        );
    }

    #[test]
    fn breaking_fragment_on_feat_branch_still_resolves_to_prd() {
        // Regression coverage: unchanged from today — both tiers already
        // agree on Prd, so this never reaches the branch-aware fallback.
        let fragments = [AddedFragment {
            path: "changelog.d/451.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        }];
        let derivation = derive_work_type(&fragments, "feat/451-thing")
            .expect("a breaking feature on a feat/ branch must resolve");
        assert_eq!(
            derivation,
            Derivation {
                work_type: WorkType::Prd,
                supplier: Supplier::Fragment,
            }
        );
    }

    #[test]
    fn breaking_fragment_with_no_branch_signal_still_falls_back_to_prd() {
        // Regression coverage: preserve today's existing
        // single-fragment-wins behavior when the branch supplies nothing.
        let fragments = [AddedFragment {
            path: "changelog.d/451.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        }];
        let derivation = derive_work_type(&fragments, "spike/451-thing")
            .expect("a breaking fragment with no branch signal must fall back to Prd");
        assert_eq!(
            derivation,
            Derivation {
                work_type: WorkType::Prd,
                supplier: Supplier::Fragment,
            }
        );
    }

    #[test]
    fn breaking_fragment_on_docs_branch_still_disagrees() {
        // A breaking change on a docs/ branch is a genuine mismatch, not a
        // case the fix/ carve-out should silence.
        let fragments = [AddedFragment {
            path: "changelog.d/451.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        }];
        let err = derive_work_type(&fragments, "docs/451-thing")
            .expect_err("a breaking fragment on a docs/ branch must still disagree");
        match err {
            WorkTypeError::FragmentBranchDisagree { fragment, branch } => {
                assert_eq!(
                    fragment,
                    ("changelog.d/451.breaking.md".to_string(), WorkType::Prd)
                );
                assert_eq!(branch, ("docs/451-thing".to_string(), WorkType::Doc));
            }
            other => panic!("expected FragmentBranchDisagree, got {other:?}"),
        }
    }

    #[test]
    fn breaking_plus_feature_fragments_on_fix_branch_still_disagree_naming_the_feature() {
        // F3 + F4: a genuine `.feature.md` alongside a `.breaking.md` on a
        // `fix/` branch is a real feature, not a breaking bug fix — it must
        // still fail, and the error must name the `.feature.md` (the actual
        // offender), not the `.breaking.md` that sorts first and that this
        // very PR makes legal on its own. Asserting the `Err` variant here
        // (not just that *an* error occurs) is also what catches the
        // "any-suffix-seen sets fragment_all_breaking" mutation: under that
        // mutation this resolves to `Ok(Bug)` regardless of fragment order,
        // letting a genuine feature ship as a bug fix.
        let fragments = [
            AddedFragment {
                path: "changelog.d/451.breaking.md".to_string(),
                suffix: "breaking".to_string(),
            },
            AddedFragment {
                path: "changelog.d/451.feature.md".to_string(),
                suffix: "feature".to_string(),
            },
        ];
        let err = derive_work_type(&fragments, "fix/451-thing").expect_err(
            "a genuine feature alongside a breaking fragment must still disagree with fix/",
        );
        match err {
            WorkTypeError::FragmentBranchDisagree { fragment, branch } => {
                assert_eq!(
                    fragment,
                    ("changelog.d/451.feature.md".to_string(), WorkType::Prd),
                    "must name the .feature.md, not whichever fragment sorted first"
                );
                assert_eq!(branch, ("fix/451-thing".to_string(), WorkType::Bug));
            }
            other => panic!("expected FragmentBranchDisagree, got {other:?}"),
        }
    }

    // -- `.bugfix.md` + `.breaking.md` is a breaking bug fix, not a
    // fragment conflict (fork#453) -------------------------------------------
    //
    // The fork#451 carve-out above only fires once tier 1 has settled on a
    // single fragment-supplied type — but `.bugfix.md` (`Bug`) and
    // `.breaking.md` (`Prd`) map to *different* types, so the tier-1 loop's
    // own pairwise comparison (`fragment_supply` vs each next fragment)
    // raises `ConflictingFragments` before the branch-aware carve-out is
    // ever consulted. Currently `Err`; fixed, this must resolve to `Bug`
    // the same way a lone `.breaking.md` on a `fix/` branch already does
    // (`breaking_fragment_on_fix_branch_resolves_to_bug` above).
    //
    // N-fragment note: a third fragment such as `.feature.md` alongside
    // `.bugfix.md` + `.breaking.md` is deliberately left unpinned here.
    // `.feature.md` is a genuine, non-breaking `Prd` fragment sitting next
    // to a `Bug` fragment — a real conflict that must still error — but
    // which `WorkTypeError` variant it should report, and which fragments
    // it should name, depends on how the eventual fix restructures tier 1's
    // pairwise loop; asserting a specific shape now would pin an
    // implementation detail rather than an observable contract.

    #[test]
    fn bugfix_plus_breaking_fragments_on_fix_branch_resolve_to_bug() {
        // Scenario: a diff carries both a `.bugfix.md` and a `.breaking.md`
        // fragment on a `fix/` branch — a breaking bug fix, the same
        // combination fork#451's carve-out already resolves for a lone
        // `.breaking.md`. It must resolve to `Bug`, not fail as a fragment
        // conflict.
        let fragments = [
            AddedFragment {
                path: "changelog.d/453.bugfix.md".to_string(),
                suffix: "bugfix".to_string(),
            },
            AddedFragment {
                path: "changelog.d/453.breaking.md".to_string(),
                suffix: "breaking".to_string(),
            },
        ];
        let derivation = derive_work_type(&fragments, "fix/453-thing").expect(
            "a .bugfix.md alongside a .breaking.md on a fix/ branch is a breaking bug fix, \
             not a fragment conflict",
        );
        assert_eq!(
            derivation,
            Derivation {
                work_type: WorkType::Bug,
                // The branch's Bug signal is what actually resolves this,
                // matching `breaking_fragment_on_fix_branch_resolves_to_bug`.
                supplier: Supplier::BranchPrefix,
            }
        );
    }

    #[test]
    fn bugfix_plus_feature_fragments_still_conflict_regression_guard() {
        // Scenario: a diff carries a `.bugfix.md` and a `.feature.md`
        // fragment — a real two-type conflict, not the breaking-specific
        // carve-out fork#453 introduces. This must keep failing with
        // `ConflictingFragments` once that carve-out lands, proving the fix
        // does not overcorrect into accepting every fragment-type
        // disagreement.
        let fragments = [
            AddedFragment {
                path: "changelog.d/453.bugfix.md".to_string(),
                suffix: "bugfix".to_string(),
            },
            AddedFragment {
                path: "changelog.d/453.feature.md".to_string(),
                suffix: "feature".to_string(),
            },
        ];
        let err = derive_work_type(&fragments, "fix/453-thing").expect_err(
            "a genuine .feature.md alongside a .bugfix.md must still conflict — this is not \
             the breaking-specific carve-out",
        );
        match err {
            WorkTypeError::ConflictingFragments { first, second } => {
                assert_eq!(
                    first,
                    ("changelog.d/453.bugfix.md".to_string(), WorkType::Bug)
                );
                assert_eq!(
                    second,
                    ("changelog.d/453.feature.md".to_string(), WorkType::Prd)
                );
            }
            other => panic!("expected ConflictingFragments, got {other:?}"),
        }
    }

    #[test]
    fn feature_then_bugfix_fragments_still_conflict_guard_regression() {
        // Scenario: a `.feature.md` fragment appears before a `.bugfix.md`
        // in the pairwise scan (P3-2) — this pins the `!any_feature_fragment`
        // guard on the exemption's second disjunct. Without that guard,
        // `first_type == Prd && work_type == Bug` alone would wrongly treat
        // this as if it were the bugfix+breaking carve-out, even though a
        // genuine `.feature.md` is present. Must still hard-conflict, the
        // same as `bugfix_plus_feature_fragments_still_conflict_regression_guard`
        // above but with the fragments in the opposite order.
        let fragments = [
            AddedFragment {
                path: "changelog.d/453.feature.md".to_string(),
                suffix: "feature".to_string(),
            },
            AddedFragment {
                path: "changelog.d/453.bugfix.md".to_string(),
                suffix: "bugfix".to_string(),
            },
        ];
        let err = derive_work_type(&fragments, "fix/453-thing").expect_err(
            "a genuine .feature.md before a .bugfix.md must still conflict, even though the \
             pairwise types echo the bugfix+breaking exemption's shape",
        );
        match err {
            WorkTypeError::ConflictingFragments { first, second } => {
                assert_eq!(
                    first,
                    ("changelog.d/453.feature.md".to_string(), WorkType::Prd)
                );
                assert_eq!(
                    second,
                    ("changelog.d/453.bugfix.md".to_string(), WorkType::Bug)
                );
            }
            other => panic!("expected ConflictingFragments, got {other:?}"),
        }
    }

    #[test]
    fn breaking_then_bugfix_fragments_on_fix_branch_resolve_to_bug_production_order() {
        // Scenario: the same `.bugfix.md` + `.breaking.md` pair as
        // `bugfix_plus_breaking_fragments_on_fix_branch_resolve_to_bug`
        // above, but in the order production actually sees it (P3-3):
        // `collect_added_fragments` sorts fragments by path, and for a
        // single issue number `changelog.d/<n>.breaking.md` sorts before
        // `changelog.d/<n>.bugfix.md` ('r' < 'u') — this PR's own fragment
        // pair takes exactly this order in CI. Exercises the exemption's
        // *second* disjunct (`first_type == Prd && ... && work_type ==
        // Bug`), not the first.
        let fragments = [
            AddedFragment {
                path: "changelog.d/453.breaking.md".to_string(),
                suffix: "breaking".to_string(),
            },
            AddedFragment {
                path: "changelog.d/453.bugfix.md".to_string(),
                suffix: "bugfix".to_string(),
            },
        ];
        let derivation = derive_work_type(&fragments, "fix/453-thing").expect(
            "a .breaking.md followed by a .bugfix.md on a fix/ branch is a breaking bug fix, \
             not a fragment conflict",
        );
        assert_eq!(
            derivation,
            Derivation {
                work_type: WorkType::Bug,
                // The .bugfix.md fragment directly supplies Bug here — the
                // branch merely agrees, so Fragment is the more honest
                // supplier than BranchPrefix (reviewer P2-2).
                supplier: Supplier::Fragment,
            }
        );
    }

    #[test]
    fn bugfix_breaking_feature_three_fragment_permutations_all_conflict() {
        // Scenario: a diff carries all three of `.bugfix.md`, `.breaking.md`,
        // and `.feature.md` on a `fix/` branch — a real conflict in every
        // ordering (P3-4), since a genuine `.feature.md` can never be
        // exempted (`any_feature_fragment` is precomputed once over the
        // whole set before the loop). Asserts at minimum `is_err()` across
        // all six permutations, closing P3-2's guard as a side effect.
        let bugfix = AddedFragment {
            path: "changelog.d/453.bugfix.md".to_string(),
            suffix: "bugfix".to_string(),
        };
        let breaking = AddedFragment {
            path: "changelog.d/453.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        };
        let feature = AddedFragment {
            path: "changelog.d/453.feature.md".to_string(),
            suffix: "feature".to_string(),
        };
        let base = [bugfix, breaking, feature];
        const ORDERS: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for order in ORDERS {
            let fragments: Vec<AddedFragment> = order.iter().map(|&i| base[i].clone()).collect();
            let result = derive_work_type(&fragments, "fix/453-thing");
            assert!(
                result.is_err(),
                "expected an error for fragment order {order:?}, got {result:?}"
            );
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

        // B2: pass the scratch repo's own branch explicitly rather than
        // letting `run_in` fall back to `current_branch`, which prefers
        // `GITHUB_HEAD_REF`/`GITHUB_REF_NAME` — set from the *real* PR
        // whenever this runs in CI — over the checked-out branch here. Not
        // exempt, so it does not take the skip path before reaching
        // `resolve_base`.
        let branch = "e1-unresolvable-base-scratch-branch";
        git(&["checkout", "-q", "-b", branch], tmp.path());

        let code = run_in(&[], tmp.path(), Some(branch));
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

    // -- B1: the merged-base skip -----------------------------------------

    /// Scenario: builds a scratch repo where `origin/main` already includes
    /// `HEAD` — the exact shape of a post-merge `push: [main]` run — and
    /// asserts `run_in` treats it as nothing-to-classify rather than running
    /// derivation and failing `NoSupplier`.
    #[test]
    fn run_in_skips_when_the_resolved_base_equals_head() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        git(&["commit", "-q", "--allow-empty", "-m", "init"], tmp.path());
        // `resolve_base`'s default target is `origin/main` — fake it at
        // HEAD so `git merge-base HEAD origin/main` resolves to HEAD itself,
        // the same shape `fetch-depth: 0` + `push: [main]` produces.
        git(
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
            tmp.path(),
        );

        // "main" carries no work-type prefix (no '/'), so if the merged-base
        // skip did not fire, `derive_work_type` would run against an empty
        // fragment list and an unprefixed branch and fail as `NoSupplier` —
        // a real derivation can never succeed on this shape. `SUCCESS` is
        // therefore diagnostic of the skip specifically, not of some other
        // path happening to derive a type (B1's "not a derivation").
        let code = run_in(&[], tmp.path(), Some("main"));
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "a merged base (base == HEAD) must be treated as nothing-to-classify \
             and skipped, not run through derivation (B1) — every merge to main \
             would otherwise turn `push: [main]` red"
        );
    }

    // -- B2: the branch override is actually consulted ---------------------

    /// Scenario: sets `GITHUB_HEAD_REF` to an exempt branch name in the
    /// process environment, then calls `run_in` with a non-exempt
    /// `branch_override` against a scratch repo with no resolvable base, and
    /// asserts the pipeline used the override rather than the environment.
    #[test]
    fn run_in_uses_the_branch_override_not_github_head_ref() {
        // nextest gives each test its own process, so this env write is
        // contained to this test and cannot bleed into another.
        unsafe {
            std::env::set_var("GITHUB_HEAD_REF", "renovate/anything-i-like");
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        git(&["commit", "-q", "--allow-empty", "-m", "init"], tmp.path());
        let branch = "b2-branch-override-scratch-branch";
        git(&["checkout", "-q", "-b", branch], tmp.path());

        // No `origin/main` exists in this repo. If `run_in` read
        // `GITHUB_HEAD_REF` instead of the override, it would take the
        // exempt-branch skip *before* ever reaching `resolve_base` and
        // return `SUCCESS`. The override's branch is not exempt, so the
        // pipeline must instead reach `resolve_base`, fail to find
        // `origin/main`, and return `EXIT_BASE_UNRESOLVABLE`.
        let code = run_in(&[], tmp.path(), Some(branch));

        unsafe {
            std::env::remove_var("GITHUB_HEAD_REF");
        }

        assert_eq!(
            code,
            ExitCode::from(EXIT_BASE_UNRESOLVABLE),
            "branch_override must be used instead of GITHUB_HEAD_REF (B2) — if \
             the environment's exempt 'renovate/anything-i-like' had been read \
             instead, this would have taken the exemption skip and returned \
             SUCCESS before resolve_base ever ran"
        );
    }

    // -- F1: non-ASCII paths are not C-quoted under -z ----------------------

    /// Scenario: feeds `parse_name_status_z` the exact NUL-separated byte
    /// shape git emits for an `A` record with a non-ASCII path, and asserts
    /// the path comes back with no surrounding quotes or octal escapes.
    #[test]
    fn parse_name_status_z_handles_non_ascii_bytes_without_quoting() {
        // Under git's default (newline-separated) --name-status this path
        // would arrive as `"changelog.d/na\303\257ve.feature.md"` — quoted
        // and octal-escaped (`core.quotePath`, on by default). `-z` disables
        // that, so the raw UTF-8 bytes must come through untouched.
        let mut stdout = Vec::new();
        stdout.extend_from_slice("A\0changelog.d/naïve.feature.md\0".as_bytes());
        let paths = parse_name_status_z(&stdout);
        assert_eq!(paths, vec!["changelog.d/naïve.feature.md".to_string()]);
    }

    /// Scenario: builds a scratch repo, adds a changelog fragment whose
    /// filename contains a non-ASCII character, and asserts
    /// `collect_added_fragments` still recognises it rather than silently
    /// dropping it — the v0.24.3 "fragment silently ignored" invariant this
    /// gate exists to protect.
    #[test]
    fn collect_added_fragments_recognizes_a_non_ascii_fragment_filename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        std::fs::create_dir_all(tmp.path().join("changelog.d")).expect("mkdir changelog.d");
        git(&["commit", "-q", "--allow-empty", "-m", "base"], tmp.path());
        let base_sha = current_head_sha(tmp.path()).expect("base sha");

        std::fs::write(
            tmp.path().join("changelog.d/naïve.feature.md"),
            "Adds a naïve cache.\n",
        )
        .expect("write fragment");
        git(&["add", "changelog.d/naïve.feature.md"], tmp.path());
        git(&["commit", "-q", "-m", "add fragment"], tmp.path());

        let fragments = collect_added_fragments(tmp.path(), &base_sha).expect("collect fragments");
        assert_eq!(
            fragments,
            vec![AddedFragment {
                path: "changelog.d/naïve.feature.md".to_string(),
                suffix: "feature".to_string(),
            }],
            "a fragment with a non-ASCII filename must still be recognised, not \
             silently dropped (F1)"
        );
    }

    // -- F2: a renamed fragment counts ---------------------------------------

    /// Scenario: feeds `parse_name_status_z` a rename record (three
    /// NUL-separated fields: status, source, destination) and asserts it
    /// resolves to the destination path alone.
    #[test]
    fn parse_name_status_z_resolves_a_rename_record_to_the_destination_path() {
        let mut stdout = Vec::new();
        stdout.extend_from_slice(b"R100\0changelog.d/194.bugfix.md\0changelog.d/500.feature.md\0");
        let paths = parse_name_status_z(&stdout);
        assert_eq!(paths, vec!["changelog.d/500.feature.md".to_string()]);
    }

    /// Scenario: feeds `parse_name_status_z` a rename record immediately
    /// followed by a plain `A` record, and asserts the second record parses
    /// correctly — a parser that mis-split the rename's three fields would
    /// drift the whole record stream by one field for everything after it.
    #[test]
    fn parse_name_status_z_does_not_desync_after_a_rename_record() {
        let mut stdout = Vec::new();
        stdout.extend_from_slice(b"R100\0changelog.d/194.bugfix.md\0changelog.d/500.feature.md\0");
        stdout.extend_from_slice(b"A\0changelog.d/501.doc.md\0");
        let paths = parse_name_status_z(&stdout);
        assert_eq!(
            paths,
            vec![
                "changelog.d/500.feature.md".to_string(),
                "changelog.d/501.doc.md".to_string(),
            ]
        );
    }

    /// Scenario: builds a scratch repo, commits a fragment, then `git mv`s
    /// it to a new stem/suffix in a second commit, and asserts
    /// `collect_added_fragments` sees the fragment at its destination path —
    /// `--diff-filter=A` alone would miss it, since git reports the rename
    /// as `R100`, not `A`.
    #[test]
    fn collect_added_fragments_counts_a_fragment_introduced_by_rename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        std::fs::create_dir_all(tmp.path().join("changelog.d")).expect("mkdir changelog.d");
        std::fs::write(tmp.path().join("changelog.d/194.bugfix.md"), "orig\n")
            .expect("write fragment");
        git(&["add", "changelog.d/194.bugfix.md"], tmp.path());
        git(&["commit", "-q", "-m", "base"], tmp.path());
        let base_sha = current_head_sha(tmp.path()).expect("base sha");

        git(
            &[
                "mv",
                "changelog.d/194.bugfix.md",
                "changelog.d/500.feature.md",
            ],
            tmp.path(),
        );
        git(
            &["commit", "-q", "-m", "rename fragment suffix"],
            tmp.path(),
        );

        let fragments = collect_added_fragments(tmp.path(), &base_sha).expect("collect fragments");
        assert_eq!(
            fragments,
            vec![AddedFragment {
                path: "changelog.d/500.feature.md".to_string(),
                suffix: "feature".to_string(),
            }],
            "F2: a fragment introduced via git mv must be seen at its \
             destination path — --diff-filter=AR resolving R rows is what \
             makes this visible"
        );
    }

    // -- M4 R1: `doc` ----------------------------------------------------

    #[test]
    fn r1_doc_adding_a_spec_test_fails() {
        let diff = DocDiff {
            touches_doc_paths: true,
            adds_spec_attr: true,
            touches_tests: false,
        };
        assert_eq!(check_r1_doc(&diff), Err(RuleViolation::DocAddsSpecTest));
    }

    #[test]
    fn r1_doc_touching_tests_fails() {
        let diff = DocDiff {
            touches_doc_paths: true,
            adds_spec_attr: false,
            touches_tests: true,
        };
        assert_eq!(check_r1_doc(&diff), Err(RuleViolation::DocTouchesTests));
    }

    #[test]
    fn r1_doc_touching_only_rustdoc_in_src_plus_docs_passes() {
        // Pins the rejection: rustdoc lives in `src/` and is genuinely
        // edited as doc work (`xtask/linkage-check/src/main.rs:1-60` is a
        // 60-line module doc; branch `fix/242-crossterm-doc-contract` is a
        // live case) — this must NOT fail merely for touching `src/`.
        //
        // S6: this constructs no `src/` path at all — `DocDiff` carries no
        // `touches_src` field to construct one with. The real protection is
        // structural: re-adding that field would break this struct literal
        // at compile time, forcing a human to look, which is a better pin
        // than a runtime assertion. The name describes the behavior that
        // absence guarantees, not a diff this test builds.
        let diff = DocDiff {
            touches_doc_paths: true,
            adds_spec_attr: false,
            touches_tests: false,
        };
        assert_eq!(check_r1_doc(&diff), Ok(()));
    }

    #[test]
    fn r1_doc_touching_no_doc_paths_fails() {
        // The positive limb: docs/**, site/**, a root *.md, or prds/**
        // must be touched by something.
        let diff = DocDiff {
            touches_doc_paths: false,
            adds_spec_attr: false,
            touches_tests: false,
        };
        assert_eq!(check_r1_doc(&diff), Err(RuleViolation::DocMissingDocPaths));
    }

    // -- S5: tests/**/*.md is doc work, not test-touching -------------------

    /// Scenario: builds a scratch repo, then a second commit that only
    /// rewords `docs/x.md` and `tests/CATALOG.md`, and asserts R1 passes —
    /// `tests/CATALOG.md` is prose documentation of the test suite, not test
    /// code, so it must not trip `DocTouchesTests`.
    #[test]
    fn r1_doc_diff_touching_only_catalog_md_under_tests_passes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        std::fs::create_dir_all(tmp.path().join("docs")).expect("mkdir docs");
        std::fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        std::fs::write(tmp.path().join("docs/x.md"), "# x\n").expect("write doc");
        std::fs::write(tmp.path().join("tests/CATALOG.md"), "# catalog\n").expect("write catalog");
        git(&["add", "docs/x.md", "tests/CATALOG.md"], tmp.path());
        git(&["commit", "-q", "-m", "base"], tmp.path());
        let base_sha = current_head_sha(tmp.path()).expect("base sha");

        std::fs::write(tmp.path().join("docs/x.md"), "# x\nmore\n").expect("edit doc");
        std::fs::write(tmp.path().join("tests/CATALOG.md"), "# catalog\nmore\n")
            .expect("edit catalog");
        git(
            &["commit", "-aq", "-m", "reword docs and catalog"],
            tmp.path(),
        );

        let diff = collect_doc_diff(tmp.path(), &base_sha).expect("collect_doc_diff");
        assert_eq!(
            check_r1_doc(&diff),
            Ok(()),
            "a CATALOG.md-only tests/ edit must not trip DocTouchesTests (S5)"
        );
    }

    /// Scenario: the same shape as above, but the second commit adds a real
    /// `.rs` file under `tests/` instead — asserts R1 still fails as
    /// `DocTouchesTests`, so a fix that made `is_test_path` always false
    /// would not satisfy the passing test above vacuously.
    #[test]
    fn r1_doc_diff_touching_a_rust_test_file_still_fails_as_doc_touches_tests() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        std::fs::create_dir_all(tmp.path().join("docs")).expect("mkdir docs");
        std::fs::write(tmp.path().join("docs/x.md"), "# x\n").expect("write doc");
        git(&["add", "docs/x.md"], tmp.path());
        git(&["commit", "-q", "-m", "base"], tmp.path());
        let base_sha = current_head_sha(tmp.path()).expect("base sha");

        std::fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        std::fs::write(tmp.path().join("docs/x.md"), "# x\nmore\n").expect("edit doc");
        std::fs::write(tmp.path().join("tests/foo.rs"), "#[test]\nfn f() {}\n")
            .expect("write test file");
        git(&["add", "docs/x.md", "tests/foo.rs"], tmp.path());
        git(
            &["commit", "-q", "-m", "edit doc and add test file"],
            tmp.path(),
        );

        let diff = collect_doc_diff(tmp.path(), &base_sha).expect("collect_doc_diff");
        assert_eq!(check_r1_doc(&diff), Err(RuleViolation::DocTouchesTests));
    }

    // -- M4 R2: `chore` ----------------------------------------------------

    #[test]
    fn r2_chore_touching_tests_heavily_passes() {
        // Pins the rejection: `test:` is 43 of the last 400 commits, and
        // the bare-`tempfile` sweep (`main.rs:74-120`) is a pure chore
        // touching all of `tests/` — this must NOT fail merely for
        // touching `tests/`.
        //
        // S6: this constructs no `tests/` path at all — `ChoreDiff` carries
        // no `touches_tests` field to construct one with. Same structural
        // guarantee as `r1_doc_touching_only_rustdoc_in_src_plus_docs_passes`
        // above: re-adding that field breaks this struct literal at compile
        // time. The name describes the behavior the field's absence
        // guarantees, not a diff this test builds.
        let diff = ChoreDiff {
            adds_cli_flag: false,
            adds_command_variant: false,
            adds_new_docs_page: false,
        };
        assert_eq!(check_r2_chore(&diff), Ok(()));
    }

    #[test]
    fn r2_chore_adding_a_cli_flag_fails() {
        let diff = ChoreDiff {
            adds_cli_flag: true,
            adds_command_variant: false,
            adds_new_docs_page: false,
        };
        assert_eq!(check_r2_chore(&diff), Err(RuleViolation::ChoreAddsCliFlag));
    }

    #[test]
    fn r2_chore_adding_a_new_command_variant_fails() {
        let diff = ChoreDiff {
            adds_cli_flag: false,
            adds_command_variant: true,
            adds_new_docs_page: false,
        };
        assert_eq!(
            check_r2_chore(&diff),
            Err(RuleViolation::ChoreAddsCommandVariant)
        );
    }

    #[test]
    fn r2_chore_adding_a_new_docs_page_fails() {
        let diff = ChoreDiff {
            adds_cli_flag: false,
            adds_command_variant: false,
            adds_new_docs_page: true,
        };
        assert_eq!(
            check_r2_chore(&diff),
            Err(RuleViolation::ChoreAddsNewDocsPage)
        );
    }

    // -- B3: added_diff_text is restricted to .rs sources --------------------

    /// Scenario: commits a Markdown file whose prose quotes both
    /// `#[spec(` and `#[arg(long = "`, and asserts `added_diff_text` does
    /// not surface either — a doc PR that quotes the attribute syntax (as
    /// CLAUDE.md, CONTRIBUTING.md and this PRD's own file all do) must not
    /// be rejected as `DocAddsSpecTest`/`ChoreAddsCliFlag`.
    #[test]
    fn added_diff_text_ignores_markdown_lines_quoting_spec_or_arg_long() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        std::fs::write(tmp.path().join("README.md"), "base\n").expect("write base file");
        git(&["add", "README.md"], tmp.path());
        git(&["commit", "-q", "-m", "base"], tmp.path());
        let base_sha = current_head_sha(tmp.path()).expect("base sha");

        std::fs::write(
            tmp.path().join("CLAUDE.md"),
            "Every `#[spec(\"...\")]` test needs a Scenario comment, and every \
             `#[arg(long = \"foo\")]` flag needs a docs update.\n",
        )
        .expect("write doc file");
        git(&["add", "CLAUDE.md"], tmp.path());
        git(
            &["commit", "-q", "-m", "doc quoting spec and arg-long"],
            tmp.path(),
        );

        let text = added_diff_text(tmp.path(), &base_sha).expect("added_diff_text");
        assert!(
            !text.contains("#[spec("),
            "B3: a Markdown line merely quoting #[spec( must not be counted"
        );
        assert!(
            !text.contains("#[arg(long = \""),
            "B3: a Markdown line merely quoting #[arg(long = \" must not be counted"
        );
    }

    /// Scenario: the other direction B3 protects — commits a `.rs` file that
    /// genuinely adds both attributes, and asserts `added_diff_text` still
    /// surfaces them, so a fix that made the search ignore `.rs` files too
    /// (always false) would not satisfy the test above vacuously.
    #[test]
    fn added_diff_text_still_sees_spec_and_arg_long_added_in_rust_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(&["init", "-q"], tmp.path());
        git(&["config", "user.email", "test@example.com"], tmp.path());
        git(&["config", "user.name", "test"], tmp.path());
        std::fs::write(tmp.path().join("README.md"), "base\n").expect("write base file");
        git(&["add", "README.md"], tmp.path());
        git(&["commit", "-q", "-m", "base"], tmp.path());
        let base_sha = current_head_sha(tmp.path()).expect("base sha");

        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir src");
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "#[spec(\"fixture/b3\")]\nfn f() {}\n\n#[arg(long = \"foo\")]\npub flag: bool,\n",
        )
        .expect("write rust source");
        git(&["add", "src/lib.rs"], tmp.path());
        git(
            &["commit", "-q", "-m", "add spec test and cli flag"],
            tmp.path(),
        );

        let text = added_diff_text(tmp.path(), &base_sha).expect("added_diff_text");
        assert!(
            text.contains("#[spec("),
            "a genuine .rs addition of #[spec( must still be visible to R1"
        );
        assert!(
            text.contains("#[arg(long = \""),
            "a genuine .rs addition of #[arg(long = \" must still be visible to R2"
        );
    }

    // -- M4 R3: `bug` ----------------------------------------------------

    /// A one-function `#[spec]` source, for `spec_test_delta` fixtures —
    /// following `xtask/linkage-check/tests/duplicate_catalog_id.rs`'s
    /// pattern of synthetic sources containing `#[spec("...")]`.
    fn spec_source(spec_id: &str, fn_name: &str, body: &str) -> String {
        format!(
            "#[spec(\"{spec_id}\")]\n/// Scenario: fixture only, not a real test.\nfn {fn_name}() {{\n{body}\n}}\n"
        )
    }

    fn tests_map(sources: &[(String, String)]) -> BTreeMap<String, TestEntry> {
        collect_tests_from_sources(sources).expect("synthetic fixture source must parse")
    }

    #[test]
    fn spec_test_delta_is_false_when_nothing_spec_related_changed() {
        let source = spec_source("fixture/work-type/001", "bug_001_noop", "let x = 1;");
        let base = tests_map(&[("tests/e2e_fixture.rs".to_string(), source.clone())]);
        let head = tests_map(&[("tests/e2e_fixture.rs".to_string(), source)]);
        assert!(
            !spec_test_delta(&base, &head),
            "identical base/head must not report a spec-test delta"
        );
    }

    #[test]
    fn spec_test_delta_is_true_for_a_newly_added_spec_test() {
        let base = tests_map(&[]);
        let head = tests_map(&[(
            "tests/e2e_fixture.rs".to_string(),
            spec_source("fixture/work-type/002", "bug_002_added", "let x = 1;"),
        )]);
        assert!(
            spec_test_delta(&base, &head),
            "a #[spec] test added in head must count toward R3's delta"
        );
    }

    #[test]
    fn spec_test_delta_is_true_for_a_modified_spec_test_body() {
        let base = tests_map(&[(
            "tests/e2e_fixture.rs".to_string(),
            spec_source("fixture/work-type/003", "bug_003_modified", "let x = 1;"),
        )]);
        let head = tests_map(&[(
            "tests/e2e_fixture.rs".to_string(),
            spec_source("fixture/work-type/003", "bug_003_modified", "let x = 2;"),
        )]);
        assert!(
            spec_test_delta(&base, &head),
            "a genuinely modified test body must count toward R3's delta"
        );
    }

    #[test]
    fn spec_test_delta_is_false_for_a_whitespace_only_body_edit() {
        // Proves the fingerprint is real rather than a line-count check:
        // the token stream is unaffected by reformatting, so this must NOT
        // count — a whitespace edit to CATALOG.md is exactly the weak case
        // R3's original "a file under tests/ changed" was rejected for.
        let base = tests_map(&[(
            "tests/e2e_fixture.rs".to_string(),
            spec_source("fixture/work-type/004", "bug_004_whitespace", "let x = 1;"),
        )]);
        let head = tests_map(&[(
            "tests/e2e_fixture.rs".to_string(),
            spec_source(
                "fixture/work-type/004",
                "bug_004_whitespace",
                "\n\n    let x    =    1;\n\n",
            ),
        )]);
        assert!(
            !spec_test_delta(&base, &head),
            "a whitespace-only body edit must not count toward R3's delta"
        );
    }

    #[test]
    fn r3_bug_with_no_spec_delta_and_no_escape_hatch_fails() {
        let diff = BugDiff {
            spec_test_delta: false,
            no_test_reason: None,
        };
        assert_eq!(
            check_r3_bug(&diff),
            Err(RuleViolation::BugMissingSpecTestDelta)
        );
    }

    #[test]
    fn r3_bug_with_a_spec_delta_passes() {
        let diff = BugDiff {
            spec_test_delta: true,
            no_test_reason: None,
        };
        assert_eq!(check_r3_bug(&diff), Ok(()));
    }

    #[test]
    fn r3_bug_with_a_no_test_reason_passes() {
        let diff = BugDiff {
            spec_test_delta: false,
            no_test_reason: Some(
                "Windows symlink materialisation cannot be exercised here".to_string(),
            ),
        };
        assert_eq!(check_r3_bug(&diff), Ok(()));
    }

    #[test]
    fn r3_bug_with_an_empty_no_test_reason_fails() {
        // M4 tester decision: an empty reason does NOT count as the escape
        // hatch — see `check_r3_bug`'s doc comment for the rationale. This
        // is the test that pins that decision.
        let diff = BugDiff {
            spec_test_delta: false,
            no_test_reason: Some(String::new()),
        };
        assert_eq!(
            check_r3_bug(&diff),
            Err(RuleViolation::BugMissingSpecTestDelta)
        );
    }

    /// Scenario: builds a scratch git repo (same helpers `--self-test` uses)
    /// whose only added fragment is a `.breaking.md` carrying a `No-Test:`
    /// line and no `#[spec]` test delta, on a `fix/` branch — the exact
    /// shape F2 describes as previously having no legal way through R3.
    /// Asserts `collect_bug_diff` actually reads the `No-Test:` reason out
    /// of the `.breaking.md` fragment (not just that `check_r3_bug` accepts
    /// a hand-built `BugDiff`, which the tests above already cover and which
    /// would pass even with the pre-fix filter).
    #[test]
    fn collect_bug_diff_reads_no_test_reason_from_a_breaking_only_fragment() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        init_self_test_repo(dir).expect("init scratch repo");
        let base_sha = resolve_base(Some("origin/main"), dir).expect("resolve base");

        run_git(&["checkout", "-q", "-b", "fix/451-breaking-no-test"], dir).expect("checkout");
        std::fs::create_dir_all(dir.join("changelog.d")).expect("mkdir changelog.d");
        std::fs::write(
            dir.join("changelog.d/451.breaking.md"),
            "A breaking bug fix in xtask/, outside R3's tests/+src/ scan.\n\n\
             No-Test: fix lives entirely in xtask/, which R3 does not scan\n",
        )
        .expect("write changelog.d/451.breaking.md");
        run_git(&["add", "changelog.d/451.breaking.md"], dir).expect("add fragment");
        run_git(
            &["commit", "-q", "-m", "breaking bugfix, no spec test"],
            dir,
        )
        .expect("commit fragment");

        let fragments = [AddedFragment {
            path: "changelog.d/451.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        }];
        let diff = collect_bug_diff(dir, &base_sha, &fragments).expect("collect_bug_diff");
        assert_eq!(
            diff.no_test_reason.as_deref(),
            Some("fix lives entirely in xtask/, which R3 does not scan"),
            "a .breaking.md fragment's No-Test: line must be read once the diff has \
             already resolved to Bug, not silently filtered out"
        );
        assert_eq!(
            check_r3_bug(&diff),
            Ok(()),
            "the escape hatch must actually satisfy R3 for this shape"
        );
    }

    /// Scenario: same scratch-repo shape as above, but the `.breaking.md`
    /// fragment carries no `No-Test:` line — confirms F2's widened filter
    /// did not also make R3 toothless for a `.breaking.md`-derived bug that
    /// genuinely has neither a spec-test delta nor a documented exemption.
    #[test]
    fn collect_bug_diff_still_fails_r3_for_a_breaking_fragment_with_no_escape_hatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        init_self_test_repo(dir).expect("init scratch repo");
        let base_sha = resolve_base(Some("origin/main"), dir).expect("resolve base");

        run_git(&["checkout", "-q", "-b", "fix/451-breaking-untested"], dir).expect("checkout");
        std::fs::create_dir_all(dir.join("changelog.d")).expect("mkdir changelog.d");
        std::fs::write(
            dir.join("changelog.d/451.breaking.md"),
            "A breaking bug fix with no No-Test: line and no spec test delta.\n",
        )
        .expect("write changelog.d/451.breaking.md");
        run_git(&["add", "changelog.d/451.breaking.md"], dir).expect("add fragment");
        run_git(
            &["commit", "-q", "-m", "breaking bugfix, no escape hatch"],
            dir,
        )
        .expect("commit fragment");

        let fragments = [AddedFragment {
            path: "changelog.d/451.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        }];
        let diff = collect_bug_diff(dir, &base_sha, &fragments).expect("collect_bug_diff");
        assert_eq!(diff.no_test_reason, None);
        assert_eq!(
            check_r3_bug(&diff),
            Err(RuleViolation::BugMissingSpecTestDelta),
            "widening the No-Test: filter must not also let an undocumented \
             .breaking.md-derived bug fix through R3"
        );
    }

    #[test]
    fn parse_no_test_directive_finds_a_reason() {
        let fragment = "Fixed the daemon lazy-spawn timeout.\n\nNo-Test: CI-config bug, no harness to exercise it\n";
        assert_eq!(
            parse_no_test_directive(fragment),
            Some("CI-config bug, no harness to exercise it".to_string())
        );
    }

    #[test]
    fn parse_no_test_directive_returns_none_when_absent() {
        let fragment = "Fixed the daemon lazy-spawn timeout.\n";
        assert_eq!(parse_no_test_directive(fragment), None);
    }

    #[test]
    fn parse_no_test_directive_returns_empty_string_for_a_bare_directive() {
        let fragment = "Fixed the daemon lazy-spawn timeout.\n\nNo-Test:\n";
        assert_eq!(parse_no_test_directive(fragment), Some(String::new()));
    }

    // -- M4 R4: `prd` ----------------------------------------------------

    #[test]
    fn fragment_stem_reads_the_portion_before_the_first_dot() {
        assert_eq!(fragment_stem("changelog.d/341.feature.md"), Some("341"));
        assert_eq!(
            fragment_stem("changelog.d/clickable-hyperlinks.feature.md"),
            Some("clickable-hyperlinks")
        );
    }

    #[test]
    fn matching_prds_file_exists_true_for_plain_numbered_form() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prds_dir = tmp.path().join("prds");
        std::fs::create_dir_all(&prds_dir).expect("mkdir prds");
        std::fs::write(prds_dir.join("341-work-type-vocabulary.md"), "").expect("write fixture");
        assert_eq!(matching_prds_file_exists(&prds_dir, "341"), Ok(true));
    }

    #[test]
    fn matching_prds_file_exists_true_for_fork_prefixed_form() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prds_dir = tmp.path().join("prds");
        std::fs::create_dir_all(&prds_dir).expect("mkdir prds");
        std::fs::write(prds_dir.join("fork-341-work-type-vocabulary.md"), "")
            .expect("write fixture");
        assert_eq!(matching_prds_file_exists(&prds_dir, "341"), Ok(true));
    }

    #[test]
    fn matching_prds_file_exists_true_under_done() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prds_dir = tmp.path().join("prds");
        let done_dir = prds_dir.join("done");
        std::fs::create_dir_all(&done_dir).expect("mkdir prds/done");
        std::fs::write(done_dir.join("341-work-type-vocabulary.md"), "").expect("write fixture");
        assert_eq!(matching_prds_file_exists(&prds_dir, "341"), Ok(true));
    }

    #[test]
    fn matching_prds_file_exists_false_when_nothing_matches() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prds_dir = tmp.path().join("prds");
        std::fs::create_dir_all(&prds_dir).expect("mkdir prds");
        std::fs::write(prds_dir.join("999-unrelated.md"), "").expect("write fixture");
        assert_eq!(matching_prds_file_exists(&prds_dir, "341"), Ok(false));
    }

    #[test]
    fn r4_feature_stem_with_no_matching_prds_file_fails() {
        assert_eq!(
            check_r4_prd("changelog.d/341.feature.md", "341", false),
            Err(RuleViolation::PrdNoMatchingFile {
                fragment_path: "changelog.d/341.feature.md".to_string(),
                stem: "341".to_string(),
            })
        );
    }

    #[test]
    fn r4_feature_stem_with_a_matching_prds_file_passes() {
        assert_eq!(
            check_r4_prd("changelog.d/341.feature.md", "341", true),
            Ok(())
        );
    }

    #[test]
    fn r4_non_numeric_stem_fails_even_when_a_file_happens_to_match() {
        // E9, decided: a numeric stem is required for feature/breaking
        // fragments. Passing `prds_file_exists: true` here proves this is
        // an independent, deliberate check — not merely a consequence of
        // `matching_prds_file_exists` returning false for a stem it can't
        // search for.
        assert_eq!(
            check_r4_prd(
                "changelog.d/clickable-hyperlinks.feature.md",
                "clickable-hyperlinks",
                true,
            ),
            Err(RuleViolation::PrdNonNumericStem {
                fragment_path: "changelog.d/clickable-hyperlinks.feature.md".to_string(),
                stem: "clickable-hyperlinks".to_string(),
            })
        );
    }

    // -- N1: RULE_COUNT feeds both the success line and the case array -----

    /// Scenario: mirrors `self_test`'s own `[fn() -> Result<String, String>;
    /// RULE_COUNT]` case-array literal and asserts its length equals
    /// `RULE_COUNT` — a case added to (or removed from) that array without
    /// bumping `RULE_COUNT` (or vice versa) fails to compile here, the same
    /// structural guarantee `r1_doc_touching_only_rustdoc_in_src_plus_docs_passes`
    /// (S6) already relies on.
    #[test]
    fn self_test_case_array_length_matches_rule_count() {
        let cases: [fn() -> Result<String, String>; RULE_COUNT] = [
            self_test_r0,
            self_test_r1,
            self_test_r2,
            self_test_r3,
            self_test_r4,
        ];
        assert_eq!(cases.len(), RULE_COUNT);
    }

    /// Scenario: calls `describe_success` directly and asserts its "N rules"
    /// text is derived from `RULE_COUNT` rather than a separate hardcoded
    /// literal — the two-literals-for-one-fact shape N1 was written to
    /// close.
    #[test]
    fn describe_success_names_the_rule_count_from_the_shared_constant() {
        let derivation = Derivation {
            work_type: WorkType::Doc,
            supplier: Supplier::BranchPrefix,
        };
        let msg = describe_success(&derivation, "docs/123-thing", "abc123", &[]);
        assert!(
            msg.contains(&format!("{RULE_COUNT} rules")),
            "describe_success must derive its rule count from RULE_COUNT, not a \
             separate hardcoded literal (N1): got {msg:?}"
        );
    }

    /// Scenario: renders the success line for the exact `.breaking.md` +
    /// `fix/` derivation the branch-aware carve-out produces, and asserts it
    /// names the branch prefix rather than falling through to `describe_success`'s
    /// `unwrap_or("?")` (F1) — before the fix, `derivation.supplier` was
    /// `Supplier::Fragment` while the only fragment present (`.breaking.md`)
    /// maps to `Prd`, not `Bug`, so the fragment lookup found nothing and
    /// the line read `... from changelog fragment '?' ...`.
    #[test]
    fn describe_success_names_branch_prefix_for_the_breaking_carve_out() {
        let fragments = [AddedFragment {
            path: "changelog.d/451.breaking.md".to_string(),
            suffix: "breaking".to_string(),
        }];
        let derivation = derive_work_type(&fragments, "fix/451-thing")
            .expect("a breaking bugfix on a fix/ branch must resolve");
        let msg = describe_success(&derivation, "fix/451-thing", "abc123", &fragments);
        assert!(
            msg.contains("from branch prefix 'fix/'"),
            "expected the success line to name the branch, not fall through to '?': got {msg:?}"
        );
        assert!(
            !msg.contains('?'),
            "the success line must never contain the unresolved-lookup fallback: got {msg:?}"
        );
    }
}
