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

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::list_tests::TestEntry;

/// Branch prefixes that skip the gate outright — Renovate PRs automerge
/// without a human (`ci.yml:14-16`), so a gate that blocked them would get
/// disabled rather than obeyed. `sync/` and `upstream/` carry fork-sync
/// commits, which are equally not a human's PR to label.
const EXEMPT_BRANCH_PREFIXES: [&str; 3] = ["renovate/", "sync/", "upstream/"];

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
        "feature" | "breaking" => Some(WorkType::Prd),
        "doc" => Some(WorkType::Doc),
        "misc" => Some(WorkType::Chore),
        _ => None,
    }
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
pub fn derive_work_type(
    fragments: &[AddedFragment],
    branch: &str,
) -> Result<Derivation, WorkTypeError> {
    // Tier 1: every added fragment must map, and every mapped fragment must
    // agree — an unrecognized suffix fails immediately (does not fall
    // through to tier 2), and a disagreement between two fragments fails
    // before tier 2 is even consulted.
    let mut fragment_supply: Option<(String, WorkType)> = None;
    for fragment in fragments {
        let work_type =
            suffix_to_work_type(&fragment.suffix).ok_or_else(|| WorkTypeError::UnknownSuffix {
                path: fragment.path.clone(),
                suffix: fragment.suffix.clone(),
            })?;
        match &fragment_supply {
            None => fragment_supply = Some((fragment.path.clone(), work_type)),
            Some((first_path, first_type)) if *first_type != work_type => {
                return Err(WorkTypeError::ConflictingFragments {
                    first: (first_path.clone(), *first_type),
                    second: (fragment.path.clone(), work_type),
                });
            }
            Some(_) => {}
        }
    }

    let branch_supply = branch_prefix_to_work_type(branch);

    match (fragment_supply, branch_supply) {
        (Some((path, fragment_type)), Some(branch_type)) => {
            if fragment_type == branch_type {
                Ok(Derivation {
                    work_type: fragment_type,
                    supplier: Supplier::Fragment,
                })
            } else {
                Err(WorkTypeError::FragmentBranchDisagree {
                    fragment: (path, fragment_type),
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

/// The testable core of `cargo xtask work-type-check`: parses `args`,
/// derives the work type for the diff in `repo_dir` against the resolved
/// base, and returns the process exit code — [`ExitCode::SUCCESS`],
/// [`EXIT_RULE_VIOLATION`], or [`EXIT_BASE_UNRESOLVABLE`]. Split from
/// [`run`] so a test can point it at a scratch git repo instead of the real
/// one.
pub fn run_in(args: &[String], repo_dir: &Path) -> ExitCode {
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

    let branch = match current_branch(repo_dir) {
        Ok(branch) => branch,
        Err(e) => {
            eprintln!("work-type-check: could not determine the current branch: {e}");
            return ExitCode::from(EXIT_RULE_VIOLATION);
        }
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

    let fragments = match collect_added_fragments(repo_dir, &base_sha) {
        Ok(fragments) => fragments,
        Err(e) => {
            eprintln!("work-type-check: could not read added changelog fragments: {e}");
            return ExitCode::from(EXIT_RULE_VIOLATION);
        }
    };

    match derive_work_type(&fragments, &branch) {
        Ok(derivation) => {
            println!(
                "work-type-check: ok ({})",
                describe_success(&derivation, &branch, &base_sha, &fragments)
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("work-type-check: {e}");
            ExitCode::from(EXIT_RULE_VIOLATION)
        }
    }
}

/// `cargo xtask work-type-check`'s entry point — [`run_in`] against the
/// current directory.
pub fn run(args: &[String]) -> ExitCode {
    let repo_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_in(args, &repo_dir)
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
/// between `base_sha` and `HEAD` — tier 1's supply. `--diff-filter=A` is
/// load-bearing: a fragment merely touched by this diff (e.g. a rebase
/// conflict resolution) is not "added in this diff" and must not count.
fn collect_added_fragments(repo_dir: &Path, base_sha: &str) -> Result<Vec<AddedFragment>, String> {
    let out = Command::new("git")
        .args([
            "diff",
            "--name-status",
            "--diff-filter=A",
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

    let mut fragments: Vec<AddedFragment> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (_status, path) = line.split_once('\t')?;
            let path = path.trim();
            if !path.ends_with(".md") {
                return None;
            }
            let file_name = Path::new(path).file_name()?.to_str()?;
            // "<stem>.<suffix>.md" — the suffix is whatever sits immediately
            // before the trailing ".md", however many dots the stem itself
            // carries (towncrier's `<issue>.<counter>.<suffix>.md` form).
            let mut segments = file_name.rsplitn(3, '.');
            segments.next()?; // "md"
            let suffix = segments.next()?.to_string();
            Some(AddedFragment {
                path: path.to_string(),
                suffix,
            })
        })
        .collect();
    fragments.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(fragments)
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
        "work type '{}' from {supplier}, base {base_sha}, 1 rule",
        derivation.work_type
    )
}

/// Run `git <args>` in `dir`, collapsing failure to a single message —
/// [`self_test`]'s own scratch-repo setup, not the thing under test.
fn run_git(args: &[&str], dir: &Path) -> Result<(), String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
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

/// `--self-test`: build a scratch repo that genuinely violates R0 — a diff
/// adding no changelog fragment, on a branch carrying no recognized
/// work-type prefix — and assert the real production pipeline
/// ([`resolve_base`] + [`collect_added_fragments`] + [`derive_work_type`])
/// rejects it, and rejects it for that *specific* reason. Follows
/// `scripts/check-symlinks.sh --self-test` in shape: the same code path is
/// shown failing on a broken case immediately before it runs for real
/// (`ci.yml:325-326`'s pattern for `work-type-check`).
///
/// E5: this must not decay into `assert!(derive("").is_err())`. It checks
/// its own preconditions before trusting the result, so it fails loudly —
/// rather than passing vacuously — if the case it builds ever stops
/// violating.
fn self_test() -> ExitCode {
    let tmp = match tempfile::tempdir() {
        Ok(tmp) => tmp,
        Err(e) => {
            eprintln!("work-type-check --self-test: could not create a scratch dir: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = tmp.path();

    let build_scratch_repo = || -> Result<(), String> {
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
        // Fake origin/main locally so base resolution succeeds — this
        // self-test proves R0's derivation is rejected, not E1's already
        // separately-pinned base-unresolvable case.
        run_git(&["update-ref", "refs/remotes/origin/main", "HEAD"], dir)?;
        // A branch carrying none of the four recognized work-type prefixes,
        // with no divergence from base at all — no fragment can have been
        // added, so tier 1 is empty by construction too.
        run_git(&["checkout", "-q", "-b", "self-test-no-supplier"], dir)?;
        Ok(())
    };
    if let Err(e) = build_scratch_repo() {
        eprintln!("work-type-check --self-test: could not build the violating scratch repo: {e}");
        return ExitCode::FAILURE;
    }

    let branch = "self-test-no-supplier";
    // Fail loudly (E5) rather than trusting the scratch repo blindly: if a
    // future edit ever gives this literal branch name a recognized prefix,
    // the case stops violating and this must say so, not report a false ok.
    if branch_prefix_to_work_type(branch).is_some() {
        eprintln!(
            "work-type-check --self-test: FAILED — scratch branch {branch:?} now carries a \
             recognized work-type prefix; this no longer builds a violating case."
        );
        return ExitCode::FAILURE;
    }

    let base_sha = match resolve_base(Some("origin/main"), dir) {
        Ok(sha) => sha,
        Err(e) => {
            eprintln!(
                "work-type-check --self-test: FAILED — could not resolve the scratch repo's \
                 own base, so this proves nothing about R0: {e}"
            );
            return ExitCode::FAILURE;
        }
    };
    let fragments = match collect_added_fragments(dir, &base_sha) {
        Ok(fragments) => fragments,
        Err(e) => {
            eprintln!(
                "work-type-check --self-test: FAILED — could not read the scratch repo's own \
                 diff: {e}"
            );
            return ExitCode::FAILURE;
        }
    };
    if !fragments.is_empty() {
        eprintln!(
            "work-type-check --self-test: FAILED — the scratch repo unexpectedly carries added \
             changelog fragments ({fragments:?}); this no longer builds a violating case."
        );
        return ExitCode::FAILURE;
    }

    match derive_work_type(&fragments, branch) {
        Err(WorkTypeError::NoSupplier { .. }) => {
            println!(
                "work-type-check --self-test: ok (a diff with no added changelog fragment and \
                 an unprefixed branch was correctly rejected as NoSupplier)"
            );
            ExitCode::SUCCESS
        }
        Ok(derivation) => {
            eprintln!(
                "work-type-check --self-test: FAILED — the violating case was accepted as \
                 {derivation:?} instead of being rejected"
            );
            ExitCode::FAILURE
        }
        Err(other) => {
            eprintln!(
                "work-type-check --self-test: FAILED — the violating case was rejected, but \
                 not for the NoSupplier reason: {other:?}"
            );
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// M4 — rules R1-R4 (PRD fork#340). R0 above is the spine and is unaffected.
//
// Each rule below is a pure function over a small, directly-constructible
// struct — the same shape as `derive_work_type` taking `&[AddedFragment]`
// rather than reading git itself. The git-diff-to-struct extraction glue
// (walking the actual diff to fill in these booleans) is the coder's to
// write when R1-R4 are wired into `run_in`; it is out of this round's scope
// and carries no test here.
//
// RED: every function below is `todo!()`. No wrong-but-typed stub — a stub
// that returns a fixed `Err` would make every "must fail" case below pass
// vacuously, which in a PRD about empty gates is the worst possible RED.
// ---------------------------------------------------------------------------

/// Why R1-R4 rejected a diff. Kept as one enum (rather than one per rule)
/// because callers that reject a whole PR want to match across rules
/// uniformly, the same reasoning `WorkTypeError` already applies to R0.
// M4 RED (PRD fork#340): R1-R4 are pinned by tests but not yet wired into
// `run_in`'s pipeline — that wiring is the coder's, scoped out of this
// round. Until then every item below is reachable only from `#[cfg(test)]`,
// so `not(test)` dead-code would otherwise fire on the plain (non-test)
// binary target. Same precedent as `src/platform/fsperm/mod.rs`. Remove
// every one of these attributes when the wiring lands — a real caller
// resolves the lint, so a leftover `allow` at that point means the coder
// forgot to plumb it in, not that the annotation is still needed.
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn check_r1_doc(_diff: &DocDiff) -> Result<(), RuleViolation> {
    todo!("R1 doc rule — PRD fork#340 M4, implemented by coder")
}

/// R2's inputs — booleans over the diff.
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn check_r2_chore(_diff: &ChoreDiff) -> Result<(), RuleViolation> {
    todo!("R2 chore rule — PRD fork#340 M4, implemented by coder")
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn spec_test_delta(
    _base: &BTreeMap<String, TestEntry>,
    _head: &BTreeMap<String, TestEntry>,
) -> bool {
    todo!("R3 spec-test delta — PRD fork#340 M4, implemented by coder")
}

/// Parse a `No-Test: <reason>` directive out of a changelog fragment's file
/// content — R3's escape hatch, mirroring `m2.allowlist`'s
/// documented-exception pattern and deliberately kept visible in the
/// release-notes source. `None` when no line begins with `No-Test:`.
/// `Some(reason)` when one does, where `reason` is the trimmed text after
/// the colon — which may be empty; see [`check_r3_bug`] for what an empty
/// reason means for the escape hatch.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_no_test_directive(_fragment_body: &str) -> Option<String> {
    todo!("R3 No-Test: directive parsing — PRD fork#340 M4, implemented by coder")
}

/// R3's inputs.
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn check_r3_bug(_diff: &BugDiff) -> Result<(), RuleViolation> {
    todo!("R3 bug rule — PRD fork#340 M4, implemented by coder")
}

/// The fragment's stem — the portion of the filename before the first `.`,
/// e.g. `"341"` for `changelog.d/341.feature.md`, or `"clickable-hyperlinks"`
/// for the real historical fragment `changelog.d/clickable-hyperlinks.feature.md`
/// (E9: a `.feature.md` with a non-numeric stem — seven historical fragments
/// used this shape). `None` when `path`'s file name carries no `.` at all.
#[cfg_attr(not(test), allow(dead_code))]
pub fn fragment_stem(_path: &str) -> Option<&str> {
    todo!("R4 fragment stem extraction — PRD fork#340 M4, implemented by coder")
}

/// Whether a `prds/` file matches fragment stem `stem` —
/// `prds/<stem>-*.md`, `prds/fork-<stem>-*.md`, or either under
/// `prds/done/`. Existence on the filesystem, never diff membership — R4's
/// whole point: a PRD spans many milestones and many PRs, and only the
/// first touches the file, so requiring the file *in the diff* would fail
/// most legitimate PRD PRs.
#[cfg_attr(not(test), allow(dead_code))]
pub fn matching_prds_file_exists(_prds_dir: &Path, _stem: &str) -> Result<bool, String> {
    todo!("R4 filesystem existence check — PRD fork#340 M4, implemented by coder")
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn check_r4_prd(
    _fragment_path: &str,
    _stem: &str,
    _prds_file_exists: bool,
) -> Result<(), RuleViolation> {
    todo!("R4 prd rule — PRD fork#340 M4, implemented by coder")
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

    // -- M4 R2: `chore` ----------------------------------------------------

    #[test]
    fn r2_chore_touching_tests_heavily_passes() {
        // Pins the rejection: `test:` is 43 of the last 400 commits, and
        // the bare-`tempfile` sweep (`main.rs:74-120`) is a pure chore
        // touching all of `tests/` — this must NOT fail merely for
        // touching `tests/`.
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
}
