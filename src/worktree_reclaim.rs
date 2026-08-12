//! `dot-agent-deck worktree list|reclaim`.
//!
//! Reclaims a git worktree only when two gates hold: its PR's state is
//! `MERGED` (via `gh`, never git ancestry — squash-merges never enter `main`'s
//! ancestry, so an ancestry check misses genuinely merged branches), and the
//! tree is clean (`git status --porcelain` empty — a merged branch's worktree
//! can still hold uncommitted files that were never part of the PR, and
//! `--porcelain` never reports gitignored content, so a worktree still
//! holding a `target/` or a `.env` also counts as "clean" here). A THIRD
//! signal — whether the deck can prove it created the worktree, AND whether
//! that clean tree still holds gitignored content — decides *how* a
//! merged-and-clean worktree is removed: a deck-created worktree with no
//! gitignored content is removed by a bare `reclaim`, with no `--yes` needed;
//! a worktree the deck cannot prove it created, OR one that still holds
//! gitignored content regardless of provenance, is instead reported as
//! reclaimable-pending-confirmation, naming its exact path, and removed only
//! once the user passes `--yes` — at which point it is removed regardless of
//! provenance or ignored content, exactly like a deck-created one. `--yes`
//! never asks the deck to trust more; it is the user vouching for what they
//! were just shown. The branch is never deleted, only the worktree directory.
//!
//! Fail-closed throughout: an unresolvable PR state (missing `gh`, a spawn or
//! parse error, or more than one PR matching the branch) means keep, never
//! remove — the gate must be satisfied affirmatively, never by absence of
//! evidence. Unknown ownership resolves to foreign, never to ours.
//!
//! No daemon/protocol involvement: this is a CLI verb that shells out to
//! `git` and `gh` directly, synchronously — no `PROTOCOL_VERSION` bump.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::worktree_owner::{OWNER_MARKER_FILENAME, path_from_bytes};

/// Version of the `--json` document shape. Bump on a field removal or a
/// meaning change; additive fields don't need a bump.
pub const SCHEMA_VERSION: u32 = 1;


/// Resolved PR state for a worktree's branch, or why it could not be
/// resolved. `Unresolvable` and `NoPr` both keep — the distinction is only
/// for the reported reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrState {
    Merged,
    Open,
    ClosedUnmerged,
    NoPr,
    Unresolvable(String),
}

impl PrState {
    fn label(&self) -> &'static str {
        match self {
            PrState::Merged => "merged",
            PrState::Open => "open",
            PrState::ClosedUnmerged => "closed_unmerged",
            PrState::NoPr => "no_pr",
            PrState::Unresolvable(_) => "unresolvable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Ours,
    Foreign,
}

/// The gate's outcome for one worktree. `Keep` and `Ask` both carry a reason;
/// `Ask` additionally means "would be removed, but requires `--yes`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Remove,
    Ask(String),
    Keep(String),
}

impl Verdict {
    fn label(&self) -> &'static str {
        match self {
            Verdict::Remove => "remove",
            Verdict::Ask(_) => "ask",
            Verdict::Keep(_) => "keep",
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Remove => None,
            Verdict::Ask(r) | Verdict::Keep(r) => Some(r.as_str()),
        }
    }
}

/// The pure decision gate: (PR state, cleanliness, ownership) -> verdict.
///
/// Evaluation order — and which reason wins when more than one condition
/// applies — is deliberate:
///
/// 1. **PR state first.** Anything other than `Merged` keeps, with a reason
///    naming the PR state, regardless of cleanliness or ownership: a not-yet-
///    merged worktree is not a candidate at all.
/// 2. **Cleanliness second.** A merged-but-dirty worktree keeps with a "dirty"
///    reason even when it is deck-owned — dirty content was never part of the
///    PR, so "the code is already merged" does not cover it. This reason wins
///    over ownership: a dirty foreign worktree is reported as dirty, not as
///    foreign, since dirty is the harder blocker (no flag can override it). An
///    *unresolvable* cleanliness probe (the check itself failed, rather than
///    finding anything dirty) also keeps, but with a distinct reason — it
///    must never be reported as "dirty" when nothing was actually found.
/// 3. **Ownership last.** Only once merged-and-clean is established does
///    ownership decide `Remove` (ours) vs `Ask` (foreign) — cleanliness alone
///    proves nothing about ownership, since a freshly created, not-yet-written
///    worktree is clean by definition.
pub fn decide(pr_state: &PrState, clean: &Cleanliness, ownership: Ownership) -> Verdict {
    match pr_state {
        PrState::Merged => match clean {
            Cleanliness::Dirty => Verdict::Keep(
                "dirty: uncommitted or untracked changes are present that were never part of the merged PR"
                    .to_string(),
            ),
            Cleanliness::Unresolvable(reason) => Verdict::Keep(format!(
                "the cleanliness check itself failed ({reason}) — keeping rather than guessing; \
                 nothing was found in the tree"
            )),
            Cleanliness::Clean => match ownership {
                Ownership::Ours => Verdict::Remove,
                Ownership::Foreign => Verdict::Ask(
                    "reclaimable: PR is merged and the tree is clean, but the deck cannot \
                     prove it created this worktree"
                        .to_string(),
                ),
            },
        },
        PrState::NoPr => Verdict::Keep("no pull request found for this branch".to_string()),
        PrState::Open => Verdict::Keep("pull request is still open".to_string()),
        PrState::ClosedUnmerged => {
            Verdict::Keep("pull request was closed without being merged".to_string())
        }
        PrState::Unresolvable(reason) => Verdict::Keep(format!(
            "pull request state could not be resolved ({reason}) — keeping rather than \
             guessing"
        )),
    }
}

/// Serialize a `PathBuf` as its lossy string rendering, so a worktree path
/// containing non-UTF-8 bytes still produces valid JSON instead of failing
/// the whole document — `PathBuf`'s stock `Serialize` errors on those bytes.
///
/// This IS still lossy, and so still aliases two byte-distinct paths onto one
/// string, exactly as [`display_path`] describes. It is left that way
/// deliberately: `worktree list --json` is a machine surface with a versioned
/// `SCHEMA_VERSION` shape, `reclaim` (the delete decision) has no `--json` at
/// all, and a consumer that needs byte-exactness wants the bytes rather than
/// a human escape — so the right answer here is an additive field and a
/// schema decision, not a silent change to what this one means.
fn serialize_path_lossy<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

/// One examined worktree, ready to render as a human row or a JSON entry.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeReport {
    #[serde(serialize_with = "serialize_path_lossy")]
    pub path: PathBuf,
    pub branch: Option<String>,
    pub clean: bool,
    pub owned: bool,
    pub pr_state: String,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The worktree's real, byte-exact path (issue #144 finding 4) — never
    /// serialized; `path` above (`to_string_lossy`) is what the JSON document
    /// and the human report show. [`run_reclaim`] passes THIS to
    /// [`remove_worktree_dir`] so a non-UTF-8 path can never cause `git
    /// worktree remove` to be handed a different, lossily-mangled path that
    /// happens to resolve to a different registered (or symlinked) worktree.
    #[serde(skip)]
    pub real_path: PathBuf,
}

/// Top-level `--json` document.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeListDocument {
    pub schema_version: u32,
    pub worktrees: Vec<WorktreeReport>,
}

impl WorktreeListDocument {
    pub fn new(worktrees: Vec<WorktreeReport>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            worktrees,
        }
    }
}

struct RawWorktree {
    path: PathBuf,
    branch: Option<String>,
}

/// Parse `git worktree list --porcelain -z` into path/branch pairs. `-z`
/// NUL-terminates each field instead of newline-terminating it, which is
/// what makes this safe: `--porcelain`'s default text mode C-quotes a path
/// containing a newline, a double quote, or other characters it decides to
/// escape, so treating that quoted, human-oriented text as a literal
/// filesystem path silently misparses (or entirely skips) such a worktree.
/// A path can never contain a NUL byte, so splitting on NUL is unambiguous
/// regardless of what the path itself contains. An empty field marks the end
/// of a worktree record, mirroring the blank-line separator `-z` replaces.
/// The first entry is always the main working tree (git's own documented
/// ordering); callers skip it, since the primary checkout is never a reclaim
/// candidate.
fn parse_worktree_porcelain(bytes: &[u8]) -> Vec<RawWorktree> {
    let mut result = Vec::new();
    let mut cur_path: Option<PathBuf> = None;
    let mut cur_branch: Option<String> = None;
    for field in bytes.split(|&b| b == 0) {
        if field.is_empty() {
            if let Some(path) = cur_path.take() {
                result.push(RawWorktree {
                    path,
                    branch: cur_branch.take(),
                });
            }
            continue;
        }
        if let Some(rest) = field.strip_prefix(b"worktree ") {
            if let Some(path) = cur_path.take() {
                result.push(RawWorktree {
                    path,
                    branch: cur_branch.take(),
                });
            }
            cur_path = Some(path_from_bytes(rest));
        } else if let Some(rest) = field.strip_prefix(b"branch ") {
            let rest = String::from_utf8_lossy(rest);
            cur_branch = Some(
                rest.strip_prefix("refs/heads/")
                    .unwrap_or(&rest)
                    .to_string(),
            );
        }
    }
    if let Some(path) = cur_path.take() {
        result.push(RawWorktree {
            path,
            branch: cur_branch.take(),
        });
    }
    result
}

/// Enumerate linked worktrees (excludes the main working tree) for the repo
/// rooted at or above `repo_dir`.
fn list_linked_worktrees(repo_dir: &Path) -> Result<Vec<RawWorktree>, String> {
    let out = Command::new("git")
        .current_dir(repo_dir)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(|e| format!("failed to spawn `git worktree list`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git worktree list` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut all = parse_worktree_porcelain(&out.stdout);
    if !all.is_empty() {
        all.remove(0); // the main working tree
    }
    Ok(all)
}

/// Outcome of probing a worktree's cleanliness. `Unresolvable` is distinct
/// from `Dirty`: both fail closed to `Keep`, but only `Dirty` means the probe
/// actually found uncommitted or untracked content — `Unresolvable` means the
/// probe itself did not run to completion, so a report must never call it
/// "dirty" (nothing was found; the check failed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanliness {
    Clean,
    Dirty,
    Unresolvable(String),
}

fn check_cleanliness(worktree_path: &Path) -> Cleanliness {
    let out = Command::new("git")
        .current_dir(worktree_path)
        .args(["status", "--porcelain"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            if String::from_utf8_lossy(&o.stdout).trim().is_empty() {
                Cleanliness::Clean
            } else {
                Cleanliness::Dirty
            }
        }
        Ok(o) => Cleanliness::Unresolvable(format!(
            "`git status --porcelain` exited with {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => {
            Cleanliness::Unresolvable(format!("failed to spawn `git status --porcelain`: {e}"))
        }
    }
}

/// Whether `worktree_path` holds any gitignored content, via `git status
/// --porcelain --ignored=matching` (issue #144 finding 1): plain
/// `--porcelain` never reports ignored files, so a worktree holding a
/// multi-GB `target/`, a `.env`, or rule-15 `.dot-agent-deck/` scratch reads
/// as `Cleanliness::Clean` even though deleting it destroys that content with
/// no confirmation. This is used ONLY to demote an otherwise-`Remove`
/// verdict to `Ask` — never to change cleanliness itself — so a spawn
/// failure or non-zero exit fails closed to `true` (assume ignored content is
/// present) rather than `false`: the failure mode of an over-cautious `Ask`
/// is an extra `--yes`, never a silent deletion.
fn has_ignored_content(worktree_path: &Path) -> bool {
    let out = Command::new("git")
        .current_dir(worktree_path)
        .args(["status", "--porcelain", "--ignored=matching"])
        .output();
    match out {
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        _ => true,
    }
}

/// Resolve *a* git-dir for `worktree_path` via `git rev-parse --git-dir` run
/// inside it. `None` on any failure to spawn, a non-zero exit, or empty
/// output — callers fail closed on `None`.
///
/// This trusts whatever `<worktree_path>/.git` (or an equivalent absolute
/// `--git-dir`) currently redirects to, which is exactly the trust a single
/// regular file inside the worktree's own working directory should never be
/// given (issue #144 finding 2): `git status --porcelain` never reports
/// `.git` itself, so a party who can write only that one file — no access to
/// the repo's real `.git` required — can redirect it to a copied admin dir
/// carrying a forged ownership marker. [`ownership_of`] is the only caller
/// that treats this result as a security boundary, and it does so ONLY after
/// additionally requiring the result to sit under the enumerating repo's own
/// common dir (see [`resolve_common_dir`]) — a bare call to this function is
/// not, by itself, safe to use for an ownership decision.
fn resolve_git_dir(worktree_path: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .current_dir(worktree_path)
        .args(["rev-parse", "--git-dir"])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return None,
    };
    let raw = trim_trailing_newline(&out.stdout);
    if raw.is_empty() {
        return None;
    }
    let git_dir = path_from_bytes(raw);
    Some(if git_dir.is_absolute() {
        git_dir
    } else {
        worktree_path.join(git_dir)
    })
}

/// Resolve the ENUMERATING repo's own common git directory —
/// `git -C repo_dir rev-parse --path-format=absolute --git-common-dir` — the
/// directory under which every one of ITS linked worktrees' own admin dirs
/// (`<common-dir>/worktrees/<name>`) must live, whether that common dir is
/// the default `<repo>/.git` or a relocated `--separate-git-dir` store.
/// `None` on any failure to spawn, a non-zero exit, or empty output — callers
/// fail closed on `None`.
fn resolve_common_dir(repo_dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .current_dir(repo_dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return None,
    };
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

/// Whether the deck can prove it created `worktree_path`, examined as part of
/// enumerating `repo_dir`: the marker file exists in the worktree's own git
/// metadata dir, AND that metadata dir genuinely sits under `repo_dir`'s own
/// common dir (`<common-dir>/worktrees/<name>` — issue #144 finding 2's
/// containment check). Without the containment check, a worktree whose own
/// `.git` file has been redirected (a single regular file inside the
/// worktree's own directory — no access to the real `.git` required) to a
/// copied admin dir carrying a forged marker would resolve `Ours`; requiring
/// containment under THIS repo's own `<common-dir>/worktrees/` rejects that
/// redirect while still accepting a legitimate `--separate-git-dir` layout,
/// since the common dir there genuinely IS the relocated store. Any failure
/// to resolve either directory, a missing marker, or a resolved git-dir
/// outside containment all resolve to `Foreign` — unknown must never resolve
/// to `Ours`.
fn ownership_of(repo_dir: &Path, worktree_path: &Path) -> Ownership {
    let (Some(git_dir), Some(common_dir)) =
        (resolve_git_dir(worktree_path), resolve_common_dir(repo_dir))
    else {
        return Ownership::Foreign;
    };
    if !git_dir.starts_with(common_dir.join("worktrees")) {
        return Ownership::Foreign;
    }
    if git_dir.join(OWNER_MARKER_FILENAME).is_file() {
        Ownership::Ours
    } else {
        Ownership::Foreign
    }
}

/// Write the `dot-agent-deck-owner` marker for a worktree this process just
/// created via `git worktree add` — the counterpart to [`ownership_of`], and
/// the only writer of this marker anywhere in the tree. Callers: only
/// `issue_dispatch_run::create_worktree` / `create_worktree_sync`, only
/// immediately after a successful add, never for a pre-existing or foreign
/// worktree.
///
/// Best-effort by design, not fail-hard: the marker is metadata for a LATER
/// `reclaim` decision, not a precondition for the worktree being usable now.
/// Callers log-and-continue on `Err` (mirroring `ensure_worktrees_excluded`'s
/// established pattern in this crate) rather than failing the whole worktree
/// creation over it — a worktree that fails to record its own ownership is
/// still a perfectly good worktree; it only means a future `reclaim` will
/// land it on `Ask` instead of `Remove` (annoying, never unsafe, since `Ask`
/// is the fail-closed default `ownership_of` already returns for anything it
/// can't prove).
///
/// No atomic write-then-rename: [`ownership_of`] only checks the marker's
/// PRESENCE (`Path::is_file`), never its content, so there is no
/// partially-written state a concurrent reader could observe as wrong — a
/// short write from e.g. a disk-full mid-write still resolves to `Ours`
/// exactly as a complete write would, and no other writer of this file
/// exists to race against.
pub(crate) fn mark_worktree_owned(worktree_path: &Path) -> Result<(), String> {
    let git_dir = resolve_git_dir(worktree_path).ok_or_else(|| {
        format!(
            "could not resolve git-dir for {} via `git rev-parse --git-dir`",
            worktree_path.display()
        )
    })?;
    std::fs::write(git_dir.join(OWNER_MARKER_FILENAME), "deck\n")
        .map_err(|e| format!("failed to write ownership marker: {e}"))
}

/// Derive a `gh --repo owner/name` slug from the worktree's own `origin`
/// remote — never from `gh`'s own inference, which resolves against the
/// upstream repo when run from a checkout of a GitHub fork that has no
/// default repo configured.
///
/// Fails closed to `None` — the caller turns that into `Unresolvable` (keep,
/// never remove) — on any remote misconfiguration: no `origin` remote, a URL
/// that doesn't parse as `owner/name`, or a host other than `github.com`
/// (`gh` only ever talks to GitHub, so a non-GitHub remote must never resolve
/// to a slug `gh` would misinterpret rather than reject).
fn derive_repo_slug(repo_dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .current_dir(repo_dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_github_owner_repo(&url)
}

/// Parse a GitHub remote URL (HTTPS, `git@` SSH, or `ssh://` SSH) into an
/// `owner/name` slug. Returns `None` for anything else, including a
/// non-GitHub host, a URL with no path, or a path with more than two
/// segments — fail closed rather than guess.
fn parse_github_owner_repo(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// `gh pr list --head <branch> --state all --repo <owner/name> --json
/// state,headRefName,headRepositoryOwner` — `--state all` because `gh pr
/// list` defaults to `--state open`, which makes every merged PR invisible;
/// `--repo`, derived from `origin` via [`derive_repo_slug`], because letting
/// `gh` infer it queries the upstream repo from a fork checkout with no
/// default set.
///
/// Matches results on `headRefName` AND `headRepositoryOwner.login` matching
/// the `origin` slug's own owner (issue #144 finding 2): `headRefName` alone
/// is not namespaced by head repository owner, so a merged PR opened from a
/// DIFFERENT fork with the same head branch name would otherwise be
/// attributed to an unrelated local branch of that name. The owner
/// comparison is ASCII-case-insensitive (issue #144 finding 3 / NEW-1):
/// GitHub logins are case-insensitive and ASCII-only, so a case-variant
/// `origin` remote (`PrageethW` vs. the canonical `prageethw` `gh` returns)
/// must still match. A reply missing the `headRepositoryOwner` field
/// entirely (the shape `gh` can return once the head repo is no longer
/// resolvable, e.g. a fork deleted after merge) is treated as a non-match,
/// not a wildcard — an unverifiable owner must never be treated as a match,
/// the same fail-closed stance every other branch of this gate takes.
///
/// The owner filter is applied as a SEPARATE pass over the `headRefName`
/// matches, not fused into one `.filter()` chain (NEW-2): a `headRefName`
/// match rejected ONLY on owner (the triangular / cross-fork-collision case)
/// must be reported as `Unresolvable` naming the real cause, never as `NoPr`
/// — `NoPr`'s "no pull request found for this branch" is false when a PR
/// with that head ref genuinely exists and was found; it only wasn't
/// confirmed as this repo's own. Zero `headRefName` matches at all resolve to
/// `NoPr` (genuinely no PR exists); more than one surviving owner match
/// resolves to `Unresolvable` (ambiguous), never guessing.
fn resolve_pr_state(repo_dir: &Path, branch: &str) -> PrState {
    let repo_slug = match derive_repo_slug(repo_dir) {
        Some(slug) => slug,
        None => {
            return PrState::Unresolvable(
                "could not derive --repo from the origin remote (missing, or not a parseable \
                 GitHub URL)"
                    .to_string(),
            );
        }
    };
    // `derive_repo_slug` returns "owner/name"; the owner half is what
    // `headRepositoryOwner.login` must match.
    let expected_owner = repo_slug
        .split_once('/')
        .map(|(owner, _)| owner)
        .unwrap_or(repo_slug.as_str());
    let out = Command::new("gh")
        .current_dir(repo_dir)
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--repo",
            &repo_slug,
            "--json",
            "state,headRefName,headRepositoryOwner",
        ])
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return PrState::Unresolvable(format!("gh unavailable: {e}")),
    };
    if !out.status.success() {
        return PrState::Unresolvable(format!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let entries: Vec<serde_json::Value> = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return PrState::Unresolvable(format!("could not parse gh output: {e}")),
    };
    let name_matches: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|v| v.get("headRefName").and_then(|h| h.as_str()) == Some(branch))
        .collect();
    let owner_matches: Vec<&serde_json::Value> = name_matches
        .iter()
        .copied()
        .filter(|v| {
            v.get("headRepositoryOwner")
                .and_then(|o| o.get("login"))
                .and_then(|l| l.as_str())
                .is_some_and(|login| login.eq_ignore_ascii_case(expected_owner))
        })
        .collect();
    match (owner_matches.as_slice(), name_matches.as_slice()) {
        ([], []) => PrState::NoPr,
        ([], _) => PrState::Unresolvable(format!(
            "{} pull request(s) match branch {branch:?} but none has headRepositoryOwner \
             {expected_owner:?} — the head repository owner could not be confirmed",
            name_matches.len()
        )),
        ([one], _) => match one.get("state").and_then(|s| s.as_str()) {
            Some("MERGED") => PrState::Merged,
            Some("OPEN") => PrState::Open,
            Some("CLOSED") => PrState::ClosedUnmerged,
            Some(other) => PrState::Unresolvable(format!("unrecognized PR state {other:?}")),
            None => PrState::Unresolvable("PR entry has no `state` field".to_string()),
        },
        _ => PrState::Unresolvable(format!(
            "{} pull requests matched branch {branch:?} and owner {expected_owner:?}",
            owner_matches.len()
        )),
    }
}

/// Examine every linked worktree of the repo rooted at `repo_dir`: resolve PR
/// state, cleanliness, and ownership for each, and decide its verdict. Pure
/// I/O orchestration; [`decide`] is the tested pure core.
///
/// PR state is resolved against each candidate's OWN path (`wt.path`), never
/// `repo_dir` (the caller's cwd), consistently with `check_cleanliness(&wt.path)`
/// and `ownership_of(repo_dir, &wt.path)`: every per-worktree property is read
/// from the worktree it describes (`repo_dir` is still threaded into
/// `ownership_of` — not as the thing being described, but as the enumerating
/// repo whose common dir the worktree's admin dir must sit under; see
/// `ownership_of`'s doc comment). One concrete case this affects:
/// `remote.<name>.url` is a list-accumulating git config variable, and `git
/// remote get-url` (called by [`derive_repo_slug`]) returns only the first
/// value, so a worktree-scoped `origin` set via `extensions.worktreeConfig`
/// never overrides one already defined in the common config — it only
/// matters when the common config defines no `origin` at all. Resolving
/// against `repo_dir` in that situation yields `Unresolvable`, keeping a
/// worktree forever even though it is genuinely merged and clean;
/// `worktree/reclaim/007` covers exactly this. (Resolving against the wrong
/// repo in general would risk matching an unrelated same-named branch's PR,
/// but that is not a reachable scenario via worktree-scoped remotes — the
/// common config's value always wins.)
///
/// An otherwise-`Remove` verdict (merged, clean, ours) is additionally
/// demoted to `Ask` when [`has_ignored_content`] finds gitignored content
/// still present (issue #144 finding 1) — `Cleanliness::Clean` alone is not
/// enough to remove unprompted, since `git status --porcelain` never reports
/// ignored files. A `Foreign` worktree is already `Ask` regardless, so the
/// check only runs when it can change the outcome.
pub fn examine_worktrees(repo_dir: &Path) -> Result<Vec<WorktreeReport>, String> {
    let raw = list_linked_worktrees(repo_dir)?;
    let mut reports = Vec::with_capacity(raw.len());
    for wt in raw {
        let cleanliness = check_cleanliness(&wt.path);
        let clean = cleanliness == Cleanliness::Clean;
        let owned = ownership_of(repo_dir, &wt.path) == Ownership::Ours;
        let ownership = if owned {
            Ownership::Ours
        } else {
            Ownership::Foreign
        };
        let pr_state = match &wt.branch {
            Some(branch) => resolve_pr_state(&wt.path, branch),
            None => PrState::Unresolvable("worktree has no branch (detached HEAD)".to_string()),
        };
        let verdict = decide(&pr_state, &cleanliness, ownership);
        let verdict = if matches!(verdict, Verdict::Remove) && has_ignored_content(&wt.path) {
            Verdict::Ask(
                "reclaimable: PR is merged and the tree is clean, but the worktree still holds \
                 gitignored content (e.g. target/, .env) that was never part of the merged PR"
                    .to_string(),
            )
        } else {
            verdict
        };
        let real_path = wt.path.clone();
        reports.push(WorktreeReport {
            path: wt.path,
            branch: wt.branch,
            clean,
            owned,
            pr_state: pr_state.label().to_string(),
            reason: verdict.reason().map(str::to_string),
            verdict: verdict.label().to_string(),
            real_path,
        });
    }
    Ok(reports)
}

/// Render a worktree path for human output as an **injective** escape: two
/// paths whose bytes differ can never produce the same string.
///
/// `Path::to_string_lossy` cannot be used for this. It collapses every
/// invalid UTF-8 sequence to `U+FFFD`, so `candidate-\xff` and
/// `candidate-\xfe` — two different directories on disk — print as one
/// identical line, while the removal acts on the distinct byte-exact values.
/// At a surface whose entire purpose is a delete decision, that leaves the
/// operator reading one line while the command acts on another directory
/// (issue #578).
///
/// `Path`'s own `Debug` is exactly the escape wanted, and it is std's rather
/// than hand-rolled: an invalid byte becomes `\xNN`, and — the half a
/// hand-rolled `\xNN` escape usually forgets — a literal backslash becomes
/// `\\`, so a directory genuinely *named* `candidate-\xFF` cannot alias the
/// one holding raw byte `0xFF`. Without that second half the collision is
/// merely relocated. It is also platform-uniform: the same call escapes
/// Windows's unpaired surrogates, so there is no `cfg` split here to drift
/// out of step with `path_from_bytes`'s.
///
/// The surrounding quotes are load-bearing, not cosmetic: they delimit the
/// path, so leading or trailing whitespace in a name is visible at the point
/// of deciding to delete it rather than invisible.
///
/// Note that `escape_debug` also escapes control characters. That is an
/// incidental property of the escape chosen for injectivity, NOT this
/// function's purpose, and it does not close the separate question of
/// terminal-rewriting characters in this output — a different mechanism
/// (bytes that pass through and rewrite the display) needing its own answer.
fn display_path(path: &Path) -> String {
    format!("{path:?}")
}

const DASH: &str = "-";

fn cell(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or(DASH)
}

/// Render the `worktree list` human table: one row per examined worktree,
/// including its verdict and reason so the output is self-explanatory.
pub fn format_list_human(reports: &[WorktreeReport]) -> String {
    if reports.is_empty() {
        return "no worktrees found\n".to_string();
    }
    let mut out = String::new();
    out.push_str("PATH\tBRANCH\tPR\tCLEAN\tOWNED\tVERDICT\tREASON\n");
    for r in reports {
        let path = display_path(&r.path);
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            path,
            cell(&r.branch),
            r.pr_state,
            if r.clean { "yes" } else { "no" },
            if r.owned { "yes" } else { "no" },
            r.verdict,
            r.reason.as_deref().unwrap_or(DASH),
        ));
    }
    out
}

/// Physically remove a worktree directory, preserving its branch:
/// `git -C <repo_dir> worktree remove -- <path>` — deliberately WITHOUT
/// `--force`, since [`examine_worktrees`] already gated on cleanliness; git's
/// own refusal on an unexpectedly dirty tree is a second line of defense
/// rather than something to override. The `--` separator (issue #144 finding
/// 4) is not reachable today — every path here came from `git worktree list`,
/// which always emits absolute paths, so none can start with `-` — but it
/// costs nothing and removes the assumption.
///
/// Takes the real `Path`, never a lossily-converted string (issue #144
/// finding 4): `git worktree remove` realpath/symlink-resolves its argument
/// rather than string-matching it against the registry, so a lossy path that
/// happens to resolve to a DIFFERENT registered worktree removes that one,
/// not merely fails — passing the byte-exact path closes the divergence at
/// its source instead of defending against it downstream. [`WorktreeReport`]
/// still carries a lossy `path: String` for the report/JSON document; only
/// this call site needs the exact bytes.
fn remove_worktree_dir(repo_dir: &Path, worktree_path: &Path) -> Result<(), String> {
    let out = Command::new("git")
        .current_dir(repo_dir)
        .args(["worktree", "remove", "--"])
        .arg(worktree_path)
        .output()
        .map_err(|e| format!("failed to spawn `git worktree remove`: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The full outcome of a `worktree reclaim` run, partitioned by what actually
/// happened to each examined worktree — used to build both the human report
/// and the exit code.
pub struct ReclaimOutcome {
    pub removed: Vec<WorktreeReport>,
    pub pending: Vec<WorktreeReport>,
    pub kept: Vec<WorktreeReport>,
}

/// Run the reclaim gate and act on it: `Remove`-verdict worktrees are removed
/// unconditionally (ownership already proves it's safe); `Ask`-verdict
/// worktrees are removed only when `yes` is true, otherwise added to
/// `pending`; `Keep`-verdict worktrees are left untouched.
pub fn run_reclaim(repo_dir: &Path, yes: bool) -> Result<ReclaimOutcome, String> {
    let reports = examine_worktrees(repo_dir)?;
    let mut removed = Vec::new();
    let mut pending = Vec::new();
    let mut kept = Vec::new();

    for r in reports {
        match r.verdict.as_str() {
            "remove" => match remove_worktree_dir(repo_dir, &r.real_path) {
                Ok(()) => removed.push(r),
                Err(e) => {
                    let mut r = r;
                    r.reason = Some(format!("removal failed: {e}"));
                    kept.push(r);
                }
            },
            "ask" if yes => match remove_worktree_dir(repo_dir, &r.real_path) {
                Ok(()) => removed.push(r),
                Err(e) => {
                    let mut r = r;
                    r.reason = Some(format!("removal failed: {e}"));
                    kept.push(r);
                }
            },
            "ask" => pending.push(r),
            _ => kept.push(r),
        }
    }

    Ok(ReclaimOutcome {
        removed,
        pending,
        kept,
    })
}

/// Render the `worktree reclaim` human report. The ask-surface rules: when
/// a pending decision exists it LEADS the output (never
/// discoverable only by reading past a report), names the exact worktree
/// paths (not a count or category), defaults to keep, and ends with the
/// exact `--yes` command that would proceed — one prompt for the whole batch,
/// not one per worktree. That command's warning is explicit that `--yes`
/// removes worktrees in this state regardless of provenance (issue #144
/// finding 1's documentation half) — the whole reason they are pending
/// confirmation rather than already gone is that the deck cannot prove it
/// created them (or that they still hold gitignored content), and `--yes`
/// overrides exactly that, never anything else about the gate.
///
/// The wording deliberately does NOT say `--yes` acts on "them" — the exact
/// set just printed (reviewer NEW-3): `--yes` re-runs [`run_reclaim`] /
/// [`examine_worktrees`] from scratch and acts on whatever that run finds,
/// not on this report's snapshot. Nothing requires a bare run to have
/// happened first, and content can change between the two invocations, so
/// promising "them" would overstate a binding this command does not have.
pub fn format_reclaim_human(outcome: &ReclaimOutcome) -> String {
    let mut out = String::new();

    if !outcome.pending.is_empty() {
        out.push_str(&format!(
            "{} worktree(s) reclaimable pending confirmation (kept for now):\n",
            outcome.pending.len()
        ));
        for r in &outcome.pending {
            out.push_str(&format!("  - {}\n", display_path(&r.path)));
        }
        out.push_str(
            "Run `dot-agent-deck worktree reclaim --yes` to remove worktrees in this state, \
             regardless of whether the deck created them. The set is re-evaluated on that \
             run.\n\n",
        );
    }

    if !outcome.removed.is_empty() {
        out.push_str("Removed:\n");
        for r in &outcome.removed {
            out.push_str(&format!("  - {}\n", display_path(&r.path)));
        }
    } else {
        out.push_str("Removed: none\n");
    }

    if !outcome.kept.is_empty() {
        out.push_str("Kept:\n");
        for r in &outcome.kept {
            out.push_str(&format!(
                "  - {} ({})\n",
                display_path(&r.path),
                r.reason.as_deref().unwrap_or("no reason recorded")
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_merged_clean_owned_removes() {
        let v = decide(&PrState::Merged, &Cleanliness::Clean, Ownership::Ours);
        assert_eq!(v, Verdict::Remove);
    }

    #[test]
    fn decide_merged_clean_foreign_asks() {
        let v = decide(&PrState::Merged, &Cleanliness::Clean, Ownership::Foreign);
        assert!(matches!(v, Verdict::Ask(_)));
    }

    #[test]
    fn decide_merged_dirty_keeps_regardless_of_ownership() {
        let owned = decide(&PrState::Merged, &Cleanliness::Dirty, Ownership::Ours);
        let foreign = decide(&PrState::Merged, &Cleanliness::Dirty, Ownership::Foreign);
        assert!(matches!(owned, Verdict::Keep(ref r) if r.contains("dirty")));
        assert!(matches!(foreign, Verdict::Keep(ref r) if r.contains("dirty")));
    }

    #[test]
    fn decide_merged_unresolvable_cleanliness_keeps_without_calling_it_dirty() {
        let v = decide(
            &PrState::Merged,
            &Cleanliness::Unresolvable("spawn failed".to_string()),
            Ownership::Ours,
        );
        assert!(
            matches!(v, Verdict::Keep(ref r) if !r.contains("dirty") && r.contains("spawn failed"))
        );
    }

    #[test]
    fn decide_no_pr_ancestor_keeps() {
        // The destructive false-positive this PRD exists to prevent: an
        // ancestor branch with no PR must never be removed, even clean and
        // owned.
        let v = decide(&PrState::NoPr, &Cleanliness::Clean, Ownership::Ours);
        assert!(matches!(v, Verdict::Keep(_)));
    }

    #[test]
    fn decide_open_and_closed_unmerged_keep() {
        assert!(matches!(
            decide(&PrState::Open, &Cleanliness::Clean, Ownership::Ours),
            Verdict::Keep(_)
        ));
        assert!(matches!(
            decide(
                &PrState::ClosedUnmerged,
                &Cleanliness::Clean,
                Ownership::Ours
            ),
            Verdict::Keep(_)
        ));
    }

    #[test]
    fn decide_unresolvable_keeps_never_removes() {
        let v = decide(
            &PrState::Unresolvable("gh not found".to_string()),
            &Cleanliness::Clean,
            Ownership::Ours,
        );
        assert!(matches!(v, Verdict::Keep(_)));
    }

    #[test]
    fn parse_porcelain_skips_nothing_and_strips_refs_prefix() {
        let text = "worktree /repo\0HEAD abc123\0branch refs/heads/main\0\0worktree /repo/wt-a\0HEAD def456\0branch refs/heads/feat/a\0\0";
        let parsed = parse_worktree_porcelain(text.as_bytes());
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, PathBuf::from("/repo"));
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert_eq!(parsed[1].path, PathBuf::from("/repo/wt-a"));
        assert_eq!(parsed[1].branch.as_deref(), Some("feat/a"));
    }

    #[test]
    fn parse_porcelain_preserves_a_path_containing_a_literal_newline() {
        // The exact case newline-delimited parsing cannot handle: a path
        // byte sequence containing `\n` would be C-quoted by `--porcelain`'s
        // default text mode and misparsed (or silently split apart) by a
        // reader that treats each newline as a field terminator. With `-z`,
        // only a NUL byte terminates a field, so a literal `\n` inside the
        // path is just more path content.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"worktree /repo\0HEAD abc123\0branch refs/heads/main\0\0");
        bytes.extend_from_slice(
            b"worktree /repo/wt-\n-embedded\0HEAD def456\0branch refs/heads/feat/weird\0\0",
        );
        let parsed = parse_worktree_porcelain(&bytes);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].path, PathBuf::from("/repo/wt-\n-embedded"));
        assert_eq!(parsed[1].branch.as_deref(), Some("feat/weird"));
    }

    #[test]
    fn json_document_carries_schema_version() {
        let reports = vec![WorktreeReport {
            path: PathBuf::from("/repo/wt-a"),
            branch: Some("feat/a".to_string()),
            clean: true,
            owned: true,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: None,
            real_path: PathBuf::from("/repo/wt-a"),
        }];
        let json = serde_json::to_string(&WorktreeListDocument::new(reports)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        assert!(json.contains("wt-a"));
    }

    /// Build a pending-verdict report for `path`, the shape `format_reclaim_human`
    /// puts in its ask section. `cfg(unix)` because every user is: constructing
    /// a path whose bytes are not valid UTF-8 needs `OsStrExt`, and on Windows
    /// these helpers would be dead code.
    #[cfg(unix)]
    fn pending_report(path: PathBuf) -> WorktreeReport {
        WorktreeReport {
            path,
            branch: Some("feat/x".to_string()),
            clean: true,
            owned: false,
            pr_state: "merged".to_string(),
            verdict: "ask".to_string(),
            reason: Some("reclaimable".to_string()),
        }
    }

    #[cfg(unix)]
    fn pending_bullets(outcome: &ReclaimOutcome) -> Vec<String> {
        format_reclaim_human(outcome)
            .lines()
            .filter(|l| l.starts_with("  - "))
            .map(str::to_string)
            .collect()
    }

    /// The half of injectivity a hand-rolled `\xNN` escape usually forgets, and
    /// which `worktree/reclaim/009` cannot cheaply reach: a directory whose name
    /// literally contains the four ASCII characters `\`, `x`, `F`, `F` must not
    /// render the same as one holding the single raw byte `0xFF`. Escaping only
    /// the invalid bytes and leaving a literal backslash alone relocates the
    /// collision rather than fixing it, and the resulting output looks exactly as
    /// correct as the real fix.
    #[cfg(unix)]
    #[test]
    fn display_path_does_not_alias_a_raw_byte_with_its_literal_escape_text() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let raw_byte = PathBuf::from(OsStr::from_bytes(b"/repo/candidate-\xff"));
        let literal_text = PathBuf::from(r"/repo/candidate-\xFF");
        assert_ne!(
            raw_byte, literal_text,
            "fixture precondition: these must be two genuinely different paths"
        );
        assert_ne!(
            display_path(&raw_byte),
            display_path(&literal_text),
            "a path holding raw byte 0xFF and a path literally named `candidate-\\xFF` are two \
             different directories and must never render alike; got {:?} for both",
            display_path(&raw_byte)
        );
    }

    /// The issue's own reproduction at the unit level: two paths differing in a
    /// single invalid byte must produce two different pending bullets, because
    /// the `--yes` that follows acts on the byte-exact values.
    #[cfg(unix)]
    #[test]
    fn reclaim_pending_bullets_distinguish_paths_differing_only_in_an_invalid_byte() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let outcome = ReclaimOutcome {
            removed: Vec::new(),
            pending: vec![
                pending_report(PathBuf::from(OsStr::from_bytes(b"/repo/candidate-\xff"))),
                pending_report(PathBuf::from(OsStr::from_bytes(b"/repo/candidate-\xfe"))),
            ],
            kept: Vec::new(),
        };
        let bullets = pending_bullets(&outcome);
        assert_eq!(bullets.len(), 2, "got bullets {bullets:?}");
        assert_ne!(
            bullets[0], bullets[1],
            "two byte-distinct worktrees rendered as one identical pending line, so the \
             operator cannot tell which directory `--yes` would remove; got {:?}",
            bullets[0]
        );
    }

    /// The `Removed:` and `Kept:` sections of the same report render through the
    /// same helper, so a path that survives a failed removal is as attributable
    /// as a pending one. Without this, only the ask section would be fixed and
    /// the after-the-fact record would still alias.
    #[cfg(unix)]
    #[test]
    fn reclaim_removed_and_kept_sections_also_distinguish_invalid_byte_paths() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let a = PathBuf::from(OsStr::from_bytes(b"/repo/candidate-\xff"));
        let b = PathBuf::from(OsStr::from_bytes(b"/repo/candidate-\xfe"));
        let outcome = ReclaimOutcome {
            removed: vec![pending_report(a.clone()), pending_report(b.clone())],
            pending: Vec::new(),
            kept: vec![pending_report(a), pending_report(b)],
        };
        let text = format_reclaim_human(&outcome);
        let bullets: Vec<&str> = text.lines().filter(|l| l.starts_with("  - ")).collect();
        assert_eq!(bullets.len(), 4, "got bullets {bullets:?} from:\n{text}");
        assert_ne!(bullets[0], bullets[1], "Removed: section aliased\n{text}");
        assert_ne!(bullets[2], bullets[3], "Kept: section aliased\n{text}");
    }

    /// `worktree list` is where the operator reads the verdicts before running
    /// `reclaim`, so its PATH column must be as attributable as the ask surface's
    /// -- otherwise the two halves of the same decision cannot be matched up.
    /// Also pins that escaping adds no tab, so the row stays seven fields.
    #[cfg(unix)]
    #[test]
    fn list_human_path_column_distinguishes_invalid_byte_paths_and_stays_one_field() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let reports = vec![
            pending_report(PathBuf::from(OsStr::from_bytes(b"/repo/candidate-\xff"))),
            pending_report(PathBuf::from(OsStr::from_bytes(b"/repo/candidate-\xfe"))),
        ];
        let text = format_list_human(&reports);
        let rows: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(rows.len(), 2, "got rows {rows:?} from:\n{text}");
        for row in &rows {
            assert_eq!(
                row.split('\t').count(),
                7,
                "the escaped path must stay a single tab-separated field; got {row:?}"
            );
        }
        assert_ne!(
            rows[0].split('\t').next(),
            rows[1].split('\t').next(),
            "the PATH column aliased two byte-distinct worktrees\n{text}"
        );
    }

    /// A path that is ordinary valid UTF-8 must still read as itself, escapes
    /// notwithstanding -- the fix must not make the common case unrecognisable.
    #[test]
    fn display_path_keeps_an_ordinary_path_readable() {
        let rendered = display_path(&PathBuf::from("/home/me/code/repo-feature"));
        assert!(
            rendered.contains("/home/me/code/repo-feature"),
            "an all-ASCII path must appear verbatim inside its rendering; got {rendered:?}"
        );
    }

    // -------------------------------------------------------------------
    // Production creation-path coverage (issue #144): a worktree made via
    // the deck's REAL `create_worktree_sync` (not the `mark_owned` test
    // helper `tests/worktree_reclaim.rs` uses for its own fixtures) must be
    // `Verdict::Remove` and be removed by a bare `reclaim`. This drives
    // `resolve_pr_state`'s real `gh` call, which offers no argument to swap
    // in a stub, so it needs a real (stubbed) `gh` on `PATH` -- unlike
    // `tests/worktree_reclaim.rs`'s fixture, which scopes its stub `PATH` to
    // a spawned `dot-agent-deck` *subprocess*'s own environment, an
    // in-process unit test has no such boundary and must mutate this
    // process's `PATH` directly. `GH_PATH_ENV_LOCK` below serializes that
    // mutation against other tests IN THIS MODULE that do the same -- it
    // does NOT make the mutation sound against production readers (see
    // `PathEnvGuard::prepend`'s SAFETY comment and `hook.rs:585` for why
    // `config.rs`'s `STATE_DIR_ENV_LOCK` is not a convention to cite as
    // justification here).
    // -------------------------------------------------------------------

    use spec::spec;

    /// Serializes tests in this module that mutate the process-global `PATH`
    /// to stand a fake `gh` in front of `resolve_pr_state`'s real
    /// `Command::new("gh")` call. Only this module's tests spawn `gh` at
    /// all (`grep 'Command::new("gh")' src/*.rs`), so this lock only ever
    /// needs to serialize against itself, but it stays cheap insurance
    /// against a future sibling test doing the same.
    static GH_PATH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: prepends `bindir` to `PATH` (so the real `git` the
    /// creation path also needs stays resolvable) and restores the prior
    /// value on drop, even on panic. Callers must hold `GH_PATH_ENV_LOCK`
    /// for this guard's entire lifetime.
    struct PathEnvGuard {
        prev_path: Option<String>,
    }

    impl PathEnvGuard {
        fn prepend(bindir: &Path) -> Self {
            let prev_path = std::env::var("PATH").ok();
            let new_path = match &prev_path {
                Some(p) => format!("{}:{p}", bindir.display()),
                None => bindir.display().to_string(),
            };
            // SAFETY: sound only because `cargo nextest` gives each test its
            // own process (see `.github/workflows/ci.yml`: nextest, NOT
            // `cargo test`, which was tried and flaked for exactly this
            // reason) -- with one test per process there is no second
            // thread in this process to race the read side. GH_PATH_ENV_LOCK
            // is NOT what makes this sound: it only serializes this
            // module's own tests against EACH OTHER, and cannot serialize
            // against libc's environ read on every `Command::spawn` in this
            // process, which never takes this (or any) lock. That is the
            // same objection upstream raised on vfarcic#419, and
            // `hook.rs:585` already records it for this crate's other env
            // locks -- this comment is not an endorsement of the pattern.
            unsafe {
                std::env::set_var("PATH", new_path);
            }
            Self { prev_path }
        }
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            // SAFETY: see PathEnvGuard::prepend.
            unsafe {
                match self.prev_path.take() {
                    Some(p) => std::env::set_var("PATH", p),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    /// A real, minimal git repo (`main` branch, one seed commit, an `origin`
    /// remote resolvable by `derive_repo_slug`) to drive the production
    /// worktree-creation path against.
    fn init_repo_with_origin(dir: &Path) {
        fn git(dir: &Path, args: &[&str]) {
            let out = std::process::Command::new("git")
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
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "--initial-branch=main", "--quiet"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        git(dir, &["add", "README.md"]);
        git(dir, &["commit", "--quiet", "-m", "seed"]);
        git(
            dir,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/test-org/test-repo.git",
            ],
        );
    }

    /// Scenario: A worktree is created through the deck's own PRODUCTION
    /// creation path (`issue_dispatch_run::create_worktree_sync`, the same
    /// function the TUI's `SpawnPane` dispatch calls) against a real git
    /// repo, with a MERGED PR fixture answered by a stub `gh`. It must
    /// resolve to `Verdict::Remove` and actually be removed by a BARE
    /// `reclaim` (no `--yes`) -- proving the creation path itself writes the
    /// `dot-agent-deck-owner` marker, not that a test helper can fake one.
    #[spec("worktree/reclaim/008")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_008_deck_created_worktree_is_removed_by_bare_reclaim() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        // Unconditionally answers `gh pr list` with one canned MERGED reply.
        // This test does not re-check `gh` invocation shape --
        // `tests/worktree_reclaim.rs`'s fixture already pins that at the CLI
        // layer -- it only needs a MERGED verdict to reach `Verdict::Remove`.
        let branch = "feat/deck-created";
        let gh_script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then\n    printf '%s\\n' \
             '[{{\"state\":\"MERGED\",\"headRefName\":\"{branch}\",\"headRepositoryOwner\":{{\"login\":\"test-org\"}}}}]'\n    \
             exit 0\nfi\nexit 1\n"
        );
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let gh_path = bindir.join("gh");
        std::fs::write(&gh_path, gh_script).unwrap();
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _path_guard = PathEnvGuard::prepend(&bindir);

        let worktree_dir = scratch.path().join("wt-deck-created");
        let creation =
            crate::issue_dispatch_run::create_worktree_sync(&repo, &worktree_dir, branch)
                .expect("create_worktree_sync must succeed against a real git repo");
        assert_eq!(
            creation,
            crate::issue_dispatch_run::WorktreeCreation::Created,
            "the production creation path must report Created for a fresh worktree dir, got {creation:?}"
        );
        assert!(
            worktree_dir.exists(),
            "create_worktree_sync reported Created but the worktree directory is missing"
        );

        let outcome =
            run_reclaim(&repo, false).expect("run_reclaim must succeed against a real git repo");
        assert_eq!(
            outcome.removed.len(),
            1,
            "a worktree created through the production creation path, with a MERGED PR and a \
             clean tree, must be removed by a BARE `reclaim` (no --yes) -- this only holds once \
             `create_worktree_sync` itself writes the ownership marker; got removed={:?} \
             pending={:?} kept={:?}",
            outcome.removed,
            outcome.pending,
            outcome.kept
        );
        assert!(
            !worktree_dir.exists(),
            "the worktree directory must actually be gone after the bare reclaim above, not \
             merely reported as removed"
        );
    }
}
