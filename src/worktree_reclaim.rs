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
    /// The identity [`owner_of`] read back from the marker, when the
    /// worktree is `Ours` and the marker carries a `created-by:` line.
    /// `None` for a `Foreign` worktree, and `None` for an `Ours` worktree
    /// whose marker predates fork #166 (the bare `"deck\n"` legacy content)
    /// — omitted from JSON entirely rather than serialized as `null`,
    /// mirroring `reason` above, so an older client reading this document
    /// still round-trips cleanly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
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
/// carrying a forged ownership marker. [`owned_git_dir`] is the only caller
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

/// Resolve `worktree_path`'s own git metadata dir and containment-check it
/// against `repo_dir`'s common dir (`<common-dir>/worktrees/<name>` — issue
/// #144 finding 2's containment check), returning it only if contained.
/// Without the containment check, a worktree whose own `.git` file has been
/// redirected (a single regular file inside the worktree's own directory —
/// no access to the real `.git` required) to a copied admin dir carrying a
/// forged marker would resolve as contained; requiring containment under
/// THIS repo's own `<common-dir>/worktrees/` rejects that redirect while
/// still accepting a legitimate `--separate-git-dir` layout, since the
/// common dir there genuinely IS the relocated store. Any failure to
/// resolve either directory, or a resolved git-dir outside containment,
/// returns `None` — unknown must never resolve to contained.
///
/// The sole resolution point for both [`ownership_of`] and [`owner_of`],
/// specifically so they agree on which directory the containment check
/// applies to. Before this helper existed, `owner_of` re-ran its own
/// independent `git rev-parse --git-dir` after `ownership_of` had already
/// resolved and containment-checked one — two separate subprocess calls,
/// each re-reading the worktree's own `.git` redirect, so a party able to
/// write that one file could flip it between the two calls and have the
/// containment check pass against the real admin dir while the content read
/// landed in a forged one it controlled (fork #166 P2 / reviewer F5).
/// Routing both through this single resolution closes that window: whatever
/// directory passed containment is the directory whose content gets read.
fn owned_git_dir(repo_dir: &Path, worktree_path: &Path) -> Option<PathBuf> {
    let (Some(git_dir), Some(common_dir)) =
        (resolve_git_dir(worktree_path), resolve_common_dir(repo_dir))
    else {
        return None;
    };
    if !git_dir.starts_with(common_dir.join("worktrees")) {
        return None;
    }
    Some(git_dir)
}

/// Whether the deck can prove it created `worktree_path`, examined as part
/// of enumerating `repo_dir`: [`owned_git_dir`] resolves and
/// containment-checks the worktree's own git metadata dir, and the marker
/// file must exist there. Any resolution failure or containment miss
/// already returns `None` from [`owned_git_dir`], which this maps to
/// `Foreign` — unknown must never resolve to `Ours`.
fn ownership_of(repo_dir: &Path, worktree_path: &Path) -> Ownership {
    match owned_git_dir(repo_dir, worktree_path) {
        Some(git_dir) if git_dir.join(OWNER_MARKER_FILENAME).is_file() => Ownership::Ours,
        _ => Ownership::Foreign,
    }
}

/// Upper bound on bytes read from a `dot-agent-deck-owner` marker file.
/// [`sanitize_marker_creator`]'s 200-char cap means the writer never emits
/// more than a few hundred bytes total, but [`owner_of`] reads a file
/// anything with write access to the worktree's admin dir could have
/// replaced with arbitrary content — capping the read at a fixed worst case
/// (not at whatever the file actually contains) means a marker grown to
/// gigabytes cannot turn `worktree list` / `reclaim` into an out-of-memory,
/// once per worktree, in a loop.
const MARKER_READ_MAX_BYTES: u64 = 4096;

/// Read and parse the `created-by:` identity out of a marker file already
/// known to exist at `marker_path`. Bounds the read at
/// [`MARKER_READ_MAX_BYTES`] (checked via a `metadata` call before the
/// actual read, so an oversized file is never loaded into memory at all)
/// and, per #173's documented contract, finds the line starting with the
/// literal `created-by: ` prefix and strips exactly that prefix — never
/// `split(':')`, since every real value embeds a second colon
/// (`issue-dispatch:<task>#<issue>`, `orchestration:<name>`) that splitting
/// would truncate at.
///
/// Unlike the writer, the bytes read back here are NOT trusted: this file
/// can be replaced by anything with write access to the worktree's admin
/// dir, so [`sanitize_marker_creator`] — the writer's own sanitizer — is
/// applied to the parsed value before it is returned. It filters
/// `char::is_control()` (Unicode category Cc: `\n`, `\r`, and other C0/C1
/// controls) only. It does **not** filter category-Cf invisible formatting
/// characters such as U+200B (zero-width space), U+202E (RTL override) or
/// U+FEFF (BOM) — those sit outside `White_Space` too (which stops at
/// U+200A), so they survive both the sanitizer and `.trim()` and can reach a
/// future rendered `worktree list` column or a `jq -r` pipeline unfiltered.
/// This is latent today (no consumer renders `owner` yet) and becomes live
/// once M2.3 adds a human-facing `OWNER` column — see fork #166 N2/auditor
/// re-audit; closing the Cf gap itself is deliberately out of scope here. An
/// empty value after stripping the prefix and trimming becomes `None`
/// (unknown), never `Some("")` — `sanitize_marker_creator`'s own "unknown"
/// floor means the writer can never produce an empty owner, so a `Some("")`
/// read back would only ever be a hand-written or corrupted marker
/// asserting a known-and-empty identity, a state no consumer was designed
/// for. A pre-#166 marker (the bare `"deck\n"` an older build wrote) has no
/// `created-by:` line at all and correctly resolves `None` here too — `Ours`
/// with an unknown owner, not an error.
fn read_marker_owner(marker_path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(marker_path).ok()?;
    if !metadata.is_file() || metadata.len() > MARKER_READ_MAX_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(marker_path).ok()?;
    content
        .lines()
        .find_map(|line| line.strip_prefix("created-by: "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(sanitize_marker_creator)
}

/// Read the `created-by:` identity back out of a worktree's ownership
/// marker (issue #425's write side; fork #166's read side). Resolves and
/// containment-checks the worktree's git-dir via [`owned_git_dir`] — the
/// same resolution [`ownership_of`] uses — so a `Foreign` worktree (missing
/// marker, forged `.git` redirect, unresolvable git-dir) usually reports
/// `None` here too, and the directory that passed containment is the exact
/// directory [`read_marker_owner`] reads from. That is not unconditional,
/// though: because this is a second, independent [`owned_git_dir`]
/// resolution rather than a shared one (see below), the auditor measured 24
/// of 120 adversarial cases landing `owned=false` from [`ownership_of`]
/// alongside a non-`None` `owner` from this function (fork #166 N3) — the
/// two disagreeing rather than both reporting unknown. That disagreement is
/// accepted as cosmetic for now (no consumer treats `owner`'s mere presence
/// as an ownership signal), but is a contract worth pinning explicitly
/// before M1.0 makes this identity load-bearing.
///
/// [`examine_worktrees`] calls this immediately after [`ownership_of`], with
/// no I/O of any kind — no `gh` call, no other filesystem work — in between,
/// specifically so the two independent [`owned_git_dir`] resolutions they
/// each perform sit back-to-back rather than spanning the seconds-wide `gh`
/// network call that used to separate them (reviewer F5). That is a
/// narrowed window, not a closed one: each [`owned_git_dir`] call spawns two
/// `git rev-parse` subprocesses ([`resolve_git_dir`] and
/// [`resolve_common_dir`]), so the two back-to-back calls here span **four**
/// spawns, not two, and the window is wide enough that the auditor measured
/// a hostile flipper winning it 54 of 120 times (45%). Collapsing them into
/// one evaluation would need `owner_of`'s public signature to change to
/// accept an already-resolved git-dir, which would also change what
/// `worktree/reclaim/017`–`022` call — out of scope for this round.
pub(crate) fn owner_of(repo_dir: &Path, worktree_path: &Path) -> Option<String> {
    let git_dir = owned_git_dir(repo_dir, worktree_path)?;
    read_marker_owner(&git_dir.join(OWNER_MARKER_FILENAME))
}

/// Write the `dot-agent-deck-owner` marker for a worktree this process just
/// created via `git worktree add` — the counterpart to [`ownership_of`], and
/// the only writer of this marker anywhere in the tree. Callers: only
/// `issue_dispatch_run::create_worktree` / `create_worktree_sync`, only
/// immediately after a successful add, never for a pre-existing or foreign
/// worktree.
///
/// `creator` names the task or orchestration that created the worktree
/// (issue #425) — e.g. `"issue-dispatch:<task>#<issue>"` or
/// `"orchestration:<name>"`. It is sanitised via
/// [`sanitize_marker_creator`] before being written, since it is
/// user-controlled config (a task name from `.dot-agent-deck.toml`, or an
/// orchestration tab name typed into the TUI) landing in a file this crate
/// later reads back.
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
/// No atomic write-then-rename for the `Ours`/`Foreign` decision:
/// [`ownership_of`] only checks the marker's PRESENCE (`Path::is_file`),
/// never its content, so there is no partially-written state a concurrent
/// `ownership_of` reader could observe as wrong — a short write from e.g. a
/// disk-full mid-write still resolves to `Ours` exactly as a complete write
/// would. This also means an older-build marker — the bare `"deck\n"` this
/// function used to write before issue #425 — still resolves to `Ours`:
/// `ownership_of` never inspects content, so a first line other than `deck`
/// would be the only way to regress that, and this function always writes
/// `deck` first for exactly that reason. Re-marking an already-marked
/// worktree (idempotent by construction: `std::fs::write` truncates rather
/// than appends) simply overwrites the identity line rather than
/// accumulating one per call.
///
/// **This no-longer covers [`owner_of`], which fork #166 added as this
/// file's first content reader.** A short write (disk full, or the process
/// killed between the two lines `format!` produces) still resolves `Ours`
/// exactly as documented above, but `owner_of` now returns whatever prefix
/// of the identity happened to land on disk — a silently truncated value
/// reported as authoritative in `worktree list --json`, with nothing to
/// distinguish it from a genuine, complete identity. Still not asking for
/// atomic writes here: the consequence today is a display-only
/// misattribution, not a removal-gate bypass (`owner` never feeds `decide`
/// or `remove_worktree_dir`). But once an orchestration matches its own
/// worktrees on this identity (the PRD's later milestones), a truncated
/// read becomes a failed match rather than a cosmetic one, and that is the
/// point at which an atomic write stops being optional.
pub(crate) fn mark_worktree_owned(worktree_path: &Path, creator: &str) -> Result<(), String> {
    let git_dir = resolve_git_dir(worktree_path).ok_or_else(|| {
        format!(
            "could not resolve git-dir for {} via `git rev-parse --git-dir`",
            worktree_path.display()
        )
    })?;
    let content = format!("deck\ncreated-by: {}\n", sanitize_marker_creator(creator));
    std::fs::write(git_dir.join(OWNER_MARKER_FILENAME), content)
        .map_err(|e| format!("failed to write ownership marker: {e}"))
}

/// Neutralise a creator identity before it is written into the
/// `dot-agent-deck-owner` marker (issue #425). The marker's format is one
/// field per line (`deck` on line 1, `created-by: <identity>` on line 2), so
/// an embedded newline or carriage return in a hand-edited task name or
/// TUI-typed orchestration name would otherwise inject a bogus extra line;
/// collapsing both to a space keeps the two-line shape intact. Every other
/// C0/DEL control character is dropped outright, since nothing about this
/// name benefits from being reproduced byte-for-byte and a stray control
/// character (that also can't render as anything a person would attribute
/// meaning to) has no reason to survive into a file this crate keeps around
/// indefinitely. Mirrors `issue_dispatch::sanitize_claimant_name`'s
/// reasoning (PRD #421) for a different sink — a local file rather than a
/// public GitHub comment — so no CommonMark-specific escaping (backticks,
/// `@`-mentions) is needed here. Every real `created-by` value embeds a
/// second colon after the prefix (`issue-dispatch:<task>#<issue>`,
/// `orchestration:<name>`), so a future reader must strip the literal
/// `created-by: ` prefix and treat the remainder as opaque, never
/// `split(':')` — see the module-level format doc.
///
/// The result is trimmed and capped at [`MARKER_CREATOR_MAX_CHARS`],
/// mirroring the length bound `scheduler::sanitize_claimant_for_render`
/// already applies to its own mirror sink. An empty or all-control-character
/// input — e.g. `validate_task` never checks `ScheduledTask.name`, so a
/// blank task name is a loadable schedule — collapses to the literal
/// `"unknown"` rather than leaving a bare `created-by: ` with no identity
/// after the prefix.
const MARKER_CREATOR_MAX_CHARS: usize = 200;

fn sanitize_marker_creator(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    if trimmed.chars().count() > MARKER_CREATOR_MAX_CHARS {
        let truncated: String = trimmed.chars().take(MARKER_CREATOR_MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        trimmed.to_string()
    }
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
/// and `ownership_of(repo_dir, &wt.path)` / `owner_of(repo_dir, &wt.path)`:
/// every per-worktree property is read from the worktree it describes
/// (`repo_dir` is still threaded into both — not as the thing being
/// described, but as the enumerating repo whose common dir the worktree's
/// admin dir must sit under; see `owned_git_dir`'s doc comment). `owner_of`
/// is called immediately after `ownership_of`, with no other work — no `gh`
/// call, no other I/O — in between; see `owner_of`'s doc comment for why
/// that ordering matters (reviewer F5). One concrete case this affects:
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
        let owner = owner_of(repo_dir, &wt.path);
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
            owner,
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
            // fork #166: WorktreeReport grows an `owner` field alongside
            // `owned` -- this literal is updated so the pre-existing test
            // keeps compiling once that field lands; it isn't itself
            // assertion coverage for the field's content (see
            // worktree_reclaim_021 for that).
            owner: None,
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
        let creation = crate::issue_dispatch_run::create_worktree_sync(
            &repo,
            &worktree_dir,
            branch,
            "test-creator",
        )
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

    /// Scenario: `create_worktree_sync` (the sync creation path both the
    /// TUI's `SpawnPane` dispatch and this module's own test above drive) is
    /// asked to create a worktree with a specific creator identity. The
    /// written marker must record that identity (issue #425), not just the
    /// bare fact that a deck made it.
    #[test]
    fn mark_worktree_owned_records_creator_identity_sync_path() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let worktree_dir = scratch.path().join("wt-with-creator");
        let creation = crate::issue_dispatch_run::create_worktree_sync(
            &repo,
            &worktree_dir,
            "feat/creator-identity",
            "issue-dispatch:my-task#42",
        )
        .expect("create_worktree_sync must succeed against a real git repo");
        assert_eq!(
            creation,
            crate::issue_dispatch_run::WorktreeCreation::Created
        );

        let git_dir = resolve_git_dir(&worktree_dir).expect("must resolve the worktree's git-dir");
        let content = std::fs::read_to_string(git_dir.join(OWNER_MARKER_FILENAME))
            .expect("marker file must exist and be readable");
        assert!(
            content.starts_with("deck\n"),
            "the bare `deck` first line must be preserved for older-reader compatibility, \
             got {content:?}"
        );
        assert!(
            content.contains("created-by: issue-dispatch:my-task#42"),
            "the marker must record the creator identity, got {content:?}"
        );
    }

    /// Scenario: a marker written by an OLDER build -- the literal
    /// `"deck\n"` this crate wrote before issue #425, with no creator line
    /// at all -- must still resolve as deck-owned. `ownership_of` only
    /// checks the marker's PRESENCE, never its content, so this holds by
    /// construction; this test pins that down explicitly rather than
    /// leaving it implicit, since a future change to `ownership_of` that
    /// started parsing content could silently stop reclaiming every
    /// worktree marked before this change shipped.
    #[test]
    fn bare_deck_marker_from_older_build_still_reads_as_ours() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let worktree_dir = scratch.path().join("wt-old-marker");
        let creation = crate::issue_dispatch_run::create_worktree_sync(
            &repo,
            &worktree_dir,
            "feat/old-marker",
            "whatever this build would have recorded",
        )
        .expect("create_worktree_sync must succeed against a real git repo");
        assert_eq!(
            creation,
            crate::issue_dispatch_run::WorktreeCreation::Created
        );

        // Overwrite with the OLDER, bare-form marker content -- simulating a
        // worktree marked by a build that predates issue #425.
        let git_dir = resolve_git_dir(&worktree_dir).expect("must resolve the worktree's git-dir");
        std::fs::write(git_dir.join(OWNER_MARKER_FILENAME), "deck\n")
            .expect("overwrite with bare older-build marker");

        assert_eq!(
            ownership_of(&repo, &worktree_dir),
            Ownership::Ours,
            "a bare `deck\\n` marker from an older build must still resolve as deck-owned"
        );
    }

    /// Scenario: a creator identity containing newlines, carriage returns,
    /// and other control characters -- a hand-edited `.dot-agent-deck.toml`
    /// task name, or a TUI-typed orchestration name, both of which carry no
    /// character restriction -- must not corrupt the marker's two-line
    /// format or its content when written and read back.
    #[test]
    fn hostile_creator_name_is_sanitised_without_corrupting_the_marker() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let worktree_dir = scratch.path().join("wt-hostile-creator");
        let hostile = "line-one\nline-two\rline-three\u{7}\u{1b}[31mred";
        let creation = crate::issue_dispatch_run::create_worktree_sync(
            &repo,
            &worktree_dir,
            "feat/hostile",
            hostile,
        )
        .expect("create_worktree_sync must succeed against a real git repo");
        assert_eq!(
            creation,
            crate::issue_dispatch_run::WorktreeCreation::Created
        );

        let git_dir = resolve_git_dir(&worktree_dir).expect("must resolve the worktree's git-dir");
        let content = std::fs::read_to_string(git_dir.join(OWNER_MARKER_FILENAME))
            .expect("marker file must exist and be readable");

        assert_eq!(
            content.lines().count(),
            2,
            "an embedded newline/carriage-return in the creator name must not add extra lines, \
             got {content:?}"
        );
        assert!(
            !content.contains('\u{7}') && !content.contains('\u{1b}'),
            "control characters must be dropped, not reproduced verbatim, got {content:?}"
        );
        assert_eq!(
            ownership_of(&repo, &worktree_dir),
            Ownership::Ours,
            "a marker written with a hostile creator name must still read as deck-owned"
        );
    }

    /// Scenario: `sanitize_marker_creator` is fed an empty string, a
    /// whitespace/control-only string, and a creator identity far longer
    /// than the cap. The empty and all-stripped cases must both record an
    /// explicit `"unknown"` rather than a blank identity after the
    /// `created-by: ` prefix, and the over-length case must be truncated
    /// at [`MARKER_CREATOR_MAX_CHARS`] rather than grow the marker file
    /// without bound.
    #[test]
    fn sanitize_marker_creator_bounds_and_guards() {
        assert_eq!(sanitize_marker_creator(""), "unknown");
        assert_eq!(sanitize_marker_creator("   "), "unknown");
        assert_eq!(sanitize_marker_creator("\u{7}\u{1b}"), "unknown");

        let long = "a".repeat(MARKER_CREATOR_MAX_CHARS + 50);
        let sanitized = sanitize_marker_creator(&long);
        assert_eq!(
            sanitized.chars().count(),
            MARKER_CREATOR_MAX_CHARS + 1,
            "must truncate to the cap plus the trailing ellipsis marker, got {} chars",
            sanitized.chars().count()
        );
        assert!(
            sanitized.ends_with('…'),
            "an over-length creator name must be marked as truncated, got {sanitized:?}"
        );

        assert_eq!(
            sanitize_marker_creator("  orchestration:worktree-demo  "),
            "orchestration:worktree-demo",
            "ordinary input must still be trimmed but otherwise passed through"
        );
    }

    // -------------------------------------------------------------------
    // Fork #166: the owner recorded in the marker is queryable, not just a
    // bare `Ours`/`Foreign` bit. `owner_of` is deliberately a NEW, separate
    // function rather than a change to `Ownership`'s shape: embedding the
    // name into `Ownership::Ours` would ripple into `decide`'s five
    // existing pure-gate tests above (`decide_merged_clean_owned_removes`
    // et al., which construct a bare `Ownership::Ours` with no payload) and
    // into `decide`'s own match arms, none of which have anything to do
    // with WHO owns a worktree -- only whether the deck can prove IT does.
    // Keeping `Ownership`/`ownership_of` untouched means that blast radius
    // is zero; `owner_of` layers the identity question on top.
    // -------------------------------------------------------------------

    /// A linked worktree via a real `git worktree add`, independent of the
    /// deck's own creation path -- these tests only care about the marker
    /// file's presence/content, not how the worktree came to exist.
    fn add_worktree(repo: &Path, worktree_dir: &Path, branch: &str) {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args([
                "worktree",
                "add",
                "-b",
                branch,
                &worktree_dir.display().to_string(),
            ])
            .output()
            .unwrap_or_else(|e| panic!("git worktree add failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Scenario: A worktree created via a real `git worktree add` has its
    /// ownership marker written with an explicit orchestration name via
    /// `mark_worktree_owned`. `owner_of` must report that exact name back,
    /// and `ownership_of`'s existing `Ours`/`Foreign` bit must agree it is
    /// owned -- the identity fork #166 exists to make queryable, not just a
    /// bare yes/no.
    #[spec("worktree/reclaim/017")]
    #[test]
    fn worktree_reclaim_017_owner_recorded_and_reported() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);
        let wt = scratch.path().join("wt-x");
        add_worktree(&repo, &wt, "feat/x");

        mark_worktree_owned(&wt, "orch-x").expect("mark_worktree_owned must succeed");

        assert_eq!(
            owner_of(&repo, &wt),
            Some("orch-x".to_string()),
            "a worktree marked owned by 'orch-x' must report exactly that name back"
        );
        assert_eq!(
            ownership_of(&repo, &wt),
            Ownership::Ours,
            "the existing Ours/Foreign bit must still agree the worktree is owned"
        );
    }

    /// Scenario: A worktree carries the bare `"deck\n"` marker with no
    /// `created-by:` line -- the exact legacy content #173's own
    /// `bare_deck_marker_from_older_build_still_reads_as_ours` test (above)
    /// already pins as resolving `Ours`. That test covers the PRESENCE
    /// half; #173 has no `owner_of` to cover the READ half, since that
    /// function doesn't exist on its side of the fork. Same fixture,
    /// different question: the same bare marker must report the owner as
    /// unknown (`None`) rather than error or resolve `Foreign`. This is
    /// what stops every worktree created before this ships from silently
    /// becoming un-reclaimable.
    #[spec("worktree/reclaim/018")]
    #[test]
    fn worktree_reclaim_018_legacy_marker_resolves_ours_owner_unknown() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let wt_legacy = scratch.path().join("wt-legacy");
        add_worktree(&repo, &wt_legacy, "feat/legacy");
        let git_dir_legacy = resolve_git_dir(&wt_legacy).expect("resolve git dir");
        std::fs::write(git_dir_legacy.join(OWNER_MARKER_FILENAME), "deck\n").unwrap();

        assert_eq!(
            ownership_of(&repo, &wt_legacy),
            Ownership::Ours,
            "a pre-#166 marker (literal legacy content \"deck\\n\") must still resolve Ours"
        );
        assert_eq!(
            owner_of(&repo, &wt_legacy),
            None,
            "a pre-#166 marker never encoded an owner name -- it must report unknown, not \
             \"deck\""
        );
    }

    /// Scenario: `main` itself -- the enumerating repo's own checkout, not a
    /// linked worktree -- is checked for ownership, even after a marker is
    /// planted directly in ITS `.git` directory and even when the repo's own
    /// directory name matches the `<name>-<change>` convention this PRD
    /// introduces for linked worktrees. It must resolve `Foreign`
    /// regardless: fork #144's containment check already guarantees this
    /// (the main checkout's git-dir sits ABOVE `.git/worktrees`, so it can
    /// never satisfy the `starts_with` check) -- this is a regression guard
    /// pinning that existing guarantee, not new behavior this PRD adds, and
    /// it needs no new API to pass.
    #[spec("worktree/reclaim/019")]
    #[test]
    fn worktree_reclaim_019_main_is_never_owned_even_if_named_like_a_worktree_or_marked() {
        let scratch = tempfile::tempdir().unwrap();
        // Deliberately named to match the `<name>-<change>` convention --
        // containment must still reject it, since naming carries no
        // authority (only the marker's location does).
        let repo = scratch.path().join("myorch-feature");
        init_repo_with_origin(&repo);

        // Plant a marker directly in main's OWN git-dir (not under
        // .git/worktrees/) -- if containment were ever weakened this would
        // be exactly the forged-ownership shape it must still reject.
        let git_dir = resolve_git_dir(&repo).expect("resolve git dir for main checkout");
        std::fs::write(git_dir.join(OWNER_MARKER_FILENAME), "deck\n").unwrap();

        assert_eq!(
            ownership_of(&repo, &repo),
            Ownership::Foreign,
            "main must never resolve Ours, even named like a worktree and even with a marker \
             planted directly in its own git-dir"
        );
    }

    /// Scenario: Three worktrees in one repo -- one marked owned by
    /// orchestration `Y`, and one carrying NO marker at all but named as if
    /// it belonged to `X` (`X-decoy`). `Y`'s worktree must report owner `Y`
    /// (never `X`), and the unmarked, X-named directory must resolve
    /// `Foreign`/unknown regardless of its name -- naming carries no
    /// authority, only the marker's presence and content do.
    #[spec("worktree/reclaim/020")]
    #[test]
    fn worktree_reclaim_020_ownership_is_per_owner_never_by_name() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let wt_y = scratch.path().join("wt-y-owned");
        add_worktree(&repo, &wt_y, "feat/y-owned");
        mark_worktree_owned(&wt_y, "Y").expect("mark_worktree_owned must succeed");

        let owner = owner_of(&repo, &wt_y);
        assert_eq!(
            owner,
            Some("Y".to_string()),
            "a worktree marked owned by Y must report Y as owner"
        );
        assert_ne!(
            owner,
            Some("X".to_string()),
            "a worktree owned by Y must never be reported as owned by a different name X"
        );

        // Named as if it belonged to X's `<name>-<change>` convention, but no
        // marker was ever written -- ownership must not be inferred from the
        // name.
        let wt_no_marker = scratch.path().join("X-decoy");
        add_worktree(&repo, &wt_no_marker, "feat/x-decoy");
        assert_eq!(
            ownership_of(&repo, &wt_no_marker),
            Ownership::Foreign,
            "a directory with no marker is never owned, whatever it is named"
        );
        assert_eq!(
            owner_of(&repo, &wt_no_marker),
            None,
            "a directory with no marker has no owner, whatever it is named"
        );
    }

    /// Scenario: A worktree owned by `orch-x` is examined through the real
    /// `examine_worktrees` -> `WorktreeListDocument` path (`gh` stubbed to a
    /// canned "no matching PR" reply -- the verdict itself is irrelevant
    /// here, only the owner field is). The returned `WorktreeReport` must
    /// carry the owner name, and it must survive JSON serialization -- what
    /// `worktree list --json` actually prints.
    #[spec("worktree/reclaim/021")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_021_worktree_list_json_carries_owner() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let wt = scratch.path().join("wt-orch-x");
        add_worktree(&repo, &wt, "feat/orch-x");
        mark_worktree_owned(&wt, "orch-x").expect("mark_worktree_owned must succeed");

        // `gh pr list` unconditionally answers "no matches" -- this test
        // only cares about the owner field, not the reclaim verdict.
        let gh_script = "#!/bin/sh\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then\n    printf '%s\\n' '[]'\n    exit 0\nfi\nexit 1\n";
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let gh_path = bindir.join("gh");
        std::fs::write(&gh_path, gh_script).unwrap();
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].owner,
            Some("orch-x".to_string()),
            "the examined report must carry the owner name"
        );

        let json = serde_json::to_string(&WorktreeListDocument::new(reports)).unwrap();
        assert!(
            json.contains("\"owner\":\"orch-x\""),
            "worktree list --json must carry the owner field, got: {json}"
        );
    }
}
