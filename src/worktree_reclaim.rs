//! `dot-agent-deck worktree list|reclaim`.
//!
//! Reclaims a git worktree only when two gates hold: its PR's state is
//! `MERGED` (via `gh`, never git ancestry — squash-merges never enter `main`'s
//! ancestry, so an ancestry check misses genuinely merged branches), and the
//! tree is clean (`git status --porcelain` empty — a merged branch's worktree
//! can still hold uncommitted files that were never part of the PR, and
//! `--porcelain` never reports gitignored content, so a worktree still
//! holding a `target/` or a `.env` also counts as "clean" here). A THIRD
//! signal — whether the deck can prove it created the worktree AND
//! successfully wrote its ownership marker, AND whether that clean tree
//! still holds gitignored content — decides *how* a merged-and-clean
//! worktree is removed: a deck-created-and-marked worktree with no
//! gitignored content is removed by a bare `reclaim`, with no `--yes`
//! needed; a worktree the deck cannot prove it both created and marked
//! (including one it created but whose marker write failed — issue #164;
//! see [`format_marker_warning`]), OR one that still holds gitignored
//! content regardless of provenance, is instead reported as
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

use crate::terminal_sanitize::{sanitize_for_terminal_display, sanitize_path_for_terminal_display};
use crate::worktree_owner::{OWNER_MARKER_FILENAME, path_from_bytes, trim_trailing_newline};

/// Version of the `--json` document shape. Bump on a field removal or a
/// meaning change; additive fields don't need a bump.
///
/// Bumped to 2 by PRD fork#298 (review F1 / audit, and issue #230's own
/// suggested direction): `owner` is an EXISTING field whose meaning changed,
/// not merely a sibling that gained a field. Before, `owner` was `Some` only
/// for a deck-created worktree whose marker held a readable `created-by:`
/// line — proof-backed, marker-only. After, it is also
/// `Some("human:<login>@<host>")` for a worktree with no marker at all
/// (`owned: false`), so a consumer doing `.worktrees[] | select(.owner !=
/// null)` to enumerate deck-created worktrees now gets every hand-made
/// worktree on the machine too, with no version signal that anything
/// changed. `warnings` and `owner_kind`, added in the same PRD, are additive
/// and need no bump on their own; `owner`'s meaning change is what forces
/// this one.
///
/// `kind` (fork#325 M4a) is likewise additive: every pre-existing field
/// keeps its exact pre-M4a meaning for a pre-existing (linked-worktree) row
/// — `kind` is simply new, always `"linked"` for such a row. The new
/// `"isolated_clone"` rows it also introduces are not a meaning change of
/// anything a consumer could have depended on before this milestone either
/// — array cardinality was never part of this version's contract (running
/// `git worktree add` between two `worktree list` calls already changes row
/// count with no version bump), only field shapes/meanings are.
///
/// Bumped to 3 by fork#325 M4a (reviewer F2): `verdict` gains a fourth
/// value, `"isolated_clone"`. The released `CHANGELOG.md` documents
/// `verdict` as exactly `remove`/`ask`/`keep` — the same kind of documented
/// consumer contract the v2 bump above exists to protect — so a consumer
/// filtering or matching against that three-value domain now sees a value
/// it didn't previously account for. An earlier round of this milestone
/// weighed a no-bump reading instead, on the argument that no pre-existing
/// (linked-worktree) row's `verdict` can ever become `"isolated_clone"` —
/// only a wholly new row kind carries the new value, so no consumer's
/// assumption about an EXISTING row is ever contradicted, unlike the
/// `owner` case above. That distinction doesn't survive contact with what
/// the v2 precedent actually bumped on: a documented value set gaining a
/// member a consumer's filter didn't previously expect, full stop — v2 did
/// not require the changed field to belong to a pre-existing row either,
/// only that a consumer relying on the documented domain could now be
/// surprised by it. `verdict` is exactly that: a real, documented API
/// contract, not an implementation detail whose row-provenance nuance a
/// consumer could reasonably be expected to track. Bump on the value-set
/// change itself.
///
/// Bumped to 4 by fork#325 M4c (maintainer-decided rule): `verdict` gains a
/// FIFTH value, `"isolated_clone_reclaimable"` -- the same shape as the v3
/// bump immediately above, restated rather than re-argued: a documented
/// value-set gaining a member a consumer's filter didn't previously expect
/// is what forces a bump here, independent of whether any pre-existing row's
/// `verdict` could ever transition into the new value (it cannot -- only an
/// isolated-clone row whose PR is merged at its exact current HEAD SHA ever
/// carries it, mirroring exactly how only a wholly-new isolated-clone row
/// ever carried v3's `"isolated_clone"`).
pub const SCHEMA_VERSION: u32 = 4;

/// The name of fork#325 M4b's isolated-clone-specific provenance artifact,
/// and the sub-directory name it is written under. Written by
/// `provision_isolated_clone_sync` at
/// [`crate::issue_dispatch_run::isolated_clone_provenance_path`]'s resolved
/// location — OUTSIDE every candidate, under
/// [`crate::platform::paths::state_dir`] — immediately after the clone
/// itself succeeds and only for a call that has just confirmed `clone_dir`
/// did not already exist — never vouching for a directory this call did
/// not itself create. Deliberately a separate namespace and a separate
/// location from [`OWNER_MARKER_FILENAME`] (which a same-uid attacker can
/// forge into any `.git` directory, including a fake one) and from the
/// shared attach-lock file `create_worktree_sync` and
/// `provision_isolated_clone_sync` both still write for cross-process
/// mutual exclusion (`issue_dispatch_run::worktree_attach_lock_path*`,
/// under the ROOT checkout's common dir) — see `candidate_has_attach_lock`'s
/// own doc comment for why conflating the two was the exact residual this
/// closes.
///
/// **Fix round 2 (reviewer R1 / auditor D1, blocker, PR #515):** M4b's
/// first attempt wrote this file directly into the candidate's OWN `.git`
/// directory, reasoning that being self-contained there meant checking it
/// needed no knowledge of where the root checkout's common dir is. That
/// reasoning was correct about the F2 problem (reviewer F2:
/// `discover_isolated_clones` invoked from inside an isolated clone itself)
/// but wrong about the security property it traded away: a same-uid
/// attacker able to plant a sibling `.git` directory at all can, by
/// definition, write into that `.git` — so evidence living there is
/// forgeable with a bare `touch`, reopening auditor A1/B1's exact
/// misattribution. `state_dir()` gets BOTH properties at once: it resolves
/// identically regardless of where the caller is rooted (no `common_dir`
/// resolution needed), and it is a directory no candidate — genuine or
/// forged — ever controls.
pub(crate) const ISOLATED_CLONE_PROVENANCE_FILENAME: &str =
    "dot-agent-deck-isolated-clone-provenance";

/// `WorktreeReport::kind` value for an ordinary `git worktree list`-visible
/// row (fork#325 M4a).
const KIND_LINKED: &str = "linked";

/// `WorktreeReport::kind` value for a deck-owned isolated clone discovered
/// as a sibling of the enumerating repo (fork#325 M4a — see
/// [`discover_isolated_clones`]). Deliberately reused verbatim as this
/// row's `verdict` too (see [`isolated_clone_report`]): a human scanning
/// `worktree list`'s VERDICT column — which already exists, unlike a new
/// dedicated table column — sees `isolated_clone` immediately, distinct
/// from `remove`/`ask`/`keep`, with no separate lookup needed.
const KIND_ISOLATED_CLONE: &str = "isolated_clone";

/// `WorktreeReport::verdict` value for an isolated clone whose branch has a
/// MERGED PR and whose own current HEAD commit SHA equals that PR's own
/// `headRefOid` exactly — the PR branch's own tip commit, not the merge
/// commit GitHub creates on the base branch (fork#325 M4c round 3, reviewer
/// B2; round 2's now-abandoned design compared against the merge-commit SHA
/// instead — see [`isolated_clone_report`]'s own doc comment for why that
/// was replaced) — distinct from
/// [`KIND_ISOLATED_CLONE`], which every other isolated-clone row still
/// reports as its `verdict` unchanged (unmerged, or merged-but-diverged).
/// `kind` stays [`KIND_ISOLATED_CLONE`] regardless — this is a `verdict`-only
/// distinction, the row kind itself doesn't change. See
/// [`isolated_clone_report`]'s own doc comment for the exact comparison, and
/// [`SCHEMA_VERSION`]'s own doc comment for why a new documented `verdict`
/// value bumps that constant.
const VERDICT_ISOLATED_CLONE_RECLAIMABLE: &str = "isolated_clone_reclaimable";

/// Resolved PR state for a worktree's branch, or why it could not be
/// resolved. `Unresolvable` and `NoPr` both keep — the distinction is only
/// for the reported reason.
///
/// `Merged` carries `headRefOid` (fork#325 M4c, PR #526 round 3, reviewer
/// B2) — the PR BRANCH's own head commit SHA, exposed directly as a flat
/// field by `gh pr list --json headRefOid`, resolved by [`resolve_pr_state`].
/// This replaces round 2's `merge_sha`/`merge_tree_sha` pair (the merge
/// commit GitHub creates ON THE BASE BRANCH, and that commit's tree):
/// reviewer B2 measured live against this repo that a deck-provisioned
/// clone's own HEAD is never equal to `mergeCommit.oid` under any GitHub
/// merge strategy (PR #481 head `7339edd5f440` vs merge `1ceb919349ef`; PR
/// #477 head `11d6327f2421` vs merge `5742ad1f93dd`), so round 2's
/// `mergeCommit`-tree comparison could (almost) never fire for a genuine
/// clone. `headRefOid` is exactly the clone's own reachable commit history
/// instead — no second `gh api graphql` round trip needed (round 2's
/// `resolve_pr_merge_tree_sha` machinery is removed entirely), since `gh pr
/// list --json` already exposes it as a flat field. `None` when `gh`'s
/// response carries no `headRefOid` at all (malformed/absent field) —
/// deliberately never treated as "unresolvable" at the `PrState` level (a
/// merged PR is still a merged PR for [`decide`]'s purposes), but
/// [`isolated_clone_report`]'s M4c comparison must treat a `None`
/// `head_ref_oid` as never eligible for the reclaimable verdict: an
/// ambiguous signal must never widen eligibility for a safety-relevant
/// deletion decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrState {
    Merged { head_ref_oid: Option<String> },
    Open,
    ClosedUnmerged,
    NoPr,
    Unresolvable(String),
}

impl PrState {
    fn label(&self) -> &'static str {
        match self {
            PrState::Merged { .. } => "merged",
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

/// A three-state REPORTING owner (PRD fork#298 M1.0) — answers "who does
/// this worktree belong to?", a different question from [`Ownership`]'s
/// "may a bare `reclaim` remove this with no prompt?". The two must never be
/// conflated: [`decide`] keeps reading [`Ownership`] alone, byte-for-byte as
/// before this PRD, so a resolved [`WorktreeOwner::Human`] can never make a
/// worktree removable without `--yes` (that would reopen fork #144's P1,
/// which fork #166 explicitly refused to reopen — see `worktree/reclaim/039`
/// for the pinned regression guard).
///
/// Resolution order (see [`resolve_worktree_owner`]):
/// 1. Marker present, carrying a `created-by:` line → `Agent`.
/// 2. Marker present, but legacy (bare `"deck\n"`, no `created-by:` line —
///    fork issue #231's exact state) → `Unknown`. Not `Agent` (there is no
///    identity to attribute it to — that would be a fabrication) and not
///    `Human` (the marker DOES prove deck creation, so reporting it as
///    human-owned would understate what is actually known).
/// 3. No marker → `Human`. CLAUDE.md rule 1's dominant real path: the
///    orchestrator's own hand-run `git worktree add` writes no marker.
/// 4. Human resolution fails (no `gh`, unauthenticated, or a spawn/exit
///    failure), or the worktree's own git metadata directory could not be
///    resolved/verified at all → `Unknown`, with a reason — never a silent
///    blank, and never a crash. `worktree list` must keep working with `gh`
///    absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeOwner {
    /// The marker's `created-by:` identity (`read_marker_owner`).
    Agent { identity: String },
    /// No marker; resolved via the same `gh api user` seam
    /// `issue_claim::resolve_gh_login` uses, plus this host's own
    /// `local_hostname()`.
    Human { login: String, host: String },
    /// Positively resolved as "cannot attribute an owner", with a stated
    /// reason — distinct from a worktree this function was never asked
    /// about at all.
    Unknown { reason: &'static str },
}

impl WorktreeOwner {
    /// PRD fork#298 (review F9): all three accessors below are `pub`, not
    /// `pub(crate)` or private, to match this enum's own `pub` visibility.
    /// Before this fix, `WorktreeOwner` was `pub` in a library crate while
    /// `kind()` and `identity_string()` were private and `resolve_worktree_owner`
    /// (the only constructor reachable from outside this module) was also
    /// private -- an external consumer of `dot_agent_deck::worktree_reclaim`
    /// could name the type and match on it but never obtain one or read
    /// anything useful from it, a visibility the module did not actually
    /// keep. Made genuinely usable rather than narrowed to `pub(crate)`,
    /// consistent with the PRD's own framing: reporting gains a three-state
    /// owner, not a three-state owner gains reporting.
    ///
    /// The `owner_kind` value serialized into [`WorktreeReport`] —
    /// `"agent"` / `"human"` / `"unknown"`.
    pub fn kind(&self) -> &'static str {
        match self {
            WorktreeOwner::Agent { .. } => "agent",
            WorktreeOwner::Human { .. } => "human",
            WorktreeOwner::Unknown { .. } => "unknown",
        }
    }

    /// The identity string to carry in [`WorktreeReport::owner`] — `None`
    /// for `Unknown`, matching `owner`'s pre-existing "no identity to
    /// report" meaning. A `Human` renders as `human:<login>@<host>`,
    /// matching `issue_dispatch::Identity::Human`'s own `Display` exactly,
    /// so the two subsystems render an identity the same way.
    pub fn identity_string(&self) -> Option<String> {
        match self {
            WorktreeOwner::Agent { identity } => Some(identity.clone()),
            WorktreeOwner::Human { login, host } => Some(format!("human:{login}@{host}")),
            WorktreeOwner::Unknown { .. } => None,
        }
    }

    /// The stated reason `owner` is absent, for `Unknown` only -- `None` for
    /// `Agent`/`Human`, which need no explaining. Surfaces the discriminator
    /// the three `*_UNKNOWN_REASON` constants already compute at
    /// construction (review F3 / audit F2): before this accessor existed,
    /// `reason` was written three times and read zero times, so the PRD's
    /// "`Unknown` with a stated reason, never a silent blank" was not
    /// delivered on any surface -- see [`WorktreeReport::owner_reason`].
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            WorktreeOwner::Unknown { reason } => Some(reason),
            WorktreeOwner::Agent { .. } | WorktreeOwner::Human { .. } => None,
        }
    }
}

/// Fork issue #231's exact state: a pre-fork#166 marker, bare `"deck\n"`
/// with no `created-by:` line.
const LEGACY_MARKER_UNKNOWN_REASON: &str = "ownership marker predates fork #166's identity tracking (bare \"deck\\n\", no `created-by:` \
     line) -- it proves deck-creation but names no identity to attribute it to (fork issue #231)";

/// The isolated-clone-path analogue of [`LEGACY_MARKER_UNKNOWN_REASON`]
/// above (fork#325 M4a, auditor A4). That text asserts "it proves
/// deck-creation" -- true on the linked-worktree path, where
/// `resolve_worktree_owner` only ever reaches this reason after
/// [`owned_git_dir`] has already containment-checked the git-dir the marker
/// lives in. `isolated_clone_report` reaches this reason only after
/// `candidate_has_attach_lock` (final round -- see that function's own doc
/// comment) has already confirmed the deck's own attach-lock artifact
/// exists for this candidate -- so the marker itself, once read, is trusted
/// exactly as much as a linked worktree's, and is still just as capable of
/// being malformed/oversized/non-UTF-8/legacy-content on its own terms.
const ISOLATED_CLONE_MARKER_UNKNOWN_REASON: &str = "this sibling directory's ownership marker exists but could not be read as a `created-by:` \
     identity (oversized, non-UTF-8, unreadable, or legacy content with no identity line) -- \
     the deck's own provenance artifact for this candidate does exist, so this is treated the \
     same as a linked worktree's legacy marker (fork#325 M4a, auditor A4)";

/// A marker file exists for this candidate, but no matching provenance
/// artifact does (fork#325 M4a, final round -- auditor A1/B1; artifact
/// relocated under `state_dir()` in M4b fix round 2 -- see
/// `ISOLATED_CLONE_PROVENANCE_FILENAME`'s doc comment). Either the deck
/// never attached this clone (a hand-planted or forged marker, auditor
/// B1's exact scenario), or it genuinely was deck-created by a build
/// predating this check, or was moved/renamed since. Discovery still lists
/// the row (structural criteria alone gate inclusion -- see
/// `discover_isolated_clones`'s own doc comment), but the marker's content
/// is deliberately never read in this case: without the provenance
/// artifact there is nothing to trust it against, so treating an unread
/// marker as evidence would reopen exactly the misattribution auditor B1
/// demonstrated.
const ISOLATED_CLONE_NO_ATTACH_LOCK_REASON: &str = "no matching isolated-clone provenance artifact exists for this candidate -- its ownership \
     marker (if any) is not read, since without that artifact there is nothing to trust the \
     marker's content against (fork#325 M4a, auditor A1/B1; M4b reviewer P1/auditor C1; \
     fix round 2 reviewer R1/auditor D1)";

/// The worktree's own git metadata directory could not be resolved, or
/// failed the containment check against the enumerating repo's common dir
/// (see `owned_git_dir`) — e.g. a forged `.git` redirect
/// (`worktree/reclaim/014`). Fails closed to `Unknown` rather than assuming
/// `Human`: an unverifiable worktree structure is not evidence of anything.
const GIT_DIR_UNRESOLVED_UNKNOWN_REASON: &str = "this worktree's own git metadata directory could not be resolved or verified as contained \
     under the repository's common directory, so ownership cannot be determined";

/// No marker, and `gh api user` could not resolve a login — `gh` absent,
/// unauthenticated, or a spawn/exit failure. `worktree list` must keep
/// working in this state, never crash and never guess an identity.
const GH_LOGIN_UNRESOLVED_UNKNOWN_REASON: &str = "no ownership marker is present and `gh api user` could not resolve a login (gh absent, \
     unauthenticated, or failing) -- worktree list keeps working rather than guessing an identity";

/// Resolve a worktree's reporting owner (PRD fork#298 M1.0). `human_cache`
/// is shared across one [`examine_worktrees`] call so that, in a repo with
/// several unmarked (hand-made) worktrees, the `gh api user` round trip
/// happens at most once per invocation rather than once per worktree — every
/// unmarked worktree in a single `worktree list` run belongs to the same
/// caller, so there is nothing to gain by re-resolving it.
fn resolve_worktree_owner(
    repo_dir: &Path,
    worktree_path: &Path,
    human_cache: &mut Option<WorktreeOwner>,
) -> WorktreeOwner {
    match owned_git_dir(repo_dir, worktree_path) {
        Some(git_dir) => {
            let marker_path = git_dir.join(OWNER_MARKER_FILENAME);
            if marker_path.is_file() {
                // Reads the marker directly via `read_marker_owner` rather
                // than going through `owner_of` (review F2 / audit F3):
                // `owner_of` would re-resolve `owned_git_dir` a THIRD
                // independent time per worktree -- on top of `ownership_of`'s
                // and this function's own above -- widening the fork #166 P2
                // flip window from four `git rev-parse` spawns to six. The
                // git-dir above is already containment-checked, so the exact
                // directory that passed containment is the directory
                // `read_marker_owner` reads from -- no weaker than routing
                // through `owner_of`, and strictly narrower.
                match read_marker_owner(&marker_path) {
                    Some(identity) => WorktreeOwner::Agent { identity },
                    None => WorktreeOwner::Unknown {
                        reason: LEGACY_MARKER_UNKNOWN_REASON,
                    },
                }
            } else {
                human_cache.get_or_insert_with(resolve_human_owner).clone()
            }
        }
        None => WorktreeOwner::Unknown {
            reason: GIT_DIR_UNRESOLVED_UNKNOWN_REASON,
        },
    }
}

/// The "no marker" branch of [`resolve_worktree_owner`]: resolve a human
/// caller via the same seam `issue_claim::resolve_gh_login` already
/// establishes (byte-identical `gh api user --jq .login` argv, via
/// `issue_dispatch::gh_current_login_argv`), plus this host's own
/// `local_hostname()`. Never panics, never shells out to anything other than
/// `gh` — a failure resolves `Unknown` with a stated reason rather than
/// crashing `worktree list`.
fn resolve_human_owner() -> WorktreeOwner {
    let host = crate::issue_dispatch_run::local_hostname();
    match crate::issue_claim::resolve_gh_login() {
        Ok(login) => WorktreeOwner::Human { login, host },
        Err(_) => WorktreeOwner::Unknown {
            reason: GH_LOGIN_UNRESOLVED_UNKNOWN_REASON,
        },
    }
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
        PrState::Merged { .. } => match clean {
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
    /// One of `"remove"` / `"ask"` / `"keep"` (from [`Verdict::label`], via
    /// [`decide`]) for a `kind == "linked"` row, or one of `"isolated_clone"`
    /// / `"isolated_clone_reclaimable"` (from [`isolated_clone_report`],
    /// never through [`decide`] at all) for a `kind == "isolated_clone"`
    /// row — see [`SCHEMA_VERSION`]'s own doc comment (reviewer F2, then
    /// fork#325 M4c) for why this fourth and fifth value each forced a
    /// bump, to `SCHEMA_VERSION` 3 and 4 respectively. Two independent
    /// producers, both of which this doc comment names, is why this field
    /// needs one where every sibling field's own doc comment already has
    /// one.
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The resolved identity string (PRD fork#298 M1.0's [`WorktreeOwner::identity_string`]):
    /// the marker's `created-by:` line for an `Agent`, `human:<login>@<host>`
    /// for a `Human` (no marker — CLAUDE.md rule 1's dominant real path),
    /// `None` for `Unknown` — including an `Ours` worktree whose marker
    /// predates fork #166 (the bare `"deck\n"` legacy content, fork issue
    /// #231) — omitted from JSON entirely rather than serialized as `null`,
    /// mirroring `reason` above, so an older client reading this document
    /// still round-trips cleanly.
    ///
    /// PRD fork#298 (audit Focus 4 / F4): this is a display string spanning
    /// TWO namespaces (a marker's free-form `created-by:` value, or a
    /// synthesised `human:<login>@<host>`) and is NOT authenticated — anyone
    /// with write access to a worktree's admin dir can plant a marker
    /// reading `created-by: human:alice@laptop`, producing `owner:
    /// "human:alice@laptop"` alongside `owner_kind: "agent"`. `owner_kind`
    /// is the only field a consumer may branch on; nothing in this crate
    /// prefix-matches `owner` today, and the point of this note is to keep
    /// it that way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The three-state owner's kind (PRD fork#298 M2.0) — `"agent"` /
    /// `"human"` / `"unknown"`, from [`WorktreeOwner::kind`]. Always
    /// present, unlike `owner` above: every worktree resolves to exactly one
    /// [`WorktreeOwner`] variant, so there is no "absent" case to omit.
    /// Reporting-only — [`decide`] never reads this, only [`Ownership`]
    /// (`owned` above), so a `Human`-owned worktree can never become
    /// removable by a bare `reclaim`.
    pub owner_kind: String,
    /// `"linked"` for an ordinary `git worktree list`-visible row (existing
    /// rows, unchanged behavior) or `"isolated_clone"` for a deck-owned
    /// isolated clone discovered as a sibling of the enumerating repo (fork
    /// #325 M4a — see [`discover_isolated_clones`]). Always present, like
    /// `owner_kind` above: every examined entry is exactly one of the two.
    /// Additive, same justification as `owner_kind`/`owner_reason` — no
    /// `SCHEMA_VERSION` bump for this field alone (see that constant's own
    /// doc comment for `owner`, the case that DID force a bump at v2, and
    /// `verdict`, the case that forced the bump to v3 — and why `kind` isn't
    /// either: it introduces no new meaning for any
    /// pre-existing field or row, only a new, always-"linked" field on rows
    /// that already existed, plus new rows of a kind no consumer could have
    /// been relying on before this milestone). Auditor A1: `owned` and
    /// `owner_kind` mean something WEAKER on a `kind == "isolated_clone"`
    /// row than on a `"linked"` one — see [`isolated_clone_report`]'s own
    /// doc comment for exactly what evidence backs them there and why it
    /// isn't as strong as the linked case's `owned_git_dir` containment
    /// check.
    pub kind: String,
    /// Fork issue #597: whether this isolated clone is explicitly pinned
    /// against fork#325 M4c's automatic reclaim (fork issue #546 hazard 2)
    /// — `is_pinned || pin_unresolvable` from [`isolated_clone_report`]'s
    /// own eligibility gate, so an unreadable pin signal reports `true`
    /// here too, fails closed exactly like every other unresolvable signal
    /// this heuristic treats. Always present, no
    /// `#[serde(skip_serializing_if)]`: the unresolvable-provenance case
    /// already fails closed to "treated as pinned" everywhere else in this
    /// file, so collapsing it to `true` here is consistent, not a loss of
    /// information. `false` for a `kind == "linked"` row — not applicable
    /// to an ordinary worktree.
    pub pinned: bool,
    /// Why `owner_kind` is `"unknown"` (review F3 / audit F2) — `None` for
    /// `Agent`/`Human`, which need no explaining. Additive (same
    /// justification as `owner_kind`; no `SCHEMA_VERSION` bump needed for
    /// this field alone). Before this field existed, [`WorktreeOwner::Unknown`]'s
    /// `reason` was computed at construction and discarded by every
    /// consumer, so three genuinely distinct causes — a benign legacy
    /// marker, `gh` absent/unauthenticated, and an unverifiable (possibly
    /// forged, per `worktree/reclaim/014`) git-dir — all rendered
    /// identically as `owner_kind: "unknown"` with nothing to tell them
    /// apart. This is the PRD's own stated M1.0 property ("`Unknown` with a
    /// stated reason, never a silent blank") actually reaching a surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_reason: Option<String>,
    /// The worktree's real, byte-exact path (issue #144 finding 4) — never
    /// serialized; `path` above (`to_string_lossy`) is what the JSON document
    /// and the human report show. [`run_reclaim`] passes THIS to
    /// [`remove_worktree_dir`] so a non-UTF-8 path can never cause `git
    /// worktree remove` to be handed a different, lossily-mangled path that
    /// happens to resolve to a different registered (or symlinked) worktree.
    #[serde(skip)]
    pub real_path: PathBuf,
    /// Who/what triggered this worktree's removal (issue #325) — the
    /// caller-supplied `remover` identity string, following the exact
    /// `owner`/`owner_kind` pattern above. `Some(_)` only for a report
    /// [`run_reclaim`] actually pushed into [`ReclaimOutcome::removed`];
    /// `None` for `pending`/`kept` — nothing was removed, so there is
    /// nothing to attribute.
    ///
    /// Same warning as `owner` above, and for the same reason (issue #325
    /// auditor A3): this is a display string spanning several free-form
    /// namespaces (`worktree:…@…|…`, `human:…@…`, `pane:…@… (cwd …)`,
    /// `"unknown"`, or literally anything a caller of the `pub` `run_reclaim`
    /// passes) and is NOT authenticated — anyone able to influence the
    /// caller's identity resolution can plant an arbitrary value here.
    /// Nothing in this crate branches on `removed_by` today, and the point
    /// of this note is to keep it that way; there is no `owner_kind`-style
    /// discrete field for this one to fall back on instead.
    ///
    /// `#[serde(skip)]`, same as `real_path` above and for the same reason:
    /// there is no `worktree reclaim --json` today, so this field is never
    /// actually serialized by anything that exists. A future `--json`
    /// consumer looking for `removed_by` in the JSON document and finding it
    /// silently absent should land here rather than guess.
    #[serde(skip)]
    pub removed_by: Option<String>,
}

/// Reports where the on-disk marker names `owner`, but the independent
/// `owned` resolution says otherwise (issue #221). `ownership_of` and
/// `owner_of` each spawn their own `git rev-parse`s to answer a related but
/// distinct question, and `owner_of`'s own doc records that under
/// concurrent worktree-admin-dir writes the two can disagree: `owned=false`
/// landing alongside a non-`None` `owner`. `worktree list --mine`'s retain
/// filter (`src/main.rs`) correctly excludes such a row -- `owned` must stay
/// a conjunct, not a relaxation -- but excluding it silently turns a
/// disagreement into indistinguishable-from-"nothing found", which is worse
/// than surfacing it. Callers use this to warn on stderr before filtering,
/// never to change what gets filtered.
///
/// PRD fork#298 (review F11 / audit F1): `owned=false && owner.is_some()` is
/// no longer sufficient evidence of a disagreement on its own. Before, every
/// non-`None` `owner` came from a marker, so the predicate really did mean
/// "a marker names this owner, yet the independent authority check says
/// foreign". Now `owner` can also be a `Human` resolution
/// (`WorktreeOwner::Human`) for an ordinary, working-as-designed hand-made
/// worktree -- CLAUDE.md rule 1's dominant real path, and every such row is
/// `owned: false` by construction -- which is the MAJORITY of worktrees in
/// this repo, not an anomaly. Restricted to `owner_kind == "agent"` (the
/// only kind a marker can produce) so this stays what its name says: a
/// genuine disagreement between two resolutions of the same marker-backed
/// claim, never a human row's ordinary, unmarked state.
pub fn owner_disagreements<'a>(reports: &'a [WorktreeReport], owner: &str) -> Vec<&'a Path> {
    reports
        .iter()
        .filter(|r| !r.owned && r.owner_kind == "agent" && r.owner.as_deref() == Some(owner))
        .map(|r| r.real_path.as_path())
        .collect()
}

/// The `--mine` fail-closed predicate (PR #215 round-1 reviewer F4 / auditor
/// L1 item 3): a row counts as "mine" only when `owned` AND the marker's
/// `owner` matches -- `owned` must stay a conjunct, never dropped, or a
/// foreign worktree whose marker happens to name this caller would pass.
/// Extracted (issue #221 review round) so `run_worktree_list_cli`'s retain
/// and its test share one definition instead of two independently typed
/// copies of the same predicate that could silently drift apart.
pub fn is_mine(report: &WorktreeReport, owner: &str) -> bool {
    report.owned && report.owner.as_deref() == Some(owner)
}

/// Formats the issue #221 disagreement warning printed by `--mine` before
/// filtering. Pulled out of `run_worktree_list_cli` so the exact
/// user-visible text is assertable by a unit test without needing to stage
/// the `owned_git_dir` race that produces the state it describes (that state
/// cannot be reached through the real-binary `Fixture` -- see
/// `tests/CATALOG.md`'s `worktree/reclaim/030` entry).
///
/// `path` and `owner` are both display-only here (issue #232): they go
/// through [`sanitize_path_for_terminal_display`] /
/// [`sanitize_for_terminal_display`], never the raw, byte-exact values --
/// this warning is printed to stderr before the operator inspects the
/// marker/admin state, so neither a hostile path component nor a hostile
/// marker `owner` can forge or hide part of that warning. `owner` needs its
/// own sanitizing pass here even though it already went through
/// [`sanitize_marker_creator`] upstream (issue #232 round 2, gap 1): that
/// sanitizer strips Unicode category `Cc` but deliberately preserves `Cf`
/// (bidi/format) chars, exactly the set this module treats as hostile for
/// display, so a marker value like `orchestration:prod\u{202e}...` would
/// otherwise reach this stderr line with a raw bidi control still in it.
pub fn format_disagreement_warning(path: &Path, owner: &str) -> String {
    format!(
        "worktree list --mine: {path} is marked owned by {owner}, but the ownership check \
         disagrees -- excluding it rather than trusting either signal (often a `git \
         rev-parse` race; persisting past a re-run rules that out -- check the marker and \
         admin dir)",
        path = sanitize_path_for_terminal_display(path),
        owner = sanitize_for_terminal_display(owner)
    )
}

/// Formats the fork issue #231 warning for a row `--mine` excludes because
/// it is `owned: true` but carries no marker identity (`owner: None`,
/// `owner_kind: "unknown"`). Distinct from [`format_disagreement_warning`]'s
/// #221 case: there `owned` and `owner` actively disagree; here `owned` is
/// true and `owner` is simply absent, so nothing here signals forgery or a
/// race, only that this row cannot be matched to any specific caller.
///
/// PRD fork#298 (review F3 / audit F2): this used to name a single likely
/// cause here ("often a legacy pre-fork#166 marker"), but `owner_kind:
/// "unknown"` with `owned: true` collapses several distinct states —
/// including a marker deliberately grown past `MARKER_READ_MAX_BYTES` or
/// written as invalid UTF-8, both plantable by the same admin-dir writer
/// that can plant a genuine legacy marker — and asserting one of them as the
/// likely cause is a confident wrong answer for the others. The `--json`
/// document's `owner_reason` field (added in the same PRD) carries the
/// actual resolved reason for a consumer that wants to distinguish them;
/// this stderr line stays deliberately non-committal.
///
/// Deliberately NOT also printed to stderr per-row (unlike the #221 warning
/// above) — issue #231's own doc notes a blanket per-row stderr warning here
/// would fire on every legacy worktree, every time, for a state that is
/// working as designed; the `--json` document is where a consumer can
/// decide, without a human terminal being spammed. `path` is display-only,
/// sanitized the same way.
pub fn format_excluded_unknown_owner_warning(path: &Path) -> String {
    format!(
        "worktree list --mine: {path} is deck-owned (owned: true) but carries no identifiable \
         marker owner (owner_kind: \"unknown\" -- see the --json document's owner_reason for \
         why) -- excluding it from --mine rather than guessing whether it is yours",
        path = sanitize_path_for_terminal_display(path)
    )
}

/// Fork issue #325 M2: `.git/shallow` lives in the repo's **common dir**, so
/// one shallow fetch (`git clone --depth`, `git fetch --depth`) truncates
/// history for every linked worktree and every orchestration sharing this
/// clone at once, not just whoever ran it. Nothing else about a shallow repo
/// errors -- `git log`, `git status`, and ref resolution all work fine -- so
/// the only symptom is a later merge or rebase against upstream failing with
/// "refusing to merge unrelated histories," which reads like a wrong remote
/// rather than truncated history.
///
/// `git rev-parse --is-shallow-repository` (rather than statting
/// `.git/shallow` directly) is what git itself uses to answer this, and it
/// resolves the common dir correctly whether `repo_dir` is the main working
/// tree or a linked worktree -- exactly the ambiguity this bug lives in, so
/// deferring to git's own answer avoids duplicating that resolution logic.
///
/// A spawn failure or non-zero exit is NOT reported as shallow -- this is an
/// advisory warning, not a correctness gate, and a probe that could not run
/// says nothing either way; fails silent rather than fails loud, matching a
/// plain repo.
fn is_shallow_repo(repo_dir: &Path) -> bool {
    let out = Command::new("git")
        .current_dir(repo_dir)
        .args(["rev-parse", "--is-shallow-repository"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "true",
        _ => false,
    }
}

/// Formats the fork issue #325 M2 warning `run_worktree_list_cli` emits when
/// [`is_shallow_repo`] finds the enumerating repo shallow. Names both the
/// condition AND its repair (`git fetch --unshallow`) rather than only the
/// symptom -- the failure this exists to head off ("refusing to merge
/// unrelated histories") reads like a wrong remote, not a truncated one, so
/// naming the repair command is what actually gets an operator unstuck.
/// `path` is display-only and goes through the same terminal sanitizer as
/// every other path this module prints (issue #232).
fn format_shallow_repo_warning(repo_dir: &Path) -> String {
    format!(
        "warning: {path} is a SHALLOW git repository -- \".git/shallow\" lives in the shared \
         common dir, so every linked worktree and every orchestration sharing this clone sees \
         the same truncated history, not just whoever ran the shallow fetch. A later merge or \
         rebase against upstream may fail with \"refusing to merge unrelated histories\", which \
         reads like a wrong remote rather than truncated history. Repair with: git -C {path} \
         fetch --unshallow",
        path = sanitize_path_for_terminal_display(repo_dir)
    )
}

/// `Some(warning)` when [`is_shallow_repo`] finds `repo_dir` shallow, `None`
/// for a normal, full-history repo. `run_worktree_list_cli` prints the
/// warning to stderr AND (fork issues #230/#231 precedent) folds it into the
/// `--json` document's `warnings` array, so a `--json` consumer sees it too
/// rather than it being visible only on a human terminal.
pub fn shallow_repo_warning(repo_dir: &Path) -> Option<String> {
    if is_shallow_repo(repo_dir) {
        Some(format_shallow_repo_warning(repo_dir))
    } else {
        None
    }
}

/// Top-level `--json` document.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeListDocument {
    pub schema_version: u32,
    pub worktrees: Vec<WorktreeReport>,
    /// Machine-readable warnings a `--mine --json` consumer would otherwise
    /// never see (fork issues #230, #231) — additive, so `SCHEMA_VERSION`
    /// does not need a bump (per this file's own doc: "additive fields don't
    /// need a bump"). Carries no `skip_serializing_if`, so EVERY `worktree
    /// list --json` document now carries a `"warnings":[]` key, including a
    /// plain non-`--mine` run where nothing has been excluded and there is
    /// nothing to warn about — a consumer can read the key unconditionally
    /// rather than treating its absence as meaningful. Reporting-only,
    /// exactly like `owner_kind` — never changes which rows `--mine`
    /// retains.
    pub warnings: Vec<String>,
}

impl WorktreeListDocument {
    pub fn new(worktrees: Vec<WorktreeReport>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            worktrees,
            warnings: Vec::new(),
        }
    }

    /// Attach `--mine` filtering warnings (fork issues #230, #231) — a
    /// separate step from [`Self::new`] so the three existing internal call
    /// sites that never filter need no change.
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
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

/// Shared by both a linked worktree's row and a discovered isolated clone's
/// (`isolated_clone_report`) — always run via [`git_in_untrusted_dir`]
/// (fork#325 M4a, auditor A3): `-c core.fsmonitor=` is a CORRECTNESS no-op
/// for a linked worktree (fsmonitor is genuinely still disabled for that
/// call; it simply doesn't matter there, since a linked worktree is already
/// reachable only via `git worktree list`, never a directory an outside
/// party could plant), and is the exact vector the
/// audit's own lab reproduced arbitrary code execution through for a
/// discovered isolated-clone candidate, whose directory this process did
/// not create.
fn check_cleanliness(worktree_path: &Path) -> Cleanliness {
    let out = git_in_untrusted_dir(worktree_path)
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

/// Best-effort "do these two paths name the same directory" check, via
/// `std::fs::canonicalize` on both sides — used only to decide which of two
/// equally-correct path SPELLINGS to prefer (see
/// [`discover_isolated_clones`]'s own doc comment), never to produce a value
/// that is itself returned or displayed: `canonicalize` resolves symlinks
/// (macOS's `/var` -> `/private/var`) and, on Windows, produces a
/// `\\?\`-prefixed extended-length path that some `git` commands reject
/// outright, so its result is only ever safe to compare, not to keep.
/// `false` whenever either side fails to canonicalize (e.g. it does not
/// exist) — unknown must never resolve to "same directory".
fn paths_refer_to_same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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
///
/// `pub(crate)`, not private: PRD fork#235 M3's `issue_claim::resolve_caller_identity`
/// calls this directly (with `repo_dir == worktree_path == cwd`, the same
/// self-referential pattern `owner_of` below already uses) to answer "is this
/// a genuinely linked worktree at all", independent of whether it carries an
/// ownership marker — round 3's identity anchor needs the containment check
/// alone, never the marker.
pub(crate) fn owned_git_dir(repo_dir: &Path, worktree_path: &Path) -> Option<PathBuf> {
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
/// rendered `worktree list` column or a `jq -r` pipeline unfiltered.
///
/// PR #215 fixup (reviewer L3 / auditor L3): this used to say the gap was
/// latent because no consumer rendered `owner` yet, and becomes live once
/// M2.3 adds a human-facing `OWNER` column. **This PR is M2.3's display
/// half** — `format_list_human` now emits the OWNER column — so the gap is
/// live as of this SHA, not latent. Its effect is bounded to display
/// spoofing in that one cell: `format_reclaim_human` does not render
/// `owner`, so a `created-by:` value carrying U+202E cannot reach the
/// removal-confirmation surface, only make the OWNER column (and,
/// depending on the terminal, the rest of that row) render reversed.
/// Closing the Cf gap itself remains out of scope here. An
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
/// two disagreeing rather than both reporting unknown.
///
/// PR #215 fixup (reviewer F4 / auditor L1 item 3): this used to say the
/// disagreement was accepted as cosmetic because "no consumer treats
/// `owner`'s mere presence as an ownership signal." That is no longer
/// true — `worktree list --mine` (`src/main.rs::run_worktree_list_cli`) is
/// exactly such a consumer as of this PR, and now filters through
/// [`is_mine`] rather than `owner` alone, so a foreign worktree whose
/// marker happens to read back a matching identity is excluded rather
/// than reported as mine.
///
/// PRD fork#298 (review F2 / audit F3): [`examine_worktrees`] no longer
/// calls this function at all, directly or indirectly. It calls
/// [`resolve_worktree_owner`], which -- once its OWN [`owned_git_dir`]
/// resolution has containment-checked the git-dir -- reads the marker via
/// [`read_marker_owner`] directly rather than routing back through this
/// function, so a THIRD independent [`owned_git_dir`] resolution is not
/// spent per marked worktree. What [`examine_worktrees`] actually gets is
/// [`ownership_of`]'s resolution immediately followed by
/// [`resolve_worktree_owner`]'s own, with no I/O of any kind — no `gh` call,
/// no other filesystem work — in between, for a MARKED worktree: each
/// [`owned_git_dir`] call spawns two `git rev-parse` subprocesses
/// ([`resolve_git_dir`] and [`resolve_common_dir`]), so those two
/// back-to-back calls span **four** spawns, not two, restoring the original
/// window the auditor measured a hostile flipper winning 54 of 120 times
/// (45%) — not narrowing it further, but not widening it either. The `gh api
/// user` call [`resolve_worktree_owner`] may make sits in the
/// mutually-exclusive NO-marker branch, so the original fork #166 P2 worst
/// case — a seconds-wide network round trip landing between the two
/// resolutions — is not reintroduced by any of this.
///
/// This function remains available as a direct, single-worktree marker
/// lookup for callers that have not already containment-checked a git-dir of
/// their own — its own test suite, and `ui.rs`'s marker-content assertions.
/// `#[cfg(test)]` because, as of the [`resolve_worktree_owner`] fix above,
/// those two test call sites are its only remaining callers; a production
/// build with no test callers left it as a genuine dead-code warning under
/// `-D warnings`.
#[cfg(test)]
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
/// Atomic write-then-rename, mirroring `SavedSession::save`'s pattern in
/// `config.rs`: the full content is written to a sibling temp file in the
/// same git-admin directory (same-filesystem, so `rename(2)` is atomic),
/// then renamed into place. The pid suffix on the temp name avoids
/// collisions between concurrently-marking decks; same-directory placement
/// is required, since `rename` is only atomic within one filesystem.
///
/// This closes a real gap, not a theoretical one (fork #164/#218):
/// `std::fs::write` is `File::create` followed by `write_all`, so on
/// ENOSPC or a process kill mid-write, `File::create` had already
/// succeeded and left a **created, partially-written, regular file** at
/// the marker path — [`ownership_of`] checks presence only
/// (`Path::is_file`), so that half-written file used to resolve `Ours`
/// exactly as a complete write would, even though [`mark_worktree_owned`]
/// itself had returned `Err`. Once issue #164 made that `Err` a
/// user-visible warning promising the worktree would need `--yes` on a
/// later `reclaim`, that stale reasoning became actively wrong: a merged,
/// clean worktree with a truncated marker would still land on
/// `Verdict::Remove` under a bare `reclaim`, contradicting the warning
/// just shown.
///
/// The invariant this now establishes: **the final marker path is only
/// ever created by a rename of a fully-written file.** Any failure along
/// the way — temp create, write, or rename — is cleaned up (the temp file
/// is removed on every error path) and leaves **no** file at the final
/// marker path, so [`ownership_of`] returns `Foreign` and a later
/// `reclaim` asks rather than silently removing. [`ownership_of`] itself
/// still checks presence only, which remains correct *given* this
/// invariant — presence now implies a complete write, not merely an
/// attempted one — and that is also what keeps an older-build marker (the
/// bare `"deck\n"` this function wrote before issue #425) resolving
/// `Ours`: [`ownership_of`] never inspects content, so a first line other
/// than `deck` would be the only way to regress that, and this function
/// always writes `deck` first for exactly that reason.
///
/// Re-marking an already-marked worktree stays idempotent: `rename` onto
/// an existing destination replaces it (verified cross-platform — Windows'
/// `MoveFileExW` is invoked with `MOVEFILE_REPLACE_EXISTING`), so the net
/// effect is the same truncate-and-overwrite `std::fs::write` used to give,
/// just via a path that never exposes a partial file to a concurrent
/// reader.
///
/// **This also finally covers [`owner_of`]**, which fork #166 added as
/// this file's first content reader, closing fork #218: a short write can
/// no longer land at the final marker path at all, so there is no more
/// silently-truncated identity for `owner_of` to read back as
/// authoritative.
///
/// [`ownership_of`]'s presence-only check and [`owner_of`]'s read path /
/// byte cap are unchanged by this — content parsing was never the fix,
/// completeness-before-visibility was.
pub(crate) fn mark_worktree_owned(worktree_path: &Path, creator: &str) -> Result<(), String> {
    let git_dir = resolve_git_dir(worktree_path).ok_or_else(|| {
        format!(
            "could not resolve git-dir for {} via `git rev-parse --git-dir`",
            worktree_path.display()
        )
    })?;
    let content = format!("deck\ncreated-by: {}\n", sanitize_marker_creator(creator));
    let marker_path = git_dir.join(OWNER_MARKER_FILENAME);
    let tmp_path = git_dir.join(format!(
        "{OWNER_MARKER_FILENAME}.{}.tmp",
        std::process::id()
    ));

    std::fs::write(&tmp_path, &content).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("failed to write ownership marker: {e}")
    })?;
    std::fs::rename(&tmp_path, &marker_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("failed to write ownership marker: {e}")
    })
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

/// `pub`, not `pub(crate)`: PR #215 fixup (auditor M1) calls this from
/// `main.rs::run_worktree_list_cli` too, a separate compilation unit
/// `pub(crate)` cannot reach, so the marker write and the
/// `DOT_AGENT_DECK_WORKTREE_OWNER` env var are the same sanitized value
/// rather than one raw and one sanitized. This function is a fixed
/// point (`f(f(x)) == f(x)` for every input — verified: the truncation
/// branch drops exactly the trailing `…` it just appended before
/// re-appending an identical one, and every other transform is already
/// idempotent), so `mark_worktree_owned` re-applying it to an
/// already-sanitized value is harmless rather than a second, diverging
/// derivation.
pub fn sanitize_marker_creator(name: &str) -> String {
    let trimmed = clean_and_trim_marker_creator(name);
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    if trimmed.chars().count() > MARKER_CREATOR_MAX_CHARS {
        let truncated: String = trimmed.chars().take(MARKER_CREATOR_MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        trimmed
    }
}

/// The clean-and-trim half of [`sanitize_marker_creator`] — everything it
/// does except the final truncation. Factored out so a producer can reject a
/// name that normalization would *change*, not merely truncate (see
/// [`marker_creator_normalizes`]).
fn clean_and_trim_marker_creator(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    cleaned.trim().to_string()
}

/// True when [`sanitize_marker_creator`] would change `name` for a reason
/// *other* than truncation — a dropped control character, a `\n`/`\r`
/// mapped to a space, or leading/trailing whitespace trimmed away. Any of
/// these lets two distinct `name` values collapse to the identical
/// marker-creator string well before either is anywhere near
/// [`MARKER_CREATOR_MAX_CHARS`] (fork #222 edge 1 follow-up: length alone
/// doesn't close the collision — `"deploy prod"` and `"deploy\nprod"` are
/// both short and both sanitize to `"deploy prod"`). A producer that rejects
/// whenever this returns `true` guarantees its own `name` is already a fixed
/// point of everything `sanitize_marker_creator` does except truncate, so
/// the only remaining way two distinct names can collide is the length
/// truncation itself — which callers must still bound separately.
pub fn marker_creator_normalizes(name: &str) -> bool {
    clean_and_trim_marker_creator(name) != name
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
///
/// Runs via [`git_in_untrusted_dir`] (fork#325 M4a final round, auditor
/// B3), even though `remote get-url` is a pure config read that cannot
/// reach `core.fsmonitor`: this function is called with `repo_dir` set to
/// an isolated-clone candidate as often as to a trusted checkout, and
/// routing every candidate-directory invocation through the same helper —
/// rather than reasoning per call site about which ones need it — is the
/// isolation this module already commits to for [`check_cleanliness`] and
/// [`resolve_isolated_clone_branch`].
pub(crate) fn derive_repo_slug(repo_dir: &Path) -> Option<String> {
    let out = git_in_untrusted_dir(repo_dir)
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
            "state,headRefName,headRepositoryOwner,headRefOid",
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
            Some("MERGED") => {
                let head_ref_oid = one
                    .get("headRefOid")
                    .and_then(|o| o.as_str())
                    .map(|s| s.to_string());
                PrState::Merged { head_ref_oid }
            }
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
/// and `ownership_of(repo_dir, &wt.path)` / `resolve_worktree_owner(repo_dir,
/// &wt.path, ..)`: every per-worktree property is read from the worktree it
/// describes (`repo_dir` is still threaded into both — not as the thing
/// being described, but as the enumerating repo whose common dir the
/// worktree's admin dir must sit under; see `owned_git_dir`'s doc comment).
/// `resolve_worktree_owner` is called immediately after `ownership_of` (PRD
/// fork#298 review F2 / audit F3 — `owner_of` is NOT the function in this
/// sequence any more; see its own doc comment for why, and for the real
/// spawn count and which branch may still make a `gh` call). One concrete
/// case this affects:
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
    // Shared across every worktree in this one call — see
    // `resolve_worktree_owner`'s doc for why a `gh api user` resolution is
    // reused rather than repeated per unmarked worktree.
    let mut human_owner_cache: Option<WorktreeOwner> = None;
    for wt in raw {
        let cleanliness = check_cleanliness(&wt.path);
        let clean = cleanliness == Cleanliness::Clean;
        let owned = ownership_of(repo_dir, &wt.path) == Ownership::Ours;
        let ownership = if owned {
            Ownership::Ours
        } else {
            Ownership::Foreign
        };
        // PRD fork#298 M1.0: a SEPARATE `owned_git_dir` resolution from
        // `ownership_of` above, deliberately mirroring the pre-existing
        // `ownership_of` + `owner_of` split (fork #166 P2 / reviewer F5) —
        // `owned` (removal authority) and `owner`/`owner_kind` (reporting)
        // must never be derived from the same resolution, or a party able to
        // flip the worktree's `.git` redirect between the two calls could
        // make the containment check pass against the real admin dir while
        // the content read lands in a forged one it controls.
        let resolved_owner = resolve_worktree_owner(repo_dir, &wt.path, &mut human_owner_cache);
        let owner = resolved_owner.identity_string();
        let owner_kind = resolved_owner.kind().to_string();
        let owner_reason = resolved_owner.reason().map(str::to_string);
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
            owner_kind,
            owner_reason,
            real_path,
            removed_by: None,
            pinned: false,
            kind: KIND_LINKED.to_string(),
        });
    }

    // Fork#325 M4a: sibling deck-owned isolated clones, invisible to
    // `list_linked_worktrees` above (`git worktree list` structurally
    // cannot see a directory that is not a linked worktree of `repo_dir` at
    // all — see `discover_isolated_clones`'s own doc comment).
    let isolated_candidates = discover_isolated_clones(repo_dir);
    if !isolated_candidates.is_empty() {
        // Resolved once per `examine_worktrees` call, not once per
        // candidate (auditor B3, final round) — every candidate's derived
        // slug is compared against this same value in
        // `isolated_clone_report`.
        let repo_slug = derive_repo_slug(repo_dir);
        for candidate in isolated_candidates {
            reports.push(isolated_clone_report(repo_slug.as_deref(), candidate));
        }
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

/// A sibling directory of the root checkout recognized as a deck-owned
/// isolated clone (fork#325 M4a): a genuine independent repository — its
/// own `.git` as a DIRECTORY, matching `provision_isolated_clone_sync`'s
/// on-disk shape and the same `clone_dir.join(".git").is_dir()` guard
/// `attempt_isolated_clone_cleanup` already uses (deliberately `is_dir()`,
/// not `exists()`: a linked worktree's `.git` is a FILE redirect, which
/// this excludes for free, structurally, with no separate check needed) —
/// carrying a readable ownership marker at `<path>/.git/dot-agent-deck-owner`.
///
/// Final round (reviewer F13 / auditor A1/B1): earlier rounds additionally
/// required the candidate's checked-out commit to be an object the
/// enumerating repo already had ([`candidate_shares_history_with`], since
/// removed). That check is gone — see [`discover_isolated_clones`]'s own
/// doc comment for why — so DISCOVERY (whether a candidate appears in the
/// report at all) is now purely structural, exactly as it was before that
/// check existed. What replaces it is `has_attach_lock` below, resolved
/// once per candidate here rather than left to be recomputed by every
/// consumer: whether ownership (`owned` on the resulting
/// [`WorktreeReport`]) is genuinely provable via the deck's own attach-lock
/// artifact — see [`candidate_has_attach_lock`]'s doc comment for what that
/// does and does not prove.
struct IsolatedCloneCandidate {
    path: PathBuf,
    git_dir: PathBuf,
    has_attach_lock: bool,
}

/// A `git` invocation scoped to run inside a directory this process did not
/// create and does not otherwise trust (fork#325 M4a auditor A3): a
/// candidate sibling directory [`discover_isolated_clones`] is examining,
/// before (and regardless of whether) it turns out to be a genuine
/// deck-owned isolated clone. Git honours that directory's own
/// `.git/config`, and the audit's own lab reproduced arbitrary code
/// execution via a forged `core.fsmonitor` merely by running `git status`
/// there — `-c core.fsmonitor=` blocks exactly that vector for every
/// command this module runs against such a directory. This is NOT full
/// hardening: the audit's final round reproduced arbitrary code execution
/// through a DIFFERENT config key, `filter.<driver>.clean` declared via a
/// candidate's own `.gitattributes`, triggered by the exact `git status`
/// call this helper exists to protect (auditor B2) — the residual is not
/// confined to subcommands this module doesn't issue; it is reachable
/// through the very subcommand this module does issue, via a different
/// config surface than the one already closed. (`core.hooksPath`,
/// `diff.external`, etc. remain reachable too, by subcommands this module
/// genuinely doesn't run.) See the audit's A3/B2 findings for the full
/// scoping and why both are accepted as a same-uid, non-blocker risk
/// regardless — closing the clean-filter vector would mean not running
/// `git status` in an untrusted directory at all, an M4b design question.
fn git_in_untrusted_dir(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(["-c", "core.fsmonitor="]);
    cmd
}

/// Whether a persistent provenance artifact exists for `candidate`
/// (fork#325 M4a, final round — reviewer F13 / auditor A1/B1, replacing
/// the earlier `candidate_shares_history_with`, since removed; M4b —
/// reviewer P1 / auditor C1 / reviewer F2 — replaced the M4a mechanism
/// with a first version of the one this function now checks; fix round 2
/// — reviewer R1 / auditor D1, blocker, PR #515 — relocated it again, to
/// the location described below).
///
/// `provision_isolated_clone_sync` writes
/// [`ISOLATED_CLONE_PROVENANCE_FILENAME`] at
/// [`crate::issue_dispatch_run::isolated_clone_provenance_path`]'s resolved
/// location — under [`crate::platform::paths::state_dir`], keyed by a hash
/// of `candidate`'s own canonical path — immediately after the clone
/// itself succeeds. This function recomputes that same path and checks for
/// the file's presence; nothing else to do, since [`canonicalize_best_effort`]
/// makes write time and check time agree on the path by construction (see
/// [`isolated_clone_provenance_path`]'s own doc comment).
///
/// [`canonicalize_best_effort`]: crate::issue_dispatch_run::canonicalize_best_effort
/// [`isolated_clone_provenance_path`]: crate::issue_dispatch_run::isolated_clone_provenance_path
///
/// This is a stronger binding than the shared-history check M4a replaced,
/// for the same reason [`owned_git_dir`]'s containment check is strong for
/// a linked worktree: the evidence lives at a path only
/// `provision_isolated_clone_sync` itself ever writes to, and — critically,
/// unlike M4b's first attempt — a path outside every candidate's own
/// control (see [`ISOLATED_CLONE_PROVENANCE_FILENAME`]'s own doc comment
/// for the fix-round-2 history). A `same-uid` attacker able only to plant a
/// sibling directory cannot forge this file's presence, closing auditor
/// B1's 4-file/1-SHA forgery (which the shared-history check could not — it
/// only checked that the candidate NAMED a commit `repo_dir` had, never
/// that it HELD one) and closing reviewer R1/auditor D1's bare 3-file
/// forgery of M4b's first attempt (`worktree/reclaim/061`). And like the
/// M4a mechanism, this has no dependency on the candidate's current `HEAD`
/// at all — a genuine clone that has since committed real, local-only work
/// is still recognized, closing reviewer F13.
///
/// **M4a's two residuals, both closed here:**
/// - **Reviewer P1 — shared namespace with ordinary linked worktrees.**
///   M4a's mechanism was the SAME attach-lock file [`create_worktree_sync`]
///   writes for an ordinary linked worktree, so a deck-created-then-removed
///   linked worktree's leftover lock file could be inherited by a later
///   forged occupant of the same path. [`ISOLATED_CLONE_PROVENANCE_FILENAME`]
///   is a wholly separate namespace and location — under `state_dir()`,
///   never under any repository's `.git` at all — that
///   [`create_worktree_sync`] never writes into under any circumstance, so
///   there is nothing left for a forged occupant to inherit
///   (`worktree/reclaim/056`).
/// - **Auditor C1 — the old lock was acquired before `clone_dir.exists()`
///   was checked.** `provision_isolated_clone_sync`'s attach lock (still
///   used, unchanged, for cross-process mutual exclusion — see
///   `worktree_attach_lock_path`'s own doc comment) is still acquired
///   before the `clone_dir.exists()` check, and that ordering is
///   deliberately preserved (moving the check earlier would reopen the
///   `clone_dir.exists()` -> `git clone` TOCTOU fork#325 auditor A3 closed
///   — fork #282's TOCTOU family). But [`ISOLATED_CLONE_PROVENANCE_FILENAME`]
///   is written only afterward, once the clone this call performed has
///   actually succeeded — a pre-planted directory hits
///   `IsolatedCloneOutcome::AlreadyClaimed` and returns before that write is
///   ever reached, so it is never vouched for (`worktree/reclaim/057`).
///
/// **Bonus, from the same design (reviewer F2, `worktree/reclaim/058`):**
/// M4a's mechanism required knowing the enumerating repo's own common
/// `.git` dir to build the lookup path, which — when
/// `discover_isolated_clones` runs from inside an isolated clone itself,
/// exactly what the Nth-concurrent-orchestration gate this milestone
/// serves does — resolved to the CLONE's own common dir, not the root
/// checkout's, so a sibling clone's genuine artifact (written under the
/// ROOT's `.git`) was never found. `state_dir()` needs no such resolution
/// at all: it works identically whether the caller is the root checkout, a
/// subdirectory of it, or another isolated clone entirely.
///
/// Other honest limits, stated plainly rather than overclaimed:
/// - **Same-uid still wins.** An attacker with write access to this
///   process's own `state_dir()` (e.g. this very uid, or having compromised
///   an agent running under it) defeats this exactly as a same-uid attacker
///   defeats `owned_git_dir`'s containment check for a linked worktree —
///   this is categorically as strong as the linked-worktree case, not
///   stronger, and never absolute. What it no longer grants is the WEAKER
///   capability M4b's first attempt granted: an attacker able only to plant
///   a sibling directory, with no access to this process's own `state_dir()`
///   at all, could forge the M4b-round-1 artifact directly. That gap is
///   exactly what this fix closes.
/// - **Best-effort provenance, not proof of current identity.** A clone
///   made by a build predating this check, or one whose write genuinely
///   failed (best-effort, logged and continued — see
///   `write_isolated_clone_provenance`'s own doc comment), has no matching
///   artifact and correctly reports `owned: false` rather than being
///   hidden — mirroring how `owned_git_dir` returning `None` yields
///   `owned: false`, never a dropped row.
/// - **Stale entries.** [`remove_isolated_clone_dir`] now clears the
///   artifact (best-effort) immediately after removing the clone directory
///   it names (fork issue #546 hazard 1), so the removal path this deck
///   itself drives no longer leaves it behind. What remains is narrower
///   than the original claim: a clone destroyed by some other means (a
///   manual `rm -rf`, or a removal whose marker-clearing step itself fails
///   — logged and tolerated, never a hard failure, per that function's own
///   doc comment) still leaves the artifact in place, and it would then
///   vouch for an unrelated directory recreated at the same path
///   afterward, if that later directory canonicalizes to the identical
///   clone-dir path the hash was computed from — in practice this requires
///   reusing the exact same `clone_dir` path after a prior clone there was
///   destroyed outside this deck's own removal path. Broader than a naive
///   per-clone-tree artifact would be (this one outlives the clone's own
///   deletion by design), but no broader than M4a's own shared-namespace
///   staleness, and nothing else in the deck ever writes into this
///   directory.
/// - **Path-bound, same as M4a (reviewer R2, PR #515).** Because the key is
///   the clone's own canonical PATH rather than anything stored inside the
///   clone's tree, `cp -r`/`mv` of a genuine clone to a different sibling
///   path carries no attribution with it — the copy's canonical path hashes
///   to a different (empty) entry, so it correctly reports `owned: false`
///   until the deck itself provisions something there. M4b's first attempt
///   (the candidate-local artifact) had temporarily inverted this, since a
///   file living inside the clone's own tree travels with a `cp -r` for
///   free; this restores M4a's original, narrower staleness window.
fn candidate_has_attach_lock(candidate_path: &Path) -> bool {
    crate::issue_dispatch_run::isolated_clone_provenance_path(candidate_path).is_file()
}

/// Enumerate deck-owned isolated clones sitting as siblings of the ROOT
/// CHECKOUT — not of `repo_dir` itself, and not of the process's current
/// working directory (fork#325 M4a — `provision_isolated_clone_sync`
/// creates them as siblings of the source repo, and its own doc comment
/// names this exact scan as the deferred M4 follow-up).
///
/// Auditor A2: the root checkout is DERIVED via [`resolve_common_dir`] —
/// the same `git rev-parse --path-format=absolute --git-common-dir` CLAUDE.md
/// rule 15 uses to resolve a root checkout from any linked worktree — never
/// assumed to already equal `repo_dir`. `repo_dir` is `std::env::current_dir()`
/// at the CLI layer (`main.rs`), which is whatever subdirectory the
/// invoking shell happens to be in; anchoring the scan on ITS parent (the
/// original defect) silently misses every real isolated clone when run from
/// a subdirectory, AND can misreport a directory genuinely inside the repo
/// (an interrupted e2e temp root, a vendored fixture) as an isolated clone
/// of it. Deriving the root checkout first, and requiring candidates to be
/// its siblings, makes the self-exclusion (`path == root_checkout`) correct
/// by construction rather than by the coincidence that `current_dir()`
/// happened to already be the root.
///
/// Anything else — a plain directory, an unrelated independent repo, a
/// linked worktree of `repo_dir` or of any other repo (its `.git` is a
/// FILE, never a DIRECTORY), a symlink (auditor A1 item 2 — `Path::is_dir()`
/// follows symlinks, so an unfiltered symlinked sibling could point
/// anywhere, including outside `parent` entirely), or a genuine clone
/// carrying no marker at all — is silently skipped, never reported, never
/// an error: discovery is best-effort scanning, not a precondition
/// `worktree list`/`reclaim` can fail on.
///
/// **Final round (reviewer F13 / auditor A1/B1): DISCOVERY no longer
/// requires the candidate's checked-out commit to share history with
/// `repo_dir`.** Earlier rounds gated inclusion on exactly that
/// (`candidate_shares_history_with`, since removed), which was wrong in two
/// independent ways. Reviewer F13: `provision_isolated_clone_sync` checks
/// the clone out onto its work branch, so the dispatched agent's first real
/// commit moves `HEAD` off anything `repo_dir` holds — the check then
/// failed for exactly the clones this milestone exists to surface, the ones
/// holding local-only work. Auditor A1/B1: the check only verified the
/// candidate NAMED a commit `repo_dir` had (a public, non-secret value),
/// never that it HELD one — a 4-file forgery with no git invocation at all
/// passed it. Discovery is therefore purely structural again, the same
/// shape it had before that check existed: `.git` is a directory, and the
/// marker file is readable. What used to be a discovery gate is now
/// `candidate_has_attach_lock`'s job instead, resolved once per candidate
/// below and carried on [`IsolatedCloneCandidate`] — but that check decides
/// `owned` on the eventual [`WorktreeReport`], not whether the row is
/// reported at all. This mirrors [`owned_git_dir`]'s role for a LINKED
/// worktree: `git worktree list` alone decides which linked rows are
/// reported, and `owned_git_dir`'s containment check only ever decides
/// `owned`/`owner` on rows already known to exist. A directory carrying
/// only a forged marker (no genuine attach lock) is therefore still listed
/// — same as a foreign linked worktree is still listed — but reports
/// `owned: false`, so it can never be misattributed to a victim identity
/// via `worktree list --mine` the way auditor B1 demonstrated. The
/// tightened M4c eligibility rule (see [`isolated_clone_report`]'s own doc
/// comment) requires `has_attach_lock` as one of its five AND'd conditions
/// — a forged clone with no genuine attach lock cannot produce the deck's
/// own provenance artifact, so it can never satisfy that condition — meaning
/// a bogus row surviving discovery cannot become eligible for auto-removal
/// (auditor A2, per the maintainer-decided tightening; this was NOT true of
/// the rule's first, since-tightened version, which compared only a
/// commit-SHA equality a forged row could in principle be made to satisfy).
///
/// Auditor A5 (not fixed here, deliberately): per-sibling work is uncapped.
/// [`isolated_clone_report`] spawns 3-5 `git`/`gh` processes per accepted
/// clone (slug, branch, clean, PR state, and `gh pr list`'s network round
/// trip when a branch resolves — the slug spawn is new as of auditor B3's
/// final-round fix, up from the 2-4 this paragraph originally measured) —
/// this function itself no longer spawns
/// anything extra per marked candidate, since `candidate_has_attach_lock`
/// is a filesystem check, not a subprocess (strictly cheaper than the
/// two-spawn check it replaces). The auditor's 2.08s/43-sibling measurement
/// predates that removal, so the real cost today is lower, not higher; a
/// cap/early-exit remains left as a known, documented cost rather than
/// fixed speculatively, revisit if this scales beyond a handful of
/// siblings in practice.
///
/// M4a Windows/macOS fix: [`resolve_common_dir`]'s `git rev-parse
/// --path-format=absolute` goes through the OS's own realpath resolution at
/// the point `git` calls `getcwd()` after `chdir`ing into `repo_dir` —
/// confirmed directly (`git -C <dir-under-a-symlink> rev-parse
/// --path-format=absolute --git-common-dir` returns the `/private/var/...`
/// form on macOS even when invoked against the unresolved `/var/...` path).
/// A candidate path built by joining onto that resolved anchor therefore
/// mixes a git-resolved fragment with a plain `Path::join`ed one: on macOS
/// this yields a `/private/var/...` `real_path` a caller who never resolved
/// `/var` itself will never match; on Windows the equivalent short-name
/// resolution combines with a `Path::join`-appended component to leave a
/// forward-slash-formatted prefix followed by a backslash-joined suffix in
/// the same string. Neither is a bug `Path`/`PathBuf` construction alone
/// fixes, since both fragments already go through `Path::join` — the
/// mismatch is that they resolve through two different means (the OS via
/// `git`, vs. none at all via `Path`). When `repo_dir` already names the
/// same directory as the derived root checkout (the common case — `repo_dir`
/// was not called against a linked worktree), anchoring on `repo_dir` itself
/// keeps every sibling path in the caller's own, unresolved spelling instead
/// of git's resolved one; the derived `root_checkout` remains the fallback
/// anchor for the case this function exists to handle (`repo_dir` being a
/// subdirectory or a linked worktree), so A2's own fix is unaffected.
///
/// **M4b (reviewer F2): `repo_dir` being an isolated clone itself.** When
/// `repo_dir` is an isolated clone, `common_dir`/`root_checkout` above
/// resolve to the CLONE's own `.git`/directory, not the actual root
/// checkout's — but that only matters for the anchor/parent computation
/// above, and the clone's parent directory already IS the root checkout's
/// parent (M4a provisions every isolated clone as a sibling of its source),
/// so sibling PATHS are still found correctly by construction. What used to
/// break was `has_attach_lock`'s check, resolved against the wrong
/// `common_dir` — fixed by keying that check off `state_dir()` instead of
/// any repository's own `.git` (see [`ISOLATED_CLONE_PROVENANCE_FILENAME`]'s
/// and [`candidate_has_attach_lock`]'s own doc comments), so it needs no
/// `common_dir` at all any more.
fn discover_isolated_clones(repo_dir: &Path) -> Vec<IsolatedCloneCandidate> {
    let Some(common_dir) = resolve_common_dir(repo_dir) else {
        return Vec::new();
    };
    let Some(root_checkout) = common_dir.parent().map(Path::to_path_buf) else {
        return Vec::new();
    };
    let anchor = if paths_refer_to_same_dir(repo_dir, &root_checkout) {
        repo_dir.to_path_buf()
    } else {
        root_checkout
    };
    let Some(parent) = anchor.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        // Auditor A1 item 2: checked via `file_type()`, which does NOT
        // traverse the symlink (unlike the `path.is_dir()` probe below),
        // before anything that would.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if path == anchor || !path.is_dir() {
            continue;
        }
        let git_dir = path.join(".git");
        if !git_dir.is_dir() {
            continue;
        }
        if !git_dir.join(OWNER_MARKER_FILENAME).is_file() {
            continue;
        }
        // Final round: the attach-lock check is a filesystem stat, not a
        // `git` spawn, so resolving it for every marked candidate (rather
        // than gating on it) costs nothing extra — see this function's own
        // doc comment for why it no longer gates inclusion. Keyed off the
        // candidate's own PATH, not its `.git` dir (fix round 2 -- the
        // artifact no longer lives anywhere under the candidate at all).
        let has_attach_lock = candidate_has_attach_lock(&path);
        found.push(IsolatedCloneCandidate {
            path,
            git_dir,
            has_attach_lock,
        });
    }
    found
}

/// Resolve an isolated clone's current branch via `git symbolic-ref --short
/// -q HEAD`, run inside the clone itself — `None` for a detached HEAD (a
/// non-zero exit; `-q` suppresses the stderr message but not the exit
/// code) or any spawn/parse failure, matching `RawWorktree::branch`'s own
/// "no branch to report" meaning closely enough for [`resolve_pr_state`] to
/// treat the two identically. Runs via [`git_in_untrusted_dir`] (auditor
/// A3), consistently with every other `git` invocation this module makes
/// with `current_dir` set inside a candidate — [`check_cleanliness`] and,
/// as of the final round, [`derive_repo_slug`] too (auditor B3). `gh pr
/// list` is a different binary this helper was never scoped to; see
/// [`isolated_clone_report`]'s own doc comment for how that call is gated
/// instead (requiring the candidate's derived slug to match the root
/// checkout's own before it is ever spent).
fn resolve_isolated_clone_branch(clone_dir: &Path) -> Option<String> {
    let out = git_in_untrusted_dir(clone_dir)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = trim_trailing_newline(&out.stdout);
    if raw.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(raw).into_owned())
}

/// Resolve an isolated clone's own current HEAD **commit** SHA via `git
/// rev-parse HEAD`, run inside the clone itself (fork#325 M4c, PR #526 round
/// 3, reviewer B2) — `None` on any spawn/exit/parse failure. Mirrors
/// [`resolve_isolated_clone_branch`]'s shape exactly, including running via
/// [`git_in_untrusted_dir`] for the same reason: this is a `git` invocation
/// with `current_dir` set inside an untrusted candidate. Used by
/// [`isolated_clone_report`] to compare against a merged PR's own
/// `headRefOid` ([`PrState::Merged::head_ref_oid`]) — plain commit-SHA
/// equality, replacing round 2's `HEAD^{tree}` comparison
/// (`resolve_isolated_clone_head_tree_sha`, removed): `headRefOid` names the
/// PR branch's own tip commit, which for a deck-provisioned clone that has
/// merged cleanly and picked up no local drift IS the clone's own `git
/// rev-parse HEAD` — no tree-level comparison needed once the round-2
/// mismatch (comparing against the base-branch merge commit instead) is
/// fixed at the source.
fn resolve_isolated_clone_head_sha(clone_dir: &Path) -> Option<String> {
    let out = git_in_untrusted_dir(clone_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = trim_trailing_newline(&out.stdout);
    if raw.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(raw).into_owned())
}

/// Resolve an isolated clone's local branches via `git for-each-ref
/// --format=%(refname:short) refs/heads`, run inside the clone itself
/// (fork#325 M4c tightening, auditor A3) — one line per local branch,
/// `None` on any spawn/exit failure (fails closed: eligibility must never
/// be decided from a branch listing that could not be resolved). Mirrors
/// [`resolve_isolated_clone_branch`]'s shape, including running via
/// [`git_in_untrusted_dir`]. Used by [`isolated_clone_report`] to require
/// EXACTLY one local branch (the resolved current branch, no others) before
/// treating a clone as reclaim-eligible: `git rev-parse HEAD`/its tree
/// proves only that ONE ref is safe to discard, while `remove_dir_all`
/// destroys the whole clone, including any other local branch that may hold
/// commits with no copy anywhere else.
fn resolve_isolated_clone_local_branches(clone_dir: &Path) -> Option<Vec<String>> {
    let out = git_in_untrusted_dir(clone_dir)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .map(|line| line.to_string())
            .filter(|line| !line.is_empty())
            .collect(),
    )
}

/// Resolve an isolated clone's `git stash list` via `git stash list`, run
/// inside the clone itself (fork#325 M4c tightening, auditor A3's other
/// half) — one line per stash entry, `None` on any spawn/exit failure
/// (fails closed, same reasoning as [`resolve_isolated_clone_local_branches`]).
/// Mirrors that function's shape, including running via
/// [`git_in_untrusted_dir`]. Used by [`isolated_clone_report`] to require an
/// EMPTY stash list before treating a clone as reclaim-eligible: a stash
/// entry is local-only content `remove_dir_all` would destroy with no copy
/// anywhere else, the same hazard an extra local branch is.
fn resolve_isolated_clone_stash_list(clone_dir: &Path) -> Option<Vec<String>> {
    let out = git_in_untrusted_dir(clone_dir)
        .args(["stash", "list"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .map(|line| line.to_string())
            .filter(|line| !line.is_empty())
            .collect(),
    )
}

/// Build the report row for one discovered isolated clone (fork#325 M4a).
/// Cleanliness and PR state ARE probed — real signals, useful to a human
/// scanning `worktree list`'s CLEAN/PR columns — but neither one, nor any
/// combination of them, is ever allowed to decide removal: unlike
/// `list_linked_worktrees`'s rows, this bypasses [`decide`] entirely and
/// hard-codes `verdict`/`reason`, mirroring
/// [`crate::issue_dispatch_run::RemovalPolicy::IsolatedClone`]'s daemon-side
/// precedent (`remove_worktree`'s own doc: "the entry is kept
/// unconditionally... a clean working tree does not prove it is safe to
/// remove_dir_all — this clone's `.git` may hold the only copy of commits
/// made on its local branch"). Deliberately never routes through
/// `Verdict::Ask` either (the task's own explicit call): `--yes` would
/// otherwise reach [`run_reclaim`]'s `remove_worktree_dir`, which shells out
/// to `git worktree remove` — a command that fails loudly (exit 128)
/// against something that isn't a linked worktree at all, rather than
/// cleanly declining. `verdict` is set to [`KIND_ISOLATED_CLONE`] itself —
/// a value distinct from `"remove"`/`"ask"` — so [`run_reclaim`]'s string
/// match falls through to its `_ => kept.push(r)` arm unconditionally,
/// with no new match arm needed there at all.
///
/// **Fork#325 M4c exception (maintainer-decided rule, tightened after an
/// audit found the first shipped version had 3 blocker-severity gaps),
/// widened to six by fork issue #546 hazard 2:** the one condition under
/// which `verdict` is NOT hard-coded to [`KIND_ISOLATED_CLONE`] is the AND
/// of all six: (1) `has_attach_lock` —
/// the deck's own provenance artifact, since a forged sibling directory can
/// satisfy every other condition below with no deck involvement at all
/// (auditor A2); (2) the working tree is [`Cleanliness::Clean`] —
/// `remove_dir_all` has no equivalent of `git worktree remove`'s own refusal
/// against a dirty tree, so eligibility must consult cleanliness directly
/// rather than leaving it a display-only field (auditor A1); (3) exactly one
/// local branch, the resolved current one — HEAD-equality proves one ref
/// safe to discard, never the whole clone `remove_dir_all` destroys, which
/// may hold a second branch with commits no copy exists of elsewhere
/// (auditor A3); (4) an empty `git stash list` — the same local-only-content
/// hazard as (3), through git's other local-only ref namespace (auditor A3);
/// and (5) a MERGED PR whose own `headRefOid` equals this clone's own
/// current HEAD commit SHA exactly ([`VERDICT_ISOLATED_CLONE_RECLAIMABLE`],
/// fork#325 M4c, PR #526 round 3, reviewer B2) — the PR BRANCH's own tip
/// commit, not the merge commit GitHub creates on the base branch (round
/// 2's now-removed `mergeCommit`-tree comparison): reviewer B2 measured
/// live against this repo that a deck-provisioned clone's own HEAD is never
/// equal to `mergeCommit.oid` under any GitHub merge strategy (PR #481 head
/// `7339edd5f440` vs merge `1ceb919349ef`; PR #477 head `11d6327f2421` vs
/// merge `5742ad1f93dd`), so round 2's rule could (almost) never fire for a
/// genuine clone — `headRefOid` is exactly the clone's own reachable commit
/// history instead (auditor A8's original tree-SHA tightening is
/// superseded by this redesign, not layered on top of it); and (6) the
/// clone must not be explicitly pinned
/// ([`crate::issue_dispatch_run::pin_isolated_clone`]/
/// [`unpin_isolated_clone`], fork issue #546 hazard 2) — a deliberate,
/// caller-set override read via [`isolated_clone_provenance_field`]'s
/// `pinned=` field, trusted only once `has_attach_lock` has already
/// verified the artifact is this deck's own; only a literal `pinned=true`
/// counts (see the Known residual paragraph below for what this override
/// closes and what it still doesn't). [`run_reclaim`]
/// gets a dedicated match arm for [`VERDICT_ISOLATED_CLONE_RECLAIMABLE`],
/// using a removal primitive other than `remove_worktree_dir`'s `git
/// worktree remove` (which fails loudly against a plain clone, not a linked
/// worktree). Every other case is unaffected and still hard-codes
/// [`KIND_ISOLATED_CLONE`] exactly as before this milestone.
/// `None`/unresolvable anywhere in this six-way chain fails closed to "not
/// eligible" — the same stance the pre-tightening rule took for an
/// unresolvable merge SHA.
///
/// **Known residual (M2, PR #526 round 3; narrowed by fork issue #546
/// hazard 2):** the original five conditions prove PROVENANCE (the deck
/// created this clone) and CONTENT SAFETY (nothing would be lost), never
/// LIVENESS (whether the clone is currently in active use). Issue #325's
/// original incident was exactly "an orchestration deleted another
/// orchestration's actively-in-use worktree" — and a clone could satisfy
/// every one of the original five conditions while a live orchestration
/// was still working in it (e.g. about to make a new commit). The daemon
/// tracks liveness for its own dispatched worktrees in-process
/// ([`crate::issue_dispatch_run::WorktreeRegistry`] /
/// `worktree_still_in_use`), but `worktree reclaim` is a plain CLI
/// subprocess (`run_worktree_reclaim_cli`) that never connects to the
/// daemon socket at all — that in-memory signal is still not reachable
/// from this call path, and nothing here invents an automatic substitute
/// for it (a heuristic liveness probe would be worse than none: a false
/// "not live" reads as license to delete). Fork issue #546's sixth
/// condition is not that automatic signal — it is a manual one: whoever
/// (or whatever) knows a clone is still in active use, or otherwise wants
/// it kept, can now say so explicitly via `pin_isolated_clone` and have
/// this function honor it. That closes the "no way to say keep this one"
/// gap for a clone someone is deliberately still resuming by name, but it
/// is opt-in and does nothing for a clone nobody thought to pin — a fully
/// automatic liveness signal `worktree reclaim` can consult without being
/// told is still open, consistent with this module's existing honesty
/// about other limits (see `owned`'s same-uid caveat above).
///
/// Final round (reviewer F13 / auditor A1/B1): `owned` and `owner`/
/// `owner_kind` below are now backed by `candidate.has_attach_lock` —
/// whether [`candidate_has_attach_lock`] found the deck's own provenance
/// artifact for this candidate under `state_dir()` (fix round 2 — see that
/// function's own doc comment for why it is no longer looked up under any
/// repository's `.git` at all) — resolved once in
/// [`discover_isolated_clones`] and carried on
/// [`IsolatedCloneCandidate`], mirroring how [`owned_git_dir`]'s
/// containment check backs a linked row's `owned`. See
/// `candidate_has_attach_lock`'s own doc comment for exactly what that
/// artifact proves and its honest limits (same-uid still wins; best-effort
/// provenance, not proof of current identity; a stale lock can outlive the
/// clone it named). The marker's content is trusted — read at all — only
/// when the attach lock is present; without it, `owner`/`owner_kind`
/// report `"unknown"` with [`ISOLATED_CLONE_NO_ATTACH_LOCK_REASON`] rather
/// than surfacing an unread, potentially forged claim (auditor B1's
/// demonstrated attack). No removal decision is ever taken on any of this
/// regardless — this function never calls [`decide`].
fn isolated_clone_report(
    repo_slug: Option<&str>,
    candidate: IsolatedCloneCandidate,
) -> WorktreeReport {
    let IsolatedCloneCandidate {
        path,
        git_dir,
        has_attach_lock,
    } = candidate;
    let branch = resolve_isolated_clone_branch(&path);
    let cleanliness = check_cleanliness(&path);
    let clean = cleanliness == Cleanliness::Clean;
    // Auditor B3 (final round): `derive_repo_slug`/`gh pr list` are steered
    // entirely by the candidate's own `origin`, which
    // `provision_isolated_clone_sync` deliberately points at the SOURCE's
    // origin rather than a local path (reviewer P1-1's prior decision) —
    // but an untrusted candidate's `origin` can still be repointed by
    // whoever wrote it. Requiring the candidate's derived slug to equal the
    // root checkout's own before ever spending a `gh` call rejects that for
    // (almost) free: a genuine deck-provisioned clone always matches, and a
    // mismatch is itself a strong signal the candidate isn't one.
    let pr_state = match &branch {
        Some(b) if repo_slug.is_some() && derive_repo_slug(&path).as_deref() == repo_slug => {
            resolve_pr_state(&path, b)
        }
        Some(_) => PrState::Unresolvable(
            "isolated clone's derived repo slug does not match the root checkout's own -- gh is \
             never queried against a repository this candidate's own (untrusted) origin chose \
             (fork#325 M4a, auditor B3)"
                .to_string(),
        ),
        None => PrState::Unresolvable("worktree has no branch (detached HEAD)".to_string()),
    };
    // Fork#325 M4c redesigned rule (maintainer-decided, PR #526 round 3,
    // reviewer B2): the only condition that ever widens this row's verdict
    // past the permanently-conservative `KIND_ISOLATED_CLONE` default is the
    // AND of all five gates below -- see this function's own doc comment for
    // why each one is required, and for M2's documented liveness residual.
    // Every resolution here fails closed: a `None` anywhere in the chain (an
    // unresolvable branch listing, stash list, or HEAD SHA) makes the
    // corresponding gate `false`, never treated as a pass.
    let head_sha = resolve_isolated_clone_head_sha(&path);
    let local_branches = resolve_isolated_clone_local_branches(&path);
    let stash_list = resolve_isolated_clone_stash_list(&path);
    let single_local_branch = branch
        .as_ref()
        .zip(local_branches.as_ref())
        .is_some_and(|(b, branches)| branches == &vec![b.clone()]);
    let stash_empty = stash_list.as_ref().is_some_and(Vec::is_empty);
    let head_matches_merge = matches!(
        &pr_state,
        PrState::Merged {
            head_ref_oid: Some(oid),
        } if head_sha.as_deref() == Some(oid.as_str())
    );
    // Fork issue #546 hazard 2 (maintainer-decided design): a sixth,
    // independent gate on top of the original five -- an explicit pin is
    // never inferred from the other five holding, and is only ever trusted
    // once `has_attach_lock` has already verified this artifact is the
    // deck's own (the same trust gate `owner`/`owner_kind` below apply to
    // this same artifact's other fields). Only the literal `pinned=true`
    // counts as pinned; a missing field (no artifact, or a pre-#546
    // `schema=2` artifact that predates the pin mechanism entirely), an
    // explicit `pinned=false` (`unpin_isolated_clone`), or any other value
    // all read as not pinned -- `pin_isolated_clone`/`unpin_isolated_clone`
    // only ever write `true` or `false` here, so anything else is not
    // evidence this deck itself produced. An artifact that exists but
    // cannot be READ (reviewer/auditor gap 2) is a THIRD outcome, distinct
    // from both -- see [`isolated_clone_pin_state`]'s own doc comment for
    // why it fails closed (`pin_unresolvable`) rather than collapsing into
    // "not pinned".
    let pin_state = has_attach_lock.then(|| isolated_clone_pin_state(&path));
    let is_pinned = matches!(pin_state, Some(Ok(true)));
    let pin_unresolvable = matches!(pin_state, Some(Err(_)));
    let is_reclaim_eligible = has_attach_lock
        && clean
        && single_local_branch
        && stash_empty
        && head_matches_merge
        && !is_pinned
        && !pin_unresolvable;
    let (verdict, reason) = if is_reclaim_eligible {
        (
            VERDICT_ISOLATED_CLONE_RECLAIMABLE.to_string(),
            "isolated clone: owned, clean, has exactly one local branch and no stash entries, \
             and this clone's own HEAD commit SHA equals the merged PR's headRefOid exactly -- \
             eligible for automatic reclaim (fork#325 M4c, PR #526 round 3, reviewer B2)"
                .to_string(),
        )
    } else {
        // Reviewer L2 (PR #526 final round): this reason string used to
        // claim the whole CLASS of isolated clones is "never auto-removed
        // ... regardless of PR state or cleanliness" -- false as of M4c,
        // which made some rows genuinely eligible (the branch above). Name
        // which of the five fork#325 M4c gates THIS row actually failed
        // instead of restating the pre-M4c blanket claim.
        let mut unmet_gates: Vec<&str> = Vec::new();
        if !has_attach_lock {
            unmet_gates.push("no deck attach-lock provenance artifact");
        }
        if !clean {
            unmet_gates.push("working tree is not clean");
        }
        if !single_local_branch {
            unmet_gates.push("does not have exactly one local branch");
        }
        if !stash_empty {
            unmet_gates.push("git stash list is not empty");
        }
        if !head_matches_merge {
            unmet_gates.push("HEAD commit SHA does not equal a merged PR's headRefOid");
        }
        if is_pinned {
            unmet_gates.push("isolated clone is explicitly pinned (fork issue #546)");
        }
        if pin_unresolvable {
            unmet_gates.push(
                "isolated clone's pin state could not be read -- fails closed, treated as \
                 pinned (fork issue #546 hazard 2)",
            );
        }
        (
            KIND_ISOLATED_CLONE.to_string(),
            format!(
                "isolated clone: not eligible for automatic reclaim -- {} (fork#325 M4c \
                 requires all five: a deck attach-lock, a clean tree, exactly one local \
                 branch, an empty stash, and HEAD == a merged PR's headRefOid; fork#546 adds a \
                 sixth: the clone must not be explicitly pinned, and an unreadable pin signal \
                 fails closed exactly like every other unresolvable signal here)",
                unmet_gates.join(", ")
            ),
        )
    };
    // Discovery already required this marker to be `is_file()`, but its
    // content is only trusted once `has_attach_lock` has confirmed the
    // deck's own attach-lock artifact for this candidate (final round —
    // see `ISOLATED_CLONE_NO_ATTACH_LOCK_REASON`'s own doc comment for why
    // an unread marker is safer than a read-but-unverified one).
    let identity = if has_attach_lock {
        read_marker_owner(&git_dir.join(OWNER_MARKER_FILENAME))
    } else {
        None
    };
    let (owner, owner_kind, owner_reason) = match identity {
        Some(id) => (Some(id), "agent".to_string(), None),
        None if has_attach_lock => (
            // Auditor A4: this path's own reason, not the linked path's
            // `LEGACY_MARKER_UNKNOWN_REASON` — that text asserts "it proves
            // deck-creation," which this path cannot back up (see
            // `ISOLATED_CLONE_MARKER_UNKNOWN_REASON`'s own doc comment).
            None,
            "unknown".to_string(),
            Some(ISOLATED_CLONE_MARKER_UNKNOWN_REASON.to_string()),
        ),
        None => (
            None,
            "unknown".to_string(),
            Some(ISOLATED_CLONE_NO_ATTACH_LOCK_REASON.to_string()),
        ),
    };
    let real_path = path.clone();
    WorktreeReport {
        path,
        branch,
        clean,
        owned: has_attach_lock,
        pr_state: pr_state.label().to_string(),
        verdict,
        reason: Some(reason),
        owner,
        owner_kind,
        owner_reason,
        real_path,
        removed_by: None,
        kind: KIND_ISOLATED_CLONE.to_string(),
        pinned: is_pinned || pin_unresolvable,
    }
}

/// Resolve fork issue #546 hazard 2's pin gate for one isolated clone,
/// shared by [`isolated_clone_report`]'s examination-time check and
/// [`remove_isolated_clone_dir`]'s TOCTOU re-verification so both read the
/// same artifact through the same logic rather than each growing its own
/// parser (reviewer/auditor gap 1). `Ok(true)` means the artifact was read
/// successfully and its `pinned=` field is the literal string `"true"`;
/// `Ok(false)` means it was read successfully and the field is absent,
/// `"false"`, or any other value. `Err` means the artifact could not be
/// read at all (permission denied, or any other I/O error) -- **the caller
/// must treat that identically to `Ok(true)`, never to `Ok(false)`**
/// (reviewer/auditor gap 2): every other gate in this heuristic fails
/// closed on an unresolvable signal (`None` anywhere in the chain makes
/// the corresponding gate `false`, never a pass), and collapsing a read
/// error to "not pinned" via `.ok()` was this exact heuristic failing
/// OPEN on the one gate the whole hazard exists to enforce. Only called
/// once `has_attach_lock` is already known `true` -- the artifact's
/// content is trusted only once that has verified it is this deck's own
/// (the same trust gate `owner`/`owner_kind` apply to this same
/// artifact's other fields).
fn isolated_clone_pin_state(path: &Path) -> Result<bool, String> {
    match std::fs::read_to_string(crate::issue_dispatch_run::isolated_clone_provenance_path(
        path,
    )) {
        Ok(content) => Ok(crate::issue_dispatch_run::isolated_clone_provenance_field(
            &content, "pinned",
        )
        .as_deref()
            == Some("true")),
        Err(e) => Err(format!(
            "isolated clone provenance artifact could not be read to resolve its pin state: {e}"
        )),
    }
}

const DASH: &str = "-";

fn cell(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or(DASH)
}

/// Render the `worktree list` human table: one row per examined worktree,
/// including its verdict and reason so the output is self-explanatory.
///
/// PATH, BRANCH, OWNER and REASON are all untrusted -- attacker-reachable by
/// varying provenance (a worktree directory name, a `git` ref name, marker
/// content that survived [`sanitize_marker_creator`]'s `Cc`-only filter, or
/// raw subprocess stderr) -- so each is routed through a display-safe escape
/// before it reaches this TAB-separated row: an unescaped raw TAB in any of
/// them would also forge a column boundary and shift every later cell. PATH
/// goes through [`display_path`] rather than [`sanitize_path_for_terminal_display`]
/// -- issue #578, injectivity for byte-exact reclaim decisions -- while
/// BRANCH, OWNER and REASON go through [`sanitize_for_terminal_display`]
/// (issue #232). `PR`, `CLEAN`, `OWNED` and `VERDICT` are internal
/// enum/boolean labels this crate produces itself, never attacker content,
/// so they are not sanitized.
pub fn format_list_human(reports: &[WorktreeReport]) -> String {
    if reports.is_empty() {
        return "no worktrees found\n".to_string();
    }
    let mut out = String::new();
    out.push_str("PATH\tBRANCH\tPR\tCLEAN\tOWNED\tOWNER\tVERDICT\tREASON\n");
    for r in reports {
        let path = display_path(&r.path);
        let branch = sanitize_for_terminal_display(cell(&r.branch));
        let owner = sanitize_for_terminal_display(cell(&r.owner));
        let reason = sanitize_for_terminal_display(r.reason.as_deref().unwrap_or(DASH));
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            path,
            branch,
            r.pr_state,
            if r.clean { "yes" } else { "no" },
            if r.owned { "yes" } else { "no" },
            owner,
            r.verdict,
            reason,
        ));
    }
    out
}

/// Renders the exact line `dot-agent-deck worktree list` writes to stderr
/// when enumeration itself fails (issue #232 round 4) -- the CLI wrapper in
/// `main.rs` calls this instead of composing `sanitize_for_terminal_display`
/// inline, so a test can call the identical production code rather than
/// re-deriving its composition: reverting the sanitizer inside here breaks
/// this function and the CLI sink together, because they are now the same
/// call.
pub fn format_list_error_for_cli(e: &str) -> String {
    format!("worktree list: {}", sanitize_for_terminal_display(e))
}

/// Physically remove a worktree directory, preserving its branch:
/// `git -C <repo_dir> worktree remove -- <path>` — deliberately WITHOUT
/// `--force`, since [`examine_worktrees`] already gated on cleanliness; git's
/// own refusal on an unexpectedly dirty tree is a second line of defense
/// rather than something to override. The `--` separator (issue #144 finding
/// 4) is not reachable today — not every [`WorktreeReport::real_path`] comes
/// from `git worktree list` any more (fork#325 M4a's isolated-clone rows
/// come from `read_dir` instead), but every path THIS function actually
/// receives still does: [`run_reclaim`]'s match never sends an isolated-clone
/// row (`verdict == "isolated_clone"` OR, since fork#325 M4c, `verdict ==
/// "isolated_clone_reclaimable"`) to this function -- the former falls
/// through to `kept` unconditionally, the latter is routed to
/// `remove_isolated_clone_dir` instead (see [`isolated_clone_report`]'s own
/// doc comment) -- so the conclusion holds even though its old
/// justification — "every path here" — no longer describes every row this
/// crate examines.
/// `git worktree list` still always emits absolute paths, so none of the
/// paths reaching this function can start with `-` — but the separator
/// costs nothing and removes the assumption regardless.
///
/// Takes the real `Path`, never a lossily-converted string (issue #144
/// finding 4): `git worktree remove` realpath/symlink-resolves its argument
/// rather than string-matching it against the registry, so a lossy path that
/// happens to resolve to a DIFFERENT registered worktree removes that one,
/// not merely fails — passing the byte-exact path closes the divergence at
/// its source instead of defending against it downstream. [`WorktreeReport`]
/// still carries a lossy `path: String` for the report/JSON document; only
/// this call site needs the exact bytes.
fn remove_worktree_dir(repo_dir: &Path, worktree_path: &Path, remover: &str) -> Result<(), String> {
    let out = Command::new("git")
        .current_dir(repo_dir)
        .args(["worktree", "remove", "--"])
        .arg(worktree_path)
        .output()
        .map_err(|e| {
            format!("failed to spawn `git worktree remove` (requested by {remover}): {e}")
        })?;
    if out.status.success() {
        // Issue #325 / reviewer B1 / auditor F2: the ONLY durable trace of a
        // confirmed removal, so a post-incident reader has something to grep
        // `DOT_AGENT_DECK_LOG` for even if `format_reclaim_human`'s printed
        // report is long gone. `remover` is an unauthenticated,
        // caller-supplied string (auditor F3) -- sanitize it here exactly as
        // `format_reclaim_human` sanitizes it for the terminal, since a log
        // file gets `cat`/`tail`ed to a terminal too.
        tracing::info!(
            path = %sanitize_path_for_terminal_display(worktree_path),
            remover = %sanitize_for_terminal_display(remover),
            "worktree removed"
        );
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Physically remove an isolated clone directory (fork#325 M4c) — used only
/// for [`VERDICT_ISOLATED_CLONE_RECLAIMABLE`] rows, never for a `"linked"`
/// worktree row (those go through [`remove_worktree_dir`] instead). An
/// isolated clone is a plain directory containing its own `.git`, not a
/// `git worktree list`-registered entry, so `git worktree remove` is the
/// wrong primitive here — it realpath/symlink-resolves its argument against
/// the registry and exits 128 against anything that isn't a linked worktree
/// at all (see [`remove_worktree_dir`]'s own doc comment). `std::fs::
/// remove_dir_all` is the direct analogue.
///
/// Mirrors [`remove_worktree_dir`]'s safety shape as closely as the
/// different removal primitive allows: takes the real `Path`, never a
/// lossily-converted string; requires `worktree_path` to itself contain a
/// `.git` DIRECTORY (an isolated clone's own repository metadata, checked
/// via [`Path::is_dir`] rather than merely [`Path::exists`] — a `.git` FILE
/// marks a linked worktree, not an isolated clone, and this function must
/// never be pointed at one) immediately before removing, as a last-moment
/// structural check that the caller is deleting something shaped like the
/// isolated clone [`isolated_clone_report`] examined and not an arbitrary
/// path that has since changed underneath it; and logs the same durable
/// `tracing::info!` trace on success, sanitized identically, so a
/// post-incident reader has one place to grep `DOT_AGENT_DECK_LOG` for
/// either removal path.
///
/// **M1 TOCTOU re-verification (fork#325 M4c, PR #526 round 3, reviewer
/// M1; widened to six by fork issue #546 hazard 1):** the `.git`-shape check
/// above is only a structural sanity check, not a re-run of the eligibility
/// gate — [`examine_worktrees`]'s examination pass and this removal can be
/// seconds to minutes apart on a large reclaim batch, and the clone could
/// have changed state in that window: a new commit, a stash push, a second
/// branch, uncommitted content, or (fork issue #546 hazard 2) a pin applied
/// after examination confirmed the clone eligible. Immediately before the
/// actual `remove_dir_all`, 5 of the 6 signals [`isolated_clone_report`]'s
/// eligibility gate reads are re-derived FRESH from `worktree_path` alone —
/// cleanliness, the local branch list, the stash list, the merged PR's
/// `headRefOid` compared against a freshly re-resolved `git rev-parse
/// HEAD`, and (via [`isolated_clone_pin_state`], the same helper
/// [`isolated_clone_report`] itself calls) the clone's own pin state —
/// rather than trusting anything computed during examination; any mismatch
/// refuses (returns `Err`, never deletes), and an unreadable pin signal
/// refuses exactly like every other unresolvable signal in this chain,
/// never silently treated as unpinned. **Fork issue #533** adds one more
/// re-check guarding how the `headRefOid` signal itself gets computed:
/// immediately before `resolve_pr_state` is called (mirroring
/// [`isolated_clone_report`]'s own slug-equality guard, auditor B3), the
/// candidate's own derived repo slug must still equal `root_repo_slug` —
/// the ROOT checkout's own slug, re-derived by [`run_reclaim`] at removal
/// time, not a value cached from examination — so `resolve_pr_state` is
/// never spent against a repository the candidate's own (untrusted)
/// `origin` chose. This deliberately takes the signature's own **three**
/// arguments as its only input (matching [`worktree/reclaim/071`]'s
/// direct-call contract) rather than threading the examination pass's
/// cached values through, so a caller can never accidentally pass a stale
/// expectation — `root_repo_slug` is not an exception to that: it is the
/// trusted root's own current slug, not anything computed about this
/// candidate during examination.
///
/// **`has_attach_lock` is deliberately the one signal whose PRESENCE is NOT
/// re-derived here** (auditor N5 / reviewer N3, PR #526 final round). The
/// other five reflect content a legitimately concurrent process could
/// change during the examination-to-removal window — a new commit, a stash
/// push, a second branch, an untracked file, or a pin — which is exactly the
/// hazard this re-verification exists to catch. The attach-lock provenance
/// artifact's presence is nothing like that: the file is created once,
/// under `state_dir()`, at `provision_isolated_clone_sync` time, keyed by
/// the clone's own canonical PATH rather than anything inside its tree, and
/// this function's own marker-clearing step (fork issue #546 hazard 1,
/// below) is the only thing that ever removes the file outright — and that
/// step runs only after `remove_dir_all` has already succeeded, later in
/// this same function body. So during the examination-to-removal window
/// this re-verification actually covers, nothing deletes the marker file:
/// its PRESENCE cannot legitimately flip from present to absent in that
/// window the way the other five signals can flip from safe to unsafe.
/// (Corrected by fork issue #546 hazard 1: this comment previously extended
/// that same "cannot legitimately flip" claim to the marker's CONTENT as a
/// whole, which is false — [`crate::issue_dispatch_run::pin_isolated_clone`]/
/// `unpin_isolated_clone` rewrite that exact artifact's `pinned=` field at
/// arbitrary times, entirely independent of this removal window. That is
/// precisely why the pin state is now re-derived above as its own signal
/// rather than folded into "has_attach_lock cannot change.") And because
/// the has_attach_lock check is purely path-keyed rather than
/// clone-identity-keyed, re-deriving its PRESENCE here would answer the
/// identical question [`isolated_clone_report`] already answered at
/// examination time — it cannot even detect the one adjacent hazard that
/// sounds similar (a different directory swapped in at the same path),
/// since a swapped-in directory at that path still reads as `owned` by the
/// same stale-path marker. Re-checking presence would cost one more
/// `is_file()` call but would not close any window this removal doesn't
/// already close by re-deriving the five signals that genuinely can change.
fn remove_isolated_clone_dir(
    worktree_path: &Path,
    remover: &str,
    root_repo_slug: Option<&str>,
) -> Result<(), String> {
    if !worktree_path.join(".git").is_dir() {
        return Err(
            "refusing to remove: no `.git` directory found at this path any more -- it no \
             longer looks like the isolated clone that was examined"
                .to_string(),
        );
    }

    let branch = resolve_isolated_clone_branch(worktree_path).ok_or_else(|| {
        "refusing to remove: the clone's current branch could not be re-resolved immediately \
         before deletion (detached HEAD, or the resolution itself failed) -- it could not have \
         been examined as reclaim-eligible in the first place"
            .to_string()
    })?;
    let cleanliness = check_cleanliness(worktree_path);
    if cleanliness != Cleanliness::Clean {
        return Err(format!(
            "refusing to remove: the clone is no longer clean immediately before deletion \
             ({cleanliness:?}) -- it may have been actively worked on since it was examined"
        ));
    }
    let local_branches = resolve_isolated_clone_local_branches(worktree_path).ok_or_else(|| {
        "refusing to remove: the clone's local branch list could not be re-resolved \
         immediately before deletion"
            .to_string()
    })?;
    if local_branches != vec![branch.clone()] {
        return Err(
            "refusing to remove: the clone no longer has exactly one local branch matching its \
             current branch immediately before deletion -- a second local branch may have been \
             created since it was examined"
                .to_string(),
        );
    }
    let stash_list = resolve_isolated_clone_stash_list(worktree_path).ok_or_else(|| {
        "refusing to remove: the clone's stash list could not be re-resolved immediately \
         before deletion"
            .to_string()
    })?;
    if !stash_list.is_empty() {
        return Err(
            "refusing to remove: the clone now carries a non-empty stash list immediately \
             before deletion -- a stash may have been pushed since it was examined"
                .to_string(),
        );
    }
    // Fork issue #533: mirrors `isolated_clone_report`'s own slug-equality
    // guard (auditor B3) at removal time instead of examination time --
    // `resolve_pr_state` must never be spent against a repo slug the
    // candidate's own (untrusted) `origin` chose, since that `origin` can
    // be repointed by a same-uid actor between examination and removal.
    if root_repo_slug.is_none() {
        return Err(
            "refusing to remove: the root checkout's own repo slug could not be re-resolved \
             immediately before deletion -- gh is never queried without a trusted repo slug to \
             validate the candidate against"
                .to_string(),
        );
    }
    if derive_repo_slug(worktree_path).as_deref() != root_repo_slug {
        return Err(
            "refusing to remove: the clone's derived repo slug no longer matches the root \
             checkout's own immediately before deletion -- gh is never queried against a \
             repository this candidate's own (untrusted) origin chose"
                .to_string(),
        );
    }
    let head_ref_oid = match resolve_pr_state(worktree_path, &branch) {
        PrState::Merged {
            head_ref_oid: Some(oid),
        } => oid,
        other => {
            return Err(format!(
                "refusing to remove: the branch's PR state could not be re-resolved as \
                 merged-with-a-known-headRefOid immediately before deletion (got {:?}) -- it \
                 may have changed since the clone was examined",
                other.label()
            ));
        }
    };
    let head_sha = resolve_isolated_clone_head_sha(worktree_path).ok_or_else(|| {
        "refusing to remove: the clone's own HEAD commit SHA could not be re-resolved \
         immediately before deletion"
            .to_string()
    })?;
    if head_sha != head_ref_oid {
        return Err(
            "refusing to remove: the clone's own HEAD commit no longer equals the merged PR's \
             headRefOid immediately before deletion -- a new local commit may have been made \
             since it was examined"
                .to_string(),
        );
    }
    // Fork issue #546 hazard 1: the sixth re-derived signal -- closes the
    // TOCTOU window a pin applied during the examination-to-removal window
    // would otherwise slip through. Uses the same helper
    // `isolated_clone_report` itself calls, so both read the artifact
    // through identical logic (see [`isolated_clone_pin_state`]'s own doc
    // comment). An unreadable artifact refuses exactly like `Ok(true)`,
    // never like `Ok(false)` -- fails closed the same way every other
    // unresolvable signal above does.
    match isolated_clone_pin_state(worktree_path) {
        Ok(false) => {}
        Ok(true) => {
            return Err(
                "refusing to remove: the clone has been pinned immediately before deletion \
                 (fork issue #546 hazard 2) -- a pin applied since it was examined must never \
                 be silently overridden"
                    .to_string(),
            );
        }
        Err(e) => {
            return Err(format!(
                "refusing to remove: the clone's pin state could not be re-resolved \
                 immediately before deletion ({e}) -- an unreadable pin signal fails closed \
                 here exactly as it does in `isolated_clone_report`, never treated as unpinned"
            ));
        }
    }

    std::fs::remove_dir_all(worktree_path).map_err(|e| {
        format!("failed to remove isolated clone directory (requested by {remover}): {e}")
    })?;

    // Fork issue #546 hazard 1: the directory is gone, but the M4b
    // provenance artifact lives entirely outside it (in `state_dir()`, by
    // design) and survives unless explicitly cleared here -- otherwise a
    // later, unrelated directory created at this same path would be
    // silently vouched for by this stale evidence. Best-effort, not
    // `?`-propagated like `forget_isolated_workspace`'s equivalent removal:
    // this function is called from `run_reclaim`'s batch loop, which
    // classifies a row as "removed" on `Ok` and "kept" (with a "removal
    // failed" reason) on `Err`. By the time the marker removal is
    // attempted the directory removal above has already succeeded, so a
    // hard failure here would misreport an isolated clone that is
    // genuinely gone from disk as one that is still there and needs
    // attention -- `forget_isolated_workspace` has no such batch
    // classification to corrupt, which is why it can afford to propagate.
    // `attempt_isolated_clone_cleanup` (issue_dispatch_run.rs) is actually
    // the closer precedent for this choice, not just an alternative to
    // contrast against: it clears the same marker after the same
    // `remove_dir_all`, best-effort, for the same reason -- there, too, the
    // directory is already gone by the time the marker removal runs, so a
    // hard failure would only misreport a cleanup that already succeeded.
    let marker_path = crate::issue_dispatch_run::isolated_clone_provenance_path(worktree_path);
    let marker_cleared = match std::fs::remove_file(&marker_path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(
                path = %sanitize_path_for_terminal_display(&marker_path),
                remover = %sanitize_for_terminal_display(remover),
                error = %e,
                "failed to remove isolated clone provenance artifact after directory removal \
                 succeeded -- stale evidence may remain at this path"
            );
            false
        }
    };

    // Issue #325 / reviewer B1 / auditor F2 precedent, carried to the M4c
    // removal path: the ONLY durable trace of a confirmed removal. `remover`
    // is an unauthenticated, caller-supplied string (auditor F3) -- sanitize
    // it here exactly as `remove_worktree_dir` does, since a log file gets
    // `cat`/`tail`ed to a terminal too.
    tracing::info!(
        path = %sanitize_path_for_terminal_display(worktree_path),
        remover = %sanitize_for_terminal_display(remover),
        marker_cleared,
        "isolated clone removed"
    );
    Ok(())
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
pub fn run_reclaim(repo_dir: &Path, yes: bool, remover: &str) -> Result<ReclaimOutcome, String> {
    let reports = examine_worktrees(repo_dir)?;
    let mut removed = Vec::new();
    let mut pending = Vec::new();
    let mut kept = Vec::new();
    // Fork issue #533 fix round: resolved once per `run_reclaim` call, not
    // once per removed row -- mirrors `examine_worktrees`'s own hoisted
    // derivation above its loop (line 1667), and drops N-1 `git remote
    // get-url origin` subprocess spawns on a batch reclaim.
    let root_repo_slug = derive_repo_slug(repo_dir);

    for r in reports {
        match r.verdict.as_str() {
            "remove" => match remove_worktree_dir(repo_dir, &r.real_path, remover) {
                Ok(()) => {
                    let mut r = r;
                    r.removed_by = Some(remover.to_string());
                    removed.push(r);
                }
                Err(e) => {
                    let mut r = r;
                    r.reason = Some(format!("removal failed: {e}"));
                    kept.push(r);
                }
            },
            "ask" if yes => match remove_worktree_dir(repo_dir, &r.real_path, remover) {
                Ok(()) => {
                    let mut r = r;
                    r.removed_by = Some(remover.to_string());
                    removed.push(r);
                }
                Err(e) => {
                    let mut r = r;
                    r.reason = Some(format!("removal failed: {e}"));
                    kept.push(r);
                }
            },
            "ask" => pending.push(r),
            // Fork#325 M4c: gated on `--yes` exactly like `"ask"`, and
            // deliberately not merged into that same match arm despite the
            // identical gating -- `"isolated_clone_reclaimable"` must never
            // route through `remove_worktree_dir` (it isn't a linked
            // worktree; see that function's own doc comment), so it needs
            // its own arm calling `remove_isolated_clone_dir` regardless of
            // how the gating condition happens to line up with `"ask"`
            // today.
            VERDICT_ISOLATED_CLONE_RECLAIMABLE if yes => {
                match remove_isolated_clone_dir(&r.real_path, remover, root_repo_slug.as_deref()) {
                    Ok(()) => {
                        let mut r = r;
                        r.removed_by = Some(remover.to_string());
                        removed.push(r);
                    }
                    Err(e) => {
                        let mut r = r;
                        r.reason = Some(format!("removal failed: {e}"));
                        kept.push(r);
                    }
                }
            }
            VERDICT_ISOLATED_CLONE_RECLAIMABLE => pending.push(r),
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
            "{} item(s) reclaimable pending confirmation (kept for now):\n",
            outcome.pending.len()
        ));
        // Auditor N3 (PR #526 final round): a pending isolated clone is
        // categorically higher-stakes than a pending linked worktree --
        // deleting it destroys a full standalone repository whose objects
        // exist nowhere else, not just a linked `git worktree remove`. Tag
        // each row so the distinction is visible in the list itself, not
        // just in the trailing sentence below.
        for r in &outcome.pending {
            let path = display_path(&r.path);
            if r.verdict == VERDICT_ISOLATED_CLONE_RECLAIMABLE {
                out.push_str(&format!(
                    "  - {path} (isolated clone -- standalone repository)\n"
                ));
            } else {
                out.push_str(&format!("  - {path}\n"));
            }
        }
        out.push_str(
            "Run `dot-agent-deck worktree reclaim --yes` to remove everything in this state. \
             For a plain worktree that means removing it regardless of whether the deck \
             created it. For an isolated clone the stakes are categorically higher -- it \
             deletes that clone's entire `.git`, not just a linked worktree, and its objects \
             exist nowhere else -- but an isolated clone reaches this list at all only when \
             the deck's own attach-lock provenance artifact was found for it, so, unlike a \
             plain worktree, it is never removed \"regardless of whether the deck created \
             it\". The set is re-evaluated on that run.\n\n",
        );
    }

    if !outcome.removed.is_empty() {
        out.push_str("Removed:\n");
        for r in &outcome.removed {
            // Issue #325 / reviewer B1 / auditor F2: this is the ONLY
            // user-facing surface for `removed_by` -- there is no `worktree
            // reclaim --json`, so an operator reading this line is the
            // whole delivered feature. `removed_by` is `Some` for every
            // report actually pushed into `removed` (see the field's own
            // doc), but match rather than assume, so a future construction
            // site that leaves it `None` degrades to the plain path instead
            // of printing a misleading "(removed by )".
            match &r.removed_by {
                Some(remover) => out.push_str(&format!(
                    "  - {} (removed by {})\n",
                    display_path(&r.path),
                    sanitize_for_terminal_display(remover)
                )),
                None => out.push_str(&format!("  - {}\n", display_path(&r.path))),
            }
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
                sanitize_for_terminal_display(r.reason.as_deref().unwrap_or("no reason recorded"))
            ));
        }
    }

    out
}

/// Renders the exact line `dot-agent-deck worktree reclaim` writes to
/// stderr when enumeration itself fails (issue #232 round 4) -- same
/// rationale as [`format_list_error_for_cli`], but a separate function and a
/// separate print site, because `worktree reclaim`'s prefix differs from
/// `worktree list`'s and the two CLI sinks must stay independently fixed
/// rather than sharing one.
pub fn format_reclaim_error_for_cli(e: &str) -> String {
    format!("worktree reclaim: {}", sanitize_for_terminal_display(e))
}

/// Renders the marker-write-warning surfaced when a newly created worktree's
/// `dot-agent-deck-owner` ownership marker could not be written (issue
/// #164) -- creation itself already succeeded (see [`mark_worktree_owned`]'s
/// doc comment), so this is a non-fatal, user-facing notice, not an error.
/// Shared by two render sinks that both interpolate a local path and an I/O
/// error string into terminal-facing output -- the TUI's post-creation
/// status message (`src/ui.rs`) and the scheduled dispatch's
/// `NotifyEvent::IssueWorktreeMarkerWarning` render (`StderrNotifier` in
/// `src/scheduler.rs`) -- mirroring [`format_list_error_for_cli`]'s
/// established shape (issue #232 round 4): both sinks call this exact
/// function rather than composing `sanitize_for_terminal_display` inline, so
/// a test can call the identical production render and reverting the
/// sanitizer here breaks both sinks' tests at once, because they are now the
/// same call.
pub fn format_marker_warning(worktree: &str, error: &str) -> String {
    format!(
        "the ownership marker for {} could not be written ({}) -- a later `reclaim` of it will \
         need `--yes`",
        sanitize_for_terminal_display(worktree),
        sanitize_for_terminal_display(error),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_merged_clean_owned_removes() {
        let v = decide(
            &PrState::Merged { head_ref_oid: None },
            &Cleanliness::Clean,
            Ownership::Ours,
        );
        assert_eq!(v, Verdict::Remove);
    }

    #[test]
    fn decide_merged_clean_foreign_asks() {
        let v = decide(
            &PrState::Merged { head_ref_oid: None },
            &Cleanliness::Clean,
            Ownership::Foreign,
        );
        assert!(matches!(v, Verdict::Ask(_)));
    }

    #[test]
    fn decide_merged_dirty_keeps_regardless_of_ownership() {
        let owned = decide(
            &PrState::Merged { head_ref_oid: None },
            &Cleanliness::Dirty,
            Ownership::Ours,
        );
        let foreign = decide(
            &PrState::Merged { head_ref_oid: None },
            &Cleanliness::Dirty,
            Ownership::Foreign,
        );
        assert!(matches!(owned, Verdict::Keep(ref r) if r.contains("dirty")));
        assert!(matches!(foreign, Verdict::Keep(ref r) if r.contains("dirty")));
    }

    #[test]
    fn decide_merged_unresolvable_cleanliness_keeps_without_calling_it_dirty() {
        let v = decide(
            &PrState::Merged { head_ref_oid: None },
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
            // PRD fork#298: `owner_kind` alongside `owner` -- same reasoning
            // as the comment above, updated only so this pre-existing test
            // keeps compiling.
            owner_kind: "unknown".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: None,
            // fork#325 M4a: `WorktreeReport` grows a `kind` field -- same
            // reasoning as `owner`/`owner_kind` above, updated only so this
            // pre-existing test keeps compiling; not itself coverage for
            // the field (see `worktree_reclaim_049`-`052` for that).
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: PathBuf::from("/repo/wt-a"),
            removed_by: None,
        }];
        let json = serde_json::to_string(&WorktreeListDocument::new(reports)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // fork#325 M4c: SCHEMA_VERSION bumps 3 -> 4 for the new documented
        // `verdict` value -- same reasoning as the field-addition bumps
        // above, updated only so this pre-existing test keeps compiling.
        assert_eq!(parsed["schema_version"], 4);
        assert!(json.contains("wt-a"));
    }

    /// Build a pending-verdict report for `path`, the shape `format_reclaim_human`
    /// puts in its ask section. `cfg(unix)` because every user is: constructing
    /// a path whose bytes are not valid UTF-8 needs `OsStrExt`, and on Windows
    /// these helpers would be dead code.
    #[cfg(unix)]
    fn pending_report(path: PathBuf) -> WorktreeReport {
        WorktreeReport {
            real_path: path.clone(),
            path,
            branch: Some("feat/x".to_string()),
            clean: true,
            owned: false,
            pr_state: "merged".to_string(),
            verdict: "ask".to_string(),
            reason: Some("reclaimable".to_string()),
            owner: None,
            owner_kind: "unknown".to_string(),
            owner_reason: None,
            removed_by: None,
            kind: KIND_LINKED.to_string(),
            pinned: false,
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
    /// which `worktree/reclaim/026` cannot cheaply reach: a directory whose name
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
    /// Also pins that escaping adds no tab, so the row stays eight fields.
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
                8,
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
            crate::issue_dispatch_run::WorktreeCreation::Created {
                marker_warning: None
            },
            "the production creation path must report Created for a fresh worktree dir, got {creation:?}"
        );
        assert!(
            worktree_dir.exists(),
            "create_worktree_sync reported Created but the worktree directory is missing"
        );

        let outcome = run_reclaim(&repo, false, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
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

    /// Scenario: same fixture as `worktree_reclaim_008` above -- a
    /// deck-created, MERGED, clean worktree that a bare `reclaim` removes --
    /// but this time the caller names WHO is running the reclaim. Issue #325
    /// documents two incidents where a worktree vanished mid-use with no
    /// trace of who did it; this pins that when the deck's OWN
    /// `remove_worktree_dir` call site does the removing, the identity/context
    /// the caller supplied is recorded on the removed worktree's own report,
    /// not silently dropped on the floor.
    #[spec("worktree/reclaim/048")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_048_removal_records_remover_identity() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let branch = "feat/attribute-removal";
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

        let worktree_dir = scratch.path().join("wt-attribute-removal");
        crate::issue_dispatch_run::create_worktree_sync(
            &repo,
            &worktree_dir,
            branch,
            "test-creator",
        )
        .expect("create_worktree_sync must succeed against a real git repo");

        let remover =
            "worktree:/tmp/dot-agent-deck-attr325@fix/325-worktree-removal-attribution|test-host";
        let outcome = run_reclaim(&repo, false, remover)
            .expect("run_reclaim must succeed against a real git repo");

        assert_eq!(
            outcome.removed.len(),
            1,
            "expected exactly one removed worktree, got removed={:?} pending={:?} kept={:?}",
            outcome.removed,
            outcome.pending,
            outcome.kept
        );
        assert_eq!(
            outcome.removed[0].removed_by.as_deref(),
            Some(remover),
            "a worktree the deck's own `run_reclaim` just removed must record who ran the \
             reclaim in `removed_by` (issue #325) -- got {:?}",
            outcome.removed[0].removed_by
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
            crate::issue_dispatch_run::WorktreeCreation::Created {
                marker_warning: None
            }
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
            crate::issue_dispatch_run::WorktreeCreation::Created {
                marker_warning: None
            }
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
            crate::issue_dispatch_run::WorktreeCreation::Created {
                marker_warning: None
            }
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

    /// Scenario: issue #164 round 2. `mark_worktree_owned`'s first write to
    /// a fresh worktree is made to fail at the TEMP-write step
    /// deterministically -- no chmod, no timing, no disk-filling -- by
    /// pre-occupying the exact temp path (`<marker>.<pid>.tmp`, computed the
    /// same way the production code computes it, since both this test and
    /// the call under test run in the same process and therefore share a
    /// pid) with a directory, so `std::fs::write` to it fails with an
    /// `Is a directory` style error before any rename is attempted. The
    /// property under test is the one the whole round exists for: a failed
    /// `mark_worktree_owned` must leave **no** file at the final marker
    /// path, so `ownership_of` resolves `Foreign` rather than the stale
    /// `Ours` a pre-atomic `std::fs::write` could leave behind on a genuine
    /// ENOSPC.
    #[test]
    fn mark_worktree_owned_temp_write_failure_leaves_no_marker() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);
        let wt = scratch.path().join("wt-temp-write-fails");
        add_worktree(&repo, &wt, "feat/temp-write-fails");

        let git_dir = resolve_git_dir(&wt).expect("must resolve the worktree's git-dir");
        let tmp_path = git_dir.join(format!(
            "{OWNER_MARKER_FILENAME}.{}.tmp",
            std::process::id()
        ));
        std::fs::create_dir(&tmp_path)
            .expect("must be able to pre-occupy the deterministic temp path with a directory");

        let result = mark_worktree_owned(&wt, "creator-that-never-lands");
        assert!(
            result.is_err(),
            "a temp-write failure must be reported, not swallowed"
        );
        assert!(
            !git_dir.join(OWNER_MARKER_FILENAME).exists(),
            "no file must exist at the final marker path when the temp write never completed"
        );
        assert_eq!(
            ownership_of(&repo, &wt),
            Ownership::Foreign,
            "with no marker at the final path, ownership_of must resolve Foreign, not Ours"
        );
    }

    /// Scenario: issue #164 round 2. A worktree is marked successfully once
    /// (a real, complete marker at the final path), then a SECOND
    /// `mark_worktree_owned` call is made to fail at the temp-write step by
    /// the same deterministic pre-occupied-directory mechanism as the test
    /// above. This pins the other half of the atomicity invariant: a failed
    /// write must never disturb a marker that is already complete at the
    /// final path -- `rename` is never reached, so the original, complete
    /// content must still be exactly what is there afterwards, not
    /// corrupted, not partially overwritten, not removed.
    #[test]
    fn mark_worktree_owned_failed_remark_does_not_corrupt_existing_marker() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);
        let wt = scratch.path().join("wt-remark-fails");
        add_worktree(&repo, &wt, "feat/remark-fails");

        mark_worktree_owned(&wt, "first-creator").expect("first mark must succeed");
        assert_eq!(owner_of(&repo, &wt), Some("first-creator".to_string()));

        let git_dir = resolve_git_dir(&wt).expect("must resolve the worktree's git-dir");
        let tmp_path = git_dir.join(format!(
            "{OWNER_MARKER_FILENAME}.{}.tmp",
            std::process::id()
        ));
        std::fs::create_dir(&tmp_path)
            .expect("must be able to pre-occupy the deterministic temp path with a directory");

        let result = mark_worktree_owned(&wt, "second-creator");
        assert!(
            result.is_err(),
            "the second mark's temp-write failure must be reported, not swallowed"
        );
        assert_eq!(
            owner_of(&repo, &wt),
            Some("first-creator".to_string()),
            "a failed re-mark must leave the existing complete marker exactly as it was -- \
             never a corrupted or partial identity, and never the failed call's identity"
        );
        assert_eq!(
            ownership_of(&repo, &wt),
            Ownership::Ours,
            "the pre-existing complete marker must still resolve Ours after the failed re-mark"
        );
    }

    /// Scenario: issue #164 round 2. A worktree is marked successfully once,
    /// then the marker is replaced with a directory -- the same fixture
    /// `mark_worktree_owned_best_effort_surfaces_a_failed_write` in
    /// `issue_dispatch_run.rs` uses, reused here to drive `mark_worktree_owned`
    /// directly rather than through its wrapper -- so the SECOND call's temp
    /// write succeeds (a fresh temp filename) but the rename into place
    /// fails (`rename` onto an existing directory fails). This pins the
    /// cleanup half of the invariant: a rename failure must not leave the
    /// temp file behind as litter in the git-admin directory.
    #[test]
    fn mark_worktree_owned_rename_failure_cleans_up_temp_file() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);
        let wt = scratch.path().join("wt-rename-fails");
        add_worktree(&repo, &wt, "feat/rename-fails");

        mark_worktree_owned(&wt, "first-creator").expect("first mark must succeed");
        let git_dir = resolve_git_dir(&wt).expect("must resolve the worktree's git-dir");
        let marker_path = git_dir.join(OWNER_MARKER_FILENAME);
        std::fs::remove_file(&marker_path).expect("marker must exist after a successful mark");
        std::fs::create_dir(&marker_path)
            .expect("must be able to replace the marker file with a directory");

        let result = mark_worktree_owned(&wt, "second-creator");
        assert!(
            result.is_err(),
            "a directory occupying the marker path must make the rename fail and be reported"
        );
        assert_eq!(
            ownership_of(&repo, &wt),
            Ownership::Foreign,
            "a directory at the final marker path is not `is_file`, so this must resolve Foreign"
        );

        let leftover_tmp: Vec<_> = std::fs::read_dir(&git_dir)
            .expect("must be able to list the git-admin directory")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(OWNER_MARKER_FILENAME) && name.contains(".tmp"))
            .collect();
        assert!(
            leftover_tmp.is_empty(),
            "the temp file must be cleaned up on a rename failure, not left behind as litter, \
             found: {leftover_tmp:?}"
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

    /// Scenario: Two markers are written directly via `mark_worktree_owned`
    /// for two DIFFERENT worktrees of the SAME repo, using the owner strings
    /// the interactive `SpawnPane` path records for two live orchestrations
    /// of the SAME config type (`review`) opened in the SAME directory --
    /// `orchestration:review-orchestrator-1` and
    /// `orchestration:review-orchestrator-2`, the shape fork#192 M1.0's
    /// suggested naming produces. `owner_of` must report each worktree's own
    /// distinct owner back. This is the unit-level complement to
    /// `orchestration/worktree/004`'s (`src/ui.rs`) real-dispatch pin of the
    /// same success criterion: `mark_worktree_owned`/`owner_of` are
    /// unchanged by fork#192, so this documents that the low-level storage
    /// already supports the property the interactive path depends on.
    #[spec("worktree/reclaim/026")]
    #[test]
    fn worktree_reclaim_026_same_config_type_same_directory_records_distinct_owners() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let wt_a = scratch.path().join("repo-review-orchestrator-1");
        add_worktree(&repo, &wt_a, "feat/review-orchestrator-1");
        mark_worktree_owned(&wt_a, "orchestration:review-orchestrator-1")
            .expect("mark_worktree_owned must succeed");

        let wt_b = scratch.path().join("repo-review-orchestrator-2");
        add_worktree(&repo, &wt_b, "feat/review-orchestrator-2");
        mark_worktree_owned(&wt_b, "orchestration:review-orchestrator-2")
            .expect("mark_worktree_owned must succeed");

        let owner_a = owner_of(&repo, &wt_a);
        let owner_b = owner_of(&repo, &wt_b);

        assert_eq!(
            owner_a,
            Some("orchestration:review-orchestrator-1".to_string())
        );
        assert_eq!(
            owner_b,
            Some("orchestration:review-orchestrator-2".to_string())
        );
        assert_ne!(
            owner_a, owner_b,
            "two orchestrations of the same config type in the same directory \
             must record distinct owners in their own worktree markers"
        );
    }

    /// Scenario: `format_list_human` renders two `WorktreeReport`s -- one
    /// carrying a known owner, one carrying `None` (the shape a pre-#166
    /// legacy marker or a `Foreign` worktree produces) -- and the resulting
    /// human table must carry an OWNER column: the exact owner string for
    /// the first row, and the existing `DASH` placeholder for the second.
    /// `reason` is deliberately `Some(..)` on both reports so the table's
    /// only unexplained dash is the owner column under test, not a
    /// pre-existing dash from an absent reason.
    #[spec("worktree/reclaim/030")]
    #[test]
    fn worktree_reclaim_030_format_list_human_renders_owner_column() {
        let reports = vec![
            WorktreeReport {
                path: PathBuf::from("/repo/wt-owned"),
                branch: Some("feat/owned".to_string()),
                clean: true,
                owned: true,
                owner: Some("orchestration:owner-x".to_string()),
                owner_kind: "agent".to_string(),
                owner_reason: None,
                pr_state: "merged".to_string(),
                verdict: "remove".to_string(),
                reason: Some("ready to remove".to_string()),
                pinned: false,
                kind: KIND_LINKED.to_string(),
                real_path: PathBuf::from("/repo/wt-owned"),
                removed_by: None,
            },
            WorktreeReport {
                path: PathBuf::from("/repo/wt-legacy"),
                branch: Some("feat/legacy".to_string()),
                clean: true,
                owned: true,
                owner: None,
                owner_kind: "unknown".to_string(),
                owner_reason: None,
                pr_state: "merged".to_string(),
                verdict: "remove".to_string(),
                reason: Some("ready to remove".to_string()),
                pinned: false,
                kind: KIND_LINKED.to_string(),
                real_path: PathBuf::from("/repo/wt-legacy"),
                removed_by: None,
            },
        ];

        let out = format_list_human(&reports);
        let mut lines = out.lines();
        let header = lines
            .next()
            .expect("format_list_human must emit a header line");
        let header_fields: Vec<&str> = header.split('\t').collect();
        let owner_idx = header_fields
            .iter()
            .position(|f| *f == "OWNER")
            .unwrap_or_else(|| {
                panic!(
                    "format_list_human's header must carry an OWNER column; got header: {header:?}"
                )
            });

        let owned_row = lines
            .next()
            .expect("format_list_human must emit a row for the owned report");
        let owned_fields: Vec<&str> = owned_row.split('\t').collect();
        assert_eq!(
            owned_fields.get(owner_idx),
            Some(&"orchestration:owner-x"),
            "the OWNER column must carry the report's owner string; got row: {owned_row:?}"
        );

        let legacy_row = lines
            .next()
            .expect("format_list_human must emit a row for the legacy (owner-unknown) report");
        let legacy_fields: Vec<&str> = legacy_row.split('\t').collect();
        assert_eq!(
            legacy_fields.get(owner_idx),
            Some(&DASH),
            "the OWNER column must render the existing DASH placeholder for a report whose \
             owner is None; got row: {legacy_row:?}"
        );
    }

    /// Scenario: Five `WorktreeReport`s are constructed directly -- one
    /// whose marker names `orch-x` but whose independent `owned` resolution
    /// came back `false` (the issue #221 disagreement shape), one genuinely
    /// owned by `orch-x`, one owned by a different name entirely, one
    /// `owned: false` with that different name, and one `owned: false` with
    /// no marker owner at all (the ordinary shape of an unmarked foreign
    /// worktree). `owner_disagreements` must return exactly the first
    /// report's path -- not the two other `owned: false` rows, which would
    /// leak if the owner half of the predicate were ever dropped -- and the
    /// shared `is_mine` predicate that `run_worktree_list_cli`'s retain also
    /// calls must keep excluding the disagreeing row afterward, pinning
    /// that surfacing the disagreement never relaxes the fail-closed
    /// filter. The exact stderr text `format_disagreement_warning` produces
    /// for the disagreeing row is asserted too.
    #[spec("worktree/reclaim/036")]
    #[test]
    fn worktree_reclaim_036_owner_disagreements_finds_owned_false_with_matching_owner() {
        let disagreeing = WorktreeReport {
            path: PathBuf::from("/repo/wt-disagree"),
            branch: Some("feat/disagree".to_string()),
            clean: true,
            owned: false,
            owner: Some("orch-x".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "unknown".to_string(),
            verdict: "keep".to_string(),
            reason: None,
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: PathBuf::from("/repo/wt-disagree"),
            removed_by: None,
        };
        let genuinely_owned = WorktreeReport {
            path: PathBuf::from("/repo/wt-owned"),
            branch: Some("feat/owned".to_string()),
            clean: true,
            owned: true,
            owner: Some("orch-x".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: Some("ready to remove".to_string()),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: PathBuf::from("/repo/wt-owned"),
            removed_by: None,
        };
        let different_owner = WorktreeReport {
            path: PathBuf::from("/repo/wt-other"),
            branch: Some("feat/other".to_string()),
            clean: true,
            owned: true,
            owner: Some("orch-y".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: Some("ready to remove".to_string()),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: PathBuf::from("/repo/wt-other"),
            removed_by: None,
        };
        // P2-1: without these two, an owner-blind `owner_disagreements` --
        // e.g. `filter(|r| !r.owned)` -- would still pass this test, because
        // the only other `owned: false` row already matches "orch-x". These
        // two carry `owned: false` under a DIFFERENT owner and under no
        // owner at all, so an owner-blind implementation would wrongly pull
        // them into the disagreement list too.
        let owned_false_different_owner = WorktreeReport {
            path: PathBuf::from("/repo/wt-foreign-marked"),
            branch: Some("feat/foreign-marked".to_string()),
            clean: true,
            owned: false,
            owner: Some("orch-y".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "unknown".to_string(),
            verdict: "keep".to_string(),
            reason: None,
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: PathBuf::from("/repo/wt-foreign-marked"),
            removed_by: None,
        };
        let owned_false_no_owner = WorktreeReport {
            path: PathBuf::from("/repo/wt-unmarked"),
            branch: Some("feat/unmarked".to_string()),
            clean: true,
            owned: false,
            owner: None,
            owner_kind: "human".to_string(),
            owner_reason: None,
            pr_state: "unknown".to_string(),
            verdict: "keep".to_string(),
            reason: None,
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: PathBuf::from("/repo/wt-unmarked"),
            removed_by: None,
        };
        let reports = vec![
            disagreeing.clone(),
            genuinely_owned.clone(),
            different_owner.clone(),
            owned_false_different_owner.clone(),
            owned_false_no_owner.clone(),
        ];

        let disagreements = owner_disagreements(&reports, "orch-x");
        assert_eq!(
            disagreements,
            vec![Path::new("/repo/wt-disagree")],
            "owner_disagreements must return exactly the row whose owner matches but whose \
             owned flag is false, not the genuinely-owned row, the different-owner row, or \
             either owned:false row belonging to someone else or no one"
        );

        // P2-2 / F3: assert against the SAME `is_mine` predicate
        // `run_worktree_list_cli`'s retain calls, not a hand-written copy --
        // a copy would stay green even if production later dropped the
        // `owned` conjunct.
        let filtered: Vec<&str> = reports
            .iter()
            .filter(|r| is_mine(r, "orch-x"))
            .map(|r| r.path.to_str().expect("test path is valid UTF-8"))
            .collect();
        assert_eq!(
            filtered,
            vec!["/repo/wt-owned"],
            "is_mine must still exclude the disagreeing row -- surfacing the disagreement must \
             never relax what gets filtered"
        );

        // P3-4 / P3-6: pin the exact user-visible stderr text, including the
        // likely-cause/remedy clause, for the one path this test can prove
        // triggers it.
        assert_eq!(
            format_disagreement_warning(Path::new("/repo/wt-disagree"), "orch-x"),
            "worktree list --mine: /repo/wt-disagree is marked owned by orch-x, but the \
             ownership check disagrees -- excluding it rather than trusting either signal \
             (often a `git rev-parse` race; persisting past a re-run rules that out -- check \
             the marker and admin dir)",
            "the disagreement warning's user-visible text must name the path and owner, state \
             what disagreed, and give a likely cause with a remedy"
        );
    }

    // -------------------------------------------------------------------
    // Issue #232: `Path::display()` / `to_string_lossy()` are encoding-lossy,
    // not content-sanitizing -- a worktree path built by `path_from_bytes`
    // (byte-exact `OsStr::from_bytes` on Unix) can carry ESC, CR, LF, other
    // C0/C1 controls, and Unicode bidi/format (Cf) controls straight through
    // into every human render site below. Each is a destructive-decision
    // surface: `worktree list --mine`'s disagreement warning, the `worktree
    // list` table, and `worktree reclaim`'s three independently reachable
    // sections (pending / Removed / Kept). The fix boundary is the render
    // site itself (`.dot-agent-deck/findings-232-surfaces.md`) --
    // `path_from_bytes`, `WorktreeReport`, and `serialize_path_lossy` must
    // NOT change, so these tests assert on formatter *output* only, never on
    // an intermediate value.
    //
    // `hostile_path_component` mixes six C0/C1 controls, four Unicode
    // bidi/format controls, and printable Unicode plus a path separator in
    // one path. `assert_hostile_content_is_sanitized` pins the expected
    // escape spelling to `char::escape_default()` -- the same escaping
    // `sanitize_for_terminal` in `src/keybindings.rs` already uses for C0/C1
    // (that helper does not cover Cf; the coder's fix must extend it or add
    // a sibling that does) -- and separately requires the printable
    // Unicode/path separator content to survive verbatim, so an over-eager
    // "escape every non-ASCII char" fix fails this test exactly as a no-op
    // fix does.
    // -------------------------------------------------------------------

    /// C0/C1 controls the fix must escape: ESC, CR, LF, TAB, DEL, and one
    /// C1 control (CSI, U+009B).
    const HOSTILE_CONTROLS: [char; 6] = ['\u{1b}', '\r', '\n', '\t', '\u{7f}', '\u{9b}'];

    /// Unicode bidi/format (`Cf`) controls the fix must escape: RTL
    /// override, left-to-right isolate, zero-width space, and BOM.
    const HOSTILE_BIDI: [char; 4] = ['\u{202e}', '\u{2066}', '\u{200b}', '\u{feff}'];

    /// A path embedding every char in [`HOSTILE_CONTROLS`] and
    /// [`HOSTILE_BIDI`], plus printable Unicode and a path separator that
    /// must survive a fix unchanged.
    fn hostile_path_component() -> PathBuf {
        let mut s = String::from("/repo/wt-café-日本語");
        for c in HOSTILE_CONTROLS.iter().chain(HOSTILE_BIDI.iter()) {
            s.push(*c);
        }
        s.push_str("-tail");
        PathBuf::from(s)
    }

    /// Asserts `cell` -- content the caller has already isolated to a single
    /// untrusted field (a TAB-split table column) or bullet body (a
    /// `format_reclaim_human` line with its `"  - "` prefix and trailing
    /// newline stripped), never a raw multi-field/multi-line rendered string
    /// -- carries no raw byte of any [`HOSTILE_CONTROLS`] / [`HOSTILE_BIDI`]
    /// char, carries each one's `char::escape_default()` spelling instead,
    /// and still carries the printable Unicode / path separator content
    /// verbatim. Both directions matter equally: silently dropping a control
    /// character is exactly as wrong as leaving it raw, since either way an
    /// operator can no longer tell two hostile filenames apart.
    ///
    /// Deliberately checks a single isolated cell, never a whole rendered
    /// row/report (issue #232 rescope): `format_list_human`'s rows are
    /// TAB-separated and `format_reclaim_human`'s bullets are
    /// newline-terminated, by design -- asserting "no raw TAB/LF anywhere in
    /// the whole output" would demand the row/bullet structure itself not
    /// exist, which no fix can satisfy. Callers pin structural corruption
    /// separately, via field/line counts on the raw output, BEFORE isolating
    /// the cell this runs against.
    fn assert_hostile_content_is_sanitized(cell: &str) {
        for c in HOSTILE_CONTROLS.iter().chain(HOSTILE_BIDI.iter()) {
            assert!(
                !cell.contains(*c),
                "raw hostile char {c:?} (U+{:04X}) must not appear verbatim in the isolated \
                 cell, got {cell:?}",
                *c as u32
            );
            let escaped: String = c.escape_default().collect();
            assert!(
                cell.contains(&escaped),
                "expected the escaped spelling {escaped:?} for {c:?} (U+{:04X}) to appear in \
                 the isolated cell so two hostile filenames stay distinguishable, got \
                 {cell:?}",
                *c as u32
            );
        }
        assert!(
            cell.contains("café") && cell.contains("日本語"),
            "printable Unicode must survive a sanitizing fix unchanged, got {cell:?}"
        );
        assert!(
            cell.contains("/repo/wt-"),
            "the path separator and ordinary path content must survive unchanged, got {cell:?}"
        );
    }

    /// Scenario: `format_disagreement_warning` is given a path whose final
    /// component embeds ESC/CR/LF/TAB/DEL/a C1 control and four Unicode
    /// bidi/format controls, mixed with printable Unicode and an ordinary
    /// path separator. The rendered warning -- shown on `worktree list
    /// --mine`'s stderr before the operator inspects marker/admin state --
    /// must carry no raw control byte and must carry each one's escaped
    /// spelling instead, while the printable content survives unchanged.
    #[test]
    fn format_disagreement_warning_escapes_hostile_path_content() {
        let path = hostile_path_component();
        let out = format_disagreement_warning(&path, "test-owner");
        assert_hostile_content_is_sanitized(&out);
    }

    /// Scenario: `format_list_human` renders one report whose `path` embeds
    /// the same hostile mix. The PATH column of the `worktree list` table --
    /// read to decide what to reclaim -- must carry no raw control byte and
    /// must carry each one's escaped spelling instead; because the table is
    /// TAB-separated, an unescaped raw TAB in the path would also corrupt
    /// the column count, so this test additionally pins the row back to
    /// exactly eight TAB-separated fields.
    #[test]
    fn format_list_human_escapes_hostile_path_content_in_path_column() {
        let path = hostile_path_component();
        let reports = vec![WorktreeReport {
            path: path.clone(),
            branch: Some("feat/hostile".to_string()),
            clean: true,
            owned: true,
            owner: Some("test-owner".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: Some("ready to remove".to_string()),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: None,
        }];

        let out = format_list_human(&reports);
        assert_eq!(
            out.lines().count(),
            2,
            "a raw newline embedded in the path must not corrupt the line count (header + \
             one data row), got: {out:?}"
        );
        let row = out
            .lines()
            .nth(1)
            .expect("format_list_human must emit a data row after the header");
        let fields: Vec<&str> = row.split('\t').collect();
        assert_eq!(
            fields.len(),
            8,
            "a raw TAB embedded in the path must not corrupt the TAB-separated column count, \
             got row: {row:?}"
        );
        assert_hostile_content_is_sanitized(fields[0]);
    }

    /// Scenario: `format_reclaim_human` renders a pending-confirmation
    /// report (no `--yes` yet) whose path embeds the hostile mix. This is
    /// the highest-risk site
    /// (`.dot-agent-deck/findings-232-surfaces.md`): it names the exact
    /// directories a following `--yes` run may remove, so a forged path
    /// here is a direct path to removing the wrong directory.
    #[test]
    fn format_reclaim_human_escapes_hostile_path_in_pending_section() {
        let path = hostile_path_component();
        let report = WorktreeReport {
            path: path.clone(),
            branch: Some("feat/hostile".to_string()),
            clean: true,
            owned: false,
            owner: None,
            owner_kind: "human".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "ask".to_string(),
            reason: Some(
                "reclaimable: PR is merged and the tree is clean, but the deck cannot prove \
                 it created this worktree"
                    .to_string(),
            ),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: None,
        };
        let outcome = ReclaimOutcome {
            removed: Vec::new(),
            pending: vec![report],
            kept: Vec::new(),
        };

        let out = format_reclaim_human(&outcome);
        assert_eq!(
            out.lines().count(),
            5,
            "a raw newline embedded in the path must not corrupt the line count (header, \
             bullet, run-command line, blank line, `Removed: none`), got: {out:?}"
        );
        let bullet = out
            .lines()
            .nth(1)
            .expect("format_reclaim_human must emit a pending bullet as the second line");
        let cell = bullet
            .strip_prefix("  - ")
            .expect("pending bullet must start with the literal '  - ' prefix");
        assert_hostile_content_is_sanitized(cell);
    }

    /// Scenario: `format_reclaim_human` renders a `Removed:` entry -- the
    /// record of a destructive action that already happened -- whose path
    /// embeds the hostile mix. A control sequence here could forge or
    /// obscure what was actually removed.
    #[test]
    fn format_reclaim_human_escapes_hostile_path_in_removed_section() {
        let path = hostile_path_component();
        let report = WorktreeReport {
            path: path.clone(),
            branch: Some("feat/hostile".to_string()),
            clean: true,
            owned: true,
            owner: Some("test-owner".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: None,
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: None,
        };
        let outcome = ReclaimOutcome {
            removed: vec![report],
            pending: Vec::new(),
            kept: Vec::new(),
        };

        let out = format_reclaim_human(&outcome);
        assert_eq!(
            out.lines().count(),
            2,
            "a raw newline embedded in the path must not corrupt the line count (`Removed:` \
             header, bullet), got: {out:?}"
        );
        let bullet = out
            .lines()
            .nth(1)
            .expect("format_reclaim_human must emit a removed bullet as the second line");
        let cell = bullet
            .strip_prefix("  - ")
            .expect("removed bullet must start with the literal '  - ' prefix");
        assert_hostile_content_is_sanitized(cell);
    }

    /// Scenario: `format_reclaim_human` renders a `Removed:` entry whose
    /// `removed_by` (issue #325's whole reason to exist -- reviewer B1 /
    /// auditor F2/F3) embeds the hostile mix. Unlike `owner`, `removed_by`
    /// is never routed through `sanitize_marker_creator` on the way in --
    /// it is an unauthenticated, caller-supplied identity string -- so it
    /// must be sanitized at THIS render site rather than assumed safe
    /// because `path` already is.
    #[test]
    fn format_reclaim_human_escapes_hostile_content_in_removed_by() {
        let path = PathBuf::from("/repo/wt-normal");
        let remover = hostile_string_component();
        let report = WorktreeReport {
            path: path.clone(),
            branch: Some("feat/hostile-remover".to_string()),
            clean: true,
            owned: true,
            owner: Some("test-owner".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: None,
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: Some(remover),
        };
        let outcome = ReclaimOutcome {
            removed: vec![report],
            pending: Vec::new(),
            kept: Vec::new(),
        };

        let out = format_reclaim_human(&outcome);
        assert_eq!(
            out.lines().count(),
            2,
            "a raw newline embedded in removed_by must not corrupt the line count (`Removed:` \
             header, bullet), got: {out:?}"
        );
        let bullet = out
            .lines()
            .nth(1)
            .expect("format_reclaim_human must emit a removed bullet as the second line");
        let cell = bullet
            .strip_prefix("  - \"/repo/wt-normal\" (removed by ")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or_else(|| {
                panic!("removed bullet must be `<path> (removed by <remover>)`, got {bullet:?}")
            });
        assert_hostile_content_is_sanitized(cell);
    }

    /// Scenario: `format_reclaim_human` renders a `Kept:` entry -- read to
    /// decide whether further cleanup/reclaim is safe, especially for a
    /// pending/dirty reason -- whose path embeds the hostile mix.
    #[test]
    fn format_reclaim_human_escapes_hostile_path_in_kept_section() {
        let path = hostile_path_component();
        let reason = "dirty: uncommitted or untracked changes are present that were never \
                       part of the merged PR";
        let report = WorktreeReport {
            path: path.clone(),
            branch: Some("feat/hostile".to_string()),
            clean: false,
            owned: true,
            owner: Some("test-owner".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "keep".to_string(),
            reason: Some(reason.to_string()),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: None,
        };
        let outcome = ReclaimOutcome {
            removed: Vec::new(),
            pending: Vec::new(),
            kept: vec![report],
        };

        let out = format_reclaim_human(&outcome);
        assert_eq!(
            out.lines().count(),
            3,
            "a raw newline embedded in the path must not corrupt the line count \
             (`Removed: none`, `Kept:` header, bullet), got: {out:?}"
        );
        let bullet = out
            .lines()
            .nth(2)
            .expect("format_reclaim_human must emit a kept bullet as the third line");
        let without_prefix = bullet
            .strip_prefix("  - ")
            .expect("kept bullet must start with the literal '  - ' prefix");
        let cell = without_prefix
            .strip_suffix(&format!(" ({reason})"))
            .expect("kept bullet must end with the literal ' ({reason})' suffix");
        assert_hostile_content_is_sanitized(cell);
    }

    /// Scenario: `WorktreeListDocument` serializes a report whose path
    /// embeds the same hostile mix used by the human-formatter tests above.
    /// Unlike those, this pins the OPPOSITE requirement: `worktree list
    /// --json`'s `path` value is a machine contract
    /// (`.dot-agent-deck/findings-232-surfaces.md`) and must NOT gain
    /// terminal escaping -- only the existing lossy non-UTF-8 replacement --
    /// so a script parsing this field keeps working. This test is expected
    /// to be GREEN already, before any #232 fix lands: it exists so the fix
    /// cannot "helpfully" sanitize the JSON path too.
    #[test]
    fn worktree_list_json_path_field_preserves_hostile_content_unescaped() {
        let path = hostile_path_component();
        let reports = vec![WorktreeReport {
            path: path.clone(),
            branch: Some("feat/hostile".to_string()),
            clean: true,
            owned: true,
            owner: Some("test-owner".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: None,
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path.clone(),
            removed_by: None,
        }];

        let json = serde_json::to_string(&WorktreeListDocument::new(reports)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let json_path = parsed["worktrees"][0]["path"]
            .as_str()
            .expect("path field must be a JSON string");

        assert_eq!(
            json_path,
            path.to_string_lossy(),
            "the JSON path value must remain exactly `to_string_lossy()`'s output -- no \
             terminal escaping applied -- so existing scripts parsing this field keep working"
        );
    }

    // -------------------------------------------------------------------
    // Issue #232 part 2: `format_list_human`'s row carries three more
    // untrusted cells beyond PATH -- OWNER, BRANCH, REASON. As decided for
    // this round, `format_reclaim_human` does not render `owner` at all --
    // its pending/Removed/Kept sections interpolate only `path` and
    // `reason`, both already fully covered by part 1's path tests -- so no
    // new cell tests are added there.
    //
    // OWNER's two hostile-char groups do NOT behave the same, and that
    // asymmetry is why OWNER gets no full-mix cell test of its own here.
    // The six `HOSTILE_CONTROLS` chars are all Unicode category Cc
    // (control: U+0000-U+001F, U+007F-U+009F, which covers DEL and the C1
    // control U+009B used here), and `owner_of` -> `read_marker_owner` is
    // the ONLY call site that ever sets `WorktreeReport.owner` (verified by
    // reading every construction site of the field) -- it unconditionally
    // maps the parsed value through `sanitize_marker_creator`, whose
    // `char::is_control()` filter strips every Cc char before the value is
    // ever assigned. A full-mix OWNER test would therefore assert
    // sanitization against six chars that provably cannot reach this cell
    // today; per this task's own instruction that is noise, not coverage,
    // and is skipped rather than written. The four `HOSTILE_BIDI` (Cf)
    // chars are NOT filtered by `sanitize_marker_creator` -- that is the
    // documented gap `read_marker_owner`'s own doc comment already calls
    // out -- so OWNER's real, reachable hostile content is Cf/bidi only,
    // and is covered by the dedicated Cf-gap test below instead of being
    // duplicated in a second, near-identical OWNER test here.
    //
    // BRANCH has no analogous in-code filter: `wt.branch` is
    // `String::from_utf8_lossy` straight off `git worktree list
    // --porcelain`'s `branch refs/heads/<name>` field with nothing applied
    // to it afterward. Verified empirically (`git check-ref-format
    // --branch`) that git's own ref-name validation rejects all five
    // plain-ASCII controls used here (ESC, CR, LF, TAB, DEL) on the normal
    // branch-creation path, but ACCEPTS the C1 control U+009B (its UTF-8
    // encoding's bytes fall outside the `<0x20 or ==0x7F` range that check
    // enforces) and all four Cf/bidi chars unmodified. So on the ordinary
    // `git branch` / `worktree add` path, only C1 + Cf/bidi are reachable
    // -- but nothing in this crate re-validates a ref name it reads back,
    // so a `.git` admin-dir a process could write directly (the exact
    // threat model `read_marker_owner`'s own doc comment already accepts
    // for the marker file) is not excluded either. The BRANCH test below
    // still uses the full mix: `format_list_human` is the render boundary
    // regardless of provenance, and nothing in the crate proves the
    // ASCII-control chars can never reach it.
    //
    // REASON has no filter at all in either direction: `check_cleanliness`
    // and `resolve_pr_state` interpolate raw, `String::from_utf8_lossy`
    // subprocess stderr (`git status --porcelain`, `gh`) straight into the
    // `Unresolvable` reason string with nothing stripped, so it is the
    // least-filtered of the three cells and the full mix is used
    // unreservedly.
    // -------------------------------------------------------------------

    /// The same hostile-content mix as [`hostile_path_component`], as a
    /// bare `String` for cells (OWNER, BRANCH, REASON) that are
    /// `Option<String>` rather than `PathBuf`.
    fn hostile_string_component() -> String {
        hostile_path_component().to_string_lossy().into_owned()
    }

    /// Asserts `output` carries no raw byte of any [`HOSTILE_BIDI`] char and
    /// carries each one's `char::escape_default()` spelling instead. Unlike
    /// [`assert_hostile_content_is_sanitized`], this does NOT also require
    /// [`HOSTILE_CONTROLS`]' escaped spellings: the Cf-gap test below feeds
    /// an input that has already had every `HOSTILE_CONTROLS` char stripped
    /// (not escaped) by `sanitize_marker_creator`, so requiring their
    /// escaped spelling too would fail on chars that were correctly removed
    /// upstream rather than escaped at the render site.
    ///
    /// Deliberately kept as a second, narrower helper rather than folded
    /// into [`assert_hostile_content_is_sanitized`] once that helper moved
    /// to operating on an isolated cell (issue #232 rescope): the two no
    /// longer differ over TAB/newline structure -- OWNER's Cf-gap test could
    /// pass an isolated OWNER cell to either helper without hitting a
    /// separator conflict. They differ over which *character set* is
    /// reachable in that cell. `sanitize_marker_creator` already stripped
    /// every `HOSTILE_CONTROLS` char before this cell's content was ever
    /// assigned, so those chars' raw bytes are absent for a reason that has
    /// nothing to do with the render-site fix under test; asserting their
    /// escaped spellings too would demand a spelling for content that was
    /// correctly removed, not escaped. Only [`HOSTILE_BIDI`] is genuinely
    /// reachable here, so only it is asserted.
    fn assert_bidi_content_is_sanitized(output: &str) {
        for c in HOSTILE_BIDI.iter() {
            assert!(
                !output.contains(*c),
                "raw hostile char {c:?} (U+{:04X}) must not appear verbatim in rendered \
                 output, got {output:?}",
                *c as u32
            );
            let escaped: String = c.escape_default().collect();
            assert!(
                output.contains(&escaped),
                "expected the escaped spelling {escaped:?} for {c:?} (U+{:04X}) to appear in \
                 rendered output, got {output:?}",
                *c as u32
            );
        }
    }

    /// Scenario: `format_list_human` renders one report whose `branch`
    /// embeds the same hostile mix used for PATH. The BRANCH column must
    /// carry no raw control byte and must carry each one's escaped spelling
    /// instead, while printable Unicode survives; a raw TAB here would also
    /// forge a column boundary and shift every later cell, so this pins the
    /// row back to eight TAB-separated fields too.
    #[test]
    fn format_list_human_escapes_hostile_content_in_branch_column() {
        let branch = hostile_string_component();
        let path = PathBuf::from("/repo/normal");
        let reports = vec![WorktreeReport {
            path: path.clone(),
            branch: Some(branch),
            clean: true,
            owned: true,
            owner: Some("test-owner".to_string()),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: Some("ready to remove".to_string()),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: None,
        }];

        let out = format_list_human(&reports);
        assert_eq!(
            out.lines().count(),
            2,
            "a raw newline embedded in the branch must not corrupt the line count (header + \
             one data row), got: {out:?}"
        );
        let row = out
            .lines()
            .nth(1)
            .expect("format_list_human must emit a data row after the header");
        let fields: Vec<&str> = row.split('\t').collect();
        assert_eq!(
            fields.len(),
            8,
            "a raw TAB embedded in the branch must not corrupt the TAB-separated column \
             count, got row: {row:?}"
        );
        assert_hostile_content_is_sanitized(fields[1]);
    }

    /// Scenario: `format_list_human` renders one report whose `reason`
    /// embeds the same hostile mix. REASON is built from raw, unsanitized
    /// subprocess stderr (`check_cleanliness` / `resolve_pr_state`
    /// interpolate `git`/`gh` diagnostics verbatim into the `Unresolvable`
    /// reason string), so it is the least-filtered of the three cells. The
    /// REASON column must carry no raw control byte and must carry each
    /// one's escaped spelling instead, while printable Unicode survives; a
    /// raw TAB here is the same row-structure attack as the other cells,
    /// pinned the same way.
    #[test]
    fn format_list_human_escapes_hostile_content_in_reason_column() {
        let reason = hostile_string_component();
        let path = PathBuf::from("/repo/normal");
        let reports = vec![WorktreeReport {
            path: path.clone(),
            branch: Some("feat/normal".to_string()),
            clean: true,
            owned: false,
            owner: None,
            owner_kind: "unknown".to_string(),
            owner_reason: None,
            pr_state: "unresolvable".to_string(),
            verdict: "keep".to_string(),
            reason: Some(reason),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: None,
        }];

        let out = format_list_human(&reports);
        assert_eq!(
            out.lines().count(),
            2,
            "a raw newline embedded in the reason must not corrupt the line count (header + \
             one data row), got: {out:?}"
        );
        let row = out
            .lines()
            .nth(1)
            .expect("format_list_human must emit a data row after the header");
        let fields: Vec<&str> = row.split('\t').collect();
        assert_eq!(
            fields.len(),
            8,
            "a raw TAB embedded in the reason must not corrupt the TAB-separated column \
             count, got row: {row:?}"
        );
        assert_hostile_content_is_sanitized(fields[7]);
    }

    /// Scenario: `sanitize_marker_creator` filters Unicode category Cc but
    /// leaves Cf/bidi controls (RTL override, LTR isolate, zero-width
    /// space, BOM) untouched, so a `created-by:` marker value carrying
    /// U+202E reaches `WorktreeReport.owner` exactly as written -- this is
    /// the gap `read_marker_owner`'s own doc comment already names. This
    /// test feeds `format_list_human` the exact value
    /// `sanitize_marker_creator` hands back for such a marker (sanity-
    /// checked below to confirm the Cf/bidi chars really do survive it) and
    /// pins the fix at the render site rather than the sanitizer: the OWNER
    /// column must still not show the raw Cf/bidi char, even though
    /// `sanitize_marker_creator` itself is untouched by this fix.
    #[test]
    fn format_list_human_escapes_cf_bidi_survivors_of_marker_sanitizer_in_owner_column() {
        let raw_creator = "orchestration:demo-café\u{202e}\u{2066}\u{200b}\u{feff}-tail";
        let owner = sanitize_marker_creator(raw_creator);
        for c in HOSTILE_BIDI.iter() {
            assert!(
                owner.contains(*c),
                "sanity check: sanitize_marker_creator must not strip Cf/bidi char {c:?} \
                 (U+{:04X}), or this test no longer pins the gap it is named for; got \
                 {owner:?}",
                *c as u32
            );
        }
        for c in HOSTILE_CONTROLS.iter() {
            assert!(
                !owner.contains(*c),
                "sanity check: sanitize_marker_creator is expected to strip Cc char {c:?} \
                 (U+{:04X}) -- if it no longer does, this test's premise (only Cf survives) \
                 is stale; got {owner:?}",
                *c as u32
            );
        }

        let path = PathBuf::from("/repo/normal");
        let reports = vec![WorktreeReport {
            path: path.clone(),
            branch: Some("feat/normal".to_string()),
            clean: true,
            owned: true,
            owner: Some(owner),
            owner_kind: "agent".to_string(),
            owner_reason: None,
            pr_state: "merged".to_string(),
            verdict: "remove".to_string(),
            reason: Some("ready to remove".to_string()),
            pinned: false,
            kind: KIND_LINKED.to_string(),
            real_path: path,
            removed_by: None,
        }];

        let out = format_list_human(&reports);
        assert_bidi_content_is_sanitized(&out);
        assert!(
            out.contains("café"),
            "printable Unicode must survive a sanitizing fix unchanged, got {out:?}"
        );
    }

    /// Scenario: issue #232 round 2, gap 1. `format_disagreement_warning`
    /// sanitized `path` but interpolated `owner` raw, even though `owner`
    /// reaches it through `sanitize_marker_creator`, which strips Unicode
    /// category `Cc` but deliberately preserves `Cf`/bidi controls -- the
    /// same survivors [`format_list_human_escapes_cf_bidi_survivors_of_marker_sanitizer_in_owner_column`]
    /// pins for the OWNER table column. A marker value like
    /// `orchestration:prod\u{202e}...` must not reach `worktree list --mine`'s
    /// stderr disagreement warning with a raw bidi control still in it.
    #[test]
    fn format_disagreement_warning_escapes_cf_bidi_survivors_of_marker_sanitizer_in_owner() {
        let raw_creator = "orchestration:prod-café\u{202e}\u{2066}\u{200b}\u{feff}-tail";
        let owner = sanitize_marker_creator(raw_creator);
        for c in HOSTILE_BIDI.iter() {
            assert!(
                owner.contains(*c),
                "sanity check: sanitize_marker_creator must not strip Cf/bidi char {c:?} \
                 (U+{:04X}), or this test no longer pins the gap it is named for; got \
                 {owner:?}",
                *c as u32
            );
        }

        let path = hostile_path_component();
        let out = format_disagreement_warning(&path, &owner);

        assert_hostile_content_is_sanitized(&out);
        assert_bidi_content_is_sanitized(&out);
        assert!(
            out.contains("café"),
            "printable Unicode in the owner must survive a sanitizing fix unchanged, got \
             {out:?}"
        );
    }

    // -------------------------------------------------------------------
    // Issue #232 round 3/4: `list_linked_worktrees`' `Err` path embeds RAW
    // `git worktree list` stderr (this function, a few lines above), which
    // `examine_worktrees` and `run_reclaim` both propagate unchanged via
    // `?`. `src/main.rs`'s `worktree list` and `worktree reclaim` CLI
    // wrappers are the only two places that ever print that `Err` to a
    // terminal, and each now prints via [`format_list_error_for_cli`] /
    // [`format_reclaim_error_for_cli`] instead of composing
    // `sanitize_for_terminal_display` inline (round 4: round 3's tests
    // called `sanitize_for_terminal_display` directly, so they stayed green
    // even if the CLI wrapper's call to it were reverted -- they proved the
    // sanitizer works, never that the CLI used it). These two functions ARE
    // the CLI print sinks now, so calling them from a test calls the same
    // code `main.rs`'s `eprintln!` calls; reverting the sanitizer inside
    // either one fails both the CLI sink and the test in the same edit.
    //
    // A real `git worktree list` failure whose stderr echoes attacker
    // content is reproduced by standing a fake `git` in front of
    // `list_linked_worktrees`' `Command::new("git")` call (the same
    // `PathEnvGuard`/`GH_PATH_ENV_LOCK` mocking `worktree_reclaim_021`
    // already uses for `gh`) that fails `worktree list` with a stderr
    // carrying the same hostile mix [`hostile_path_component`] uses.
    // -------------------------------------------------------------------

    /// A synthetic `git worktree list` failure message embedding every char
    /// in [`HOSTILE_CONTROLS`] and [`HOSTILE_BIDI`] between printable text,
    /// standing in for a real `git worktree list` stderr that echoes back
    /// hostile repository/worktree path content.
    fn hostile_git_worktree_list_stderr() -> String {
        let mut s =
            String::from("fatal: worktree administrative files are corrupt at path-café-日本語");
        for c in HOSTILE_CONTROLS.iter().chain(HOSTILE_BIDI.iter()) {
            s.push(*c);
        }
        s.push_str("-tail");
        s
    }

    /// Writes a fake `git` executable into `bindir` that fails ONLY a
    /// `worktree list ...` invocation, with `stderr` set to
    /// `hostile_stderr` and exit code 1; any other argv exits 1 with no
    /// output, since `examine_worktrees`/`run_reclaim` return via `?` on
    /// the very first `git` call and never reach a second one on this
    /// path.
    #[cfg(unix)]
    fn write_fake_git_failing_worktree_list(bindir: &Path, hostile_stderr: &str) {
        use std::os::unix::fs::PermissionsExt;

        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = worktree ] && [ \"$2\" = list ]; then\n  printf '%s' '{hostile_stderr}' >&2\n  exit 1\nfi\nexit 1\n"
        );
        let git_path = bindir.join("git");
        std::fs::write(&git_path, script).expect("must write fake git script");
        std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o755))
            .expect("must make fake git script executable");
    }

    /// Scenario: `worktree list`'s enumeration fails because `git worktree
    /// list` itself fails, with a stderr that echoes hostile
    /// repository/worktree path content. `examine_worktrees` must still
    /// propagate that stderr RAW in its `Err` -- sanitization is not this
    /// library function's job -- and the error is then rendered through
    /// [`format_list_error_for_cli`], the SAME function
    /// `run_worktree_list_cli` in `src/main.rs` calls to build its
    /// `eprintln!` line, not a re-derivation of what it does. The rendered
    /// line must carry no raw hostile char and the escaped spelling of each
    /// instead, while the surrounding printable text survives unchanged --
    /// so reverting the `sanitize_for_terminal_display` call inside
    /// `format_list_error_for_cli` fails this test AND `main.rs`'s actual
    /// stderr output at once, because they are now one call.
    #[test]
    #[cfg(unix)]
    fn examine_worktrees_raw_error_is_sanitized_by_the_list_cli_sink() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let hostile_stderr = hostile_git_worktree_list_stderr();
        write_fake_git_failing_worktree_list(&bindir, &hostile_stderr);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let repo_dir = scratch.path().join("wherever");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let err = examine_worktrees(&repo_dir)
            .expect_err("the mocked git worktree list failure must propagate as Err");
        assert!(
            err.contains(&hostile_stderr),
            "examine_worktrees must propagate the raw git stderr unchanged -- sanitizing is the \
             CLI print sink's job, not the library's; got {err:?}"
        );

        // The same call `run_worktree_list_cli` makes before its `eprintln!`
        // -- not `sanitize_for_terminal_display` called directly, or this
        // test would stay green even if the CLI wrapper stopped calling it.
        let sanitized = format_list_error_for_cli(&err);
        assert!(
            sanitized.starts_with("worktree list: "),
            "expected the CLI sink's own prefix, got {sanitized:?}"
        );
        for c in HOSTILE_CONTROLS.iter().chain(HOSTILE_BIDI.iter()) {
            assert!(
                !sanitized.contains(*c),
                "raw hostile char {c:?} (U+{:04X}) must not appear verbatim in what \
                 `worktree list`'s sink prints, got {sanitized:?}",
                *c as u32
            );
            let escaped: String = c.escape_default().collect();
            assert!(
                sanitized.contains(&escaped),
                "expected the escaped spelling {escaped:?} for {c:?} (U+{:04X}) in what \
                 `worktree list`'s sink prints, got {sanitized:?}",
                *c as u32
            );
        }
        assert!(
            sanitized.contains("path-café-日本語"),
            "printable text surrounding the hostile content must survive unchanged, got \
             {sanitized:?}"
        );
    }

    /// Scenario: `worktree reclaim`'s enumeration fails the same way --
    /// `run_reclaim` calls `examine_worktrees` first and propagates its
    /// `Err` via `?` unchanged, reaching a SEPARATE render function,
    /// [`format_reclaim_error_for_cli`] (`"worktree reclaim: ..."` vs.
    /// `worktree list`'s `"worktree list: ..."`), which is the SAME
    /// function `run_worktree_reclaim_cli` in `src/main.rs` calls to build
    /// its `eprintln!` line. This pins that the `reclaim` sink is
    /// independently fixed, not merely inherited from `list`'s fix, and
    /// that this test exercises the actual sink rather than re-deriving its
    /// composition.
    #[test]
    #[cfg(unix)]
    fn run_reclaim_raw_error_is_sanitized_by_the_reclaim_cli_sink() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let hostile_stderr = hostile_git_worktree_list_stderr();
        write_fake_git_failing_worktree_list(&bindir, &hostile_stderr);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let repo_dir = scratch.path().join("wherever");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let err = match run_reclaim(&repo_dir, false, "test-remover") {
            Err(e) => e,
            // `ReclaimOutcome` derives no `Debug`, so this cannot print what it
            // got -- the arm is unreachable anyway, since the mocked `git`
            // fails every invocation.
            Ok(_) => panic!("the mocked git worktree list failure must propagate as Err"),
        };
        assert!(
            err.contains(&hostile_stderr),
            "run_reclaim must propagate the raw git stderr unchanged -- sanitizing is the CLI \
             print sink's job, not the library's; got {err:?}"
        );

        // The same call `run_worktree_reclaim_cli` makes before its
        // `eprintln!` -- not `sanitize_for_terminal_display` called
        // directly, or this test would stay green even if the CLI wrapper
        // stopped calling it.
        let sanitized = format_reclaim_error_for_cli(&err);
        assert!(
            sanitized.starts_with("worktree reclaim: "),
            "expected the CLI sink's own prefix, got {sanitized:?}"
        );
        for c in HOSTILE_CONTROLS.iter().chain(HOSTILE_BIDI.iter()) {
            assert!(
                !sanitized.contains(*c),
                "raw hostile char {c:?} (U+{:04X}) must not appear verbatim in what `worktree \
                 reclaim`'s sink prints, got {sanitized:?}",
                *c as u32
            );
            let escaped: String = c.escape_default().collect();
            assert!(
                sanitized.contains(&escaped),
                "expected the escaped spelling {escaped:?} for {c:?} (U+{:04X}) in what \
                 `worktree reclaim`'s sink prints, got {sanitized:?}",
                *c as u32
            );
        }
        assert!(
            sanitized.contains("path-café-日本語"),
            "printable text surrounding the hostile content must survive unchanged, got \
             {sanitized:?}"
        );
    }

    /// Scenario: issue #164. `format_marker_warning` interpolates a local
    /// worktree path and a raw I/O error string -- both are local,
    /// uncontrolled content that can still carry hostile terminal-control
    /// characters, exactly the concern issue #232 exists for. Mirrors
    /// `format_disagreement_warning`'s own sanitization test above: calls
    /// the exact function both the TUI's post-creation status message
    /// (`src/ui.rs`) and the scheduled dispatch's `StderrNotifier` render
    /// (`src/scheduler.rs`, `NotifyEvent::IssueWorktreeMarkerWarning`) call,
    /// not a re-derivation of what it does -- reverting the
    /// `sanitize_for_terminal_display` calls inside `format_marker_warning`
    /// would fail this test AND both production sinks at once, because they
    /// are now the same call.
    #[test]
    fn format_marker_warning_sanitizes_path_and_error() {
        let hostile_path = hostile_path_component();
        let hostile_error = hostile_git_worktree_list_stderr();

        let out = format_marker_warning(&hostile_path.to_string_lossy(), &hostile_error);

        assert_hostile_content_is_sanitized(&out);
        assert!(
            out.contains("café") && out.contains("日本語"),
            "printable Unicode surrounding the hostile content must survive unchanged, got \
             {out:?}"
        );
        assert!(
            out.contains("could not be written"),
            "expected the warning's own wording to survive alongside the sanitized content, \
             got {out:?}"
        );
    }

    // -------------------------------------------------------------------
    // Fork #325 M4a: isolated-clone discovery (RED — no production code
    // exists yet). An isolated clone (`provision_isolated_clone_sync`) is a
    // fully independent `git clone` sibling of the repo, not a linked
    // worktree -- `list_linked_worktrees`/`examine_worktrees` structurally
    // cannot see it today, since it never appears in `git worktree list`'s
    // output run from `repo_dir`. These tests pin the discovery surface
    // this milestone slice adds: `examine_worktrees` must also enumerate
    // deck-owned isolated clones sitting as siblings of `repo_dir`,
    // distinguish them from an ordinary linked worktree in the report, and
    // never assign one an automatic-removal verdict.
    // -------------------------------------------------------------------

    /// A genuine, independent `git clone` of `repo` at `clone_dir` -- the
    /// same on-disk shape `provision_isolated_clone_sync` produces
    /// (fork#325 M3): a real `.git` DIRECTORY, never a linked worktree's
    /// `.git` FILE redirect. `origin` is repointed at the same GitHub URL
    /// `init_repo_with_origin` gives `repo`, mirroring
    /// `point_isolated_clone_origin`'s real behavior when the source has an
    /// origin (fork#325 M3) -- a plain `git clone`'s default `origin`
    /// (repo's own local filesystem path) would make `derive_repo_slug`
    /// fail to parse a GitHub owner/repo, and every PR-state-dependent
    /// assertion below would then be vacuous (`Unresolvable` already never
    /// removes, so it would prove nothing about the isolated-clone-specific
    /// gate under test).
    fn clone_repo_with_github_origin(repo: &Path, clone_dir: &Path) {
        let out = std::process::Command::new("git")
            .args([
                "clone",
                "--quiet",
                "--",
                &repo.display().to_string(),
                &clone_dir.display().to_string(),
            ])
            .output()
            .unwrap_or_else(|e| panic!("git clone failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = std::process::Command::new("git")
            .current_dir(clone_dir)
            .args([
                "remote",
                "set-url",
                "origin",
                "https://github.com/test-org/test-repo.git",
            ])
            .output()
            .unwrap_or_else(|e| panic!("git remote set-url failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git remote set-url failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Repoint `dir`'s `origin` remote at a URL `parse_github_owner_repo`
    /// cannot parse (reviewer F6): `derive_repo_slug` then returns `None`
    /// and `resolve_pr_state` fails closed to `PrState::Unresolvable`
    /// without ever spawning the real, ambient `gh` -- for a test whose
    /// assertions don't depend on the PR-state verdict, this is cheaper and
    /// more hermetic than `write_merged_gh_stub`'s `PATH`-scoped stub.
    fn set_non_github_origin(dir: &Path) {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args([
                "remote",
                "set-url",
                "origin",
                "https://example.invalid/test-org/test-repo.git",
            ])
            .output()
            .unwrap_or_else(|e| panic!("git remote set-url failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git remote set-url failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A `gh` stub answering `gh pr list --head <branch> ...` with a single
    /// canned MERGED reply for `branch`, matching `worktree_reclaim_008`'s
    /// own script shape.
    #[cfg(unix)]
    fn write_merged_gh_stub(bindir: &Path, branch: &str) {
        use std::os::unix::fs::PermissionsExt;
        let gh_script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then\n    printf '%s\\n' \
             '[{{\"state\":\"MERGED\",\"headRefName\":\"{branch}\",\"headRepositoryOwner\":{{\"login\":\"test-org\"}}}}]'\n    \
             exit 0\nfi\nexit 1\n"
        );
        std::fs::create_dir_all(bindir).unwrap();
        let gh_path = bindir.join("gh");
        std::fs::write(&gh_path, gh_script).unwrap();
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Same shape as [`write_merged_gh_stub`], additionally carrying
    /// `headRefOid` -- the PR branch's own head commit SHA (fork#325 M4c
    /// round 3 / PR #526 reviewer B2) -- in the `gh pr list --json` reply.
    /// This is the ONLY field the redesigned eligibility check compares a
    /// clone's own `git rev-parse HEAD` against: `gh pr list --json`
    /// exposes `headRefOid` directly as a flat field, so no second `gh api
    /// graphql` subcommand (the round-2 `mergeCommit.tree.oid` machinery
    /// this replaces) is needed at all. The script branches on `$1`/`$2`
    /// only, exactly as [`write_merged_gh_stub`] already did for `pr`/
    /// `list` -- it does not otherwise validate the invocation's arguments,
    /// matching that stub's own level of fidelity.
    #[cfg(unix)]
    fn write_merged_gh_stub_with_head_ref_oid(bindir: &Path, branch: &str, head_ref_oid: &str) {
        use std::os::unix::fs::PermissionsExt;
        let gh_script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then\n    printf '%s\\n' \
             '[{{\"state\":\"MERGED\",\"headRefName\":\"{branch}\",\"headRepositoryOwner\":{{\"login\":\"test-org\"}},\"headRefOid\":\"{head_ref_oid}\"}}]'\n    \
             exit 0\nfi\nexit 1\n"
        );
        std::fs::create_dir_all(bindir).unwrap();
        let gh_path = bindir.join("gh");
        std::fs::write(&gh_path, gh_script).unwrap();
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Same shape as [`write_merged_gh_stub_with_head_ref_oid`], additionally
    /// taking `owner` explicitly instead of hardcoding `"test-org"` -- issue
    /// #533's fixture needs a merged-PR reply whose `headRepositoryOwner`
    /// matches a DIFFERENT (attacker) slug's owner, not the root checkout's
    /// own, to prove `resolve_pr_state` genuinely resolves the reply against
    /// whichever slug it was asked about.
    ///
    /// Also `touch`es [`GH_INVOKED_MARKER_NAME`] in `bindir` as its very
    /// first action, unconditionally -- before the `$1`/`$2` branch, so it
    /// fires on ANY invocation of this stub, not only a `pr list` call
    /// (auditor's ordering-coverage suggestion, PR #686 fix round). A caller
    /// that asserts the marker is absent after a refusal is asserting that
    /// `gh` was never spawned at all, not merely that the call ultimately
    /// failed.
    #[cfg(unix)]
    fn write_merged_gh_stub_with_owner_and_head_ref_oid(
        bindir: &Path,
        branch: &str,
        owner: &str,
        head_ref_oid: &str,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let marker_path = bindir.join(GH_INVOKED_MARKER_NAME);
        let gh_script = format!(
            "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then\n    printf '%s\\n' \
             '[{{\"state\":\"MERGED\",\"headRefName\":\"{branch}\",\"headRepositoryOwner\":{{\"login\":\"{owner}\"}},\"headRefOid\":\"{head_ref_oid}\"}}]'\n    \
             exit 0\nfi\nexit 1\n",
            marker_path.display()
        );
        std::fs::create_dir_all(bindir).unwrap();
        let gh_path = bindir.join("gh");
        std::fs::write(&gh_path, gh_script).unwrap();
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Filename [`write_merged_gh_stub_with_owner_and_head_ref_oid`]'s stub
    /// `touch`es on every invocation, inside the same `bindir` the stub
    /// itself lives in -- lets a caller assert the marker's absence to prove
    /// `gh` was never spawned, not merely that a `gh` call ultimately failed.
    #[cfg(unix)]
    const GH_INVOKED_MARKER_NAME: &str = "gh-was-invoked.marker";

    /// `git rev-parse HEAD` run inside `dir`, trimmed -- a fixture-only
    /// helper for tests that need to assert on an isolated clone's actual
    /// HEAD SHA (fork#325 M4c), mirroring `worktree_reclaim_054`'s own
    /// inline `rev-parse HEAD` call.
    fn git_rev_parse_head(dir: &Path) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse HEAD must spawn");
        assert!(
            out.status.success(),
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Scenario: A real, deck-owned isolated clone (a genuine `git clone`
    /// sibling of the repo, its own `.git` DIRECTORY, marked owned via the
    /// same `mark_worktree_owned` call `provision_isolated_clone_sync` uses)
    /// sits next to `repo`. `examine_worktrees(repo)` must report it --
    /// today it is silently absent, since `list_linked_worktrees` (`git
    /// worktree list`) structurally cannot see a directory that is not a
    /// linked worktree of `repo` at all.
    #[spec("worktree/reclaim/049")]
    #[test]
    fn worktree_reclaim_049_isolated_clone_is_discovered() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-issue-999");
        clone_repo_with_github_origin(&repo, &clone_dir);
        // Reviewer F6: this test's assertions don't depend on the PR-state
        // verdict, so a non-GitHub origin keeps `resolve_pr_state` from
        // spawning the real, ambient `gh`.
        set_non_github_origin(&clone_dir);
        mark_worktree_owned(&clone_dir, "issue-dispatch:isolated-999#999")
            .expect("mark_worktree_owned must succeed against a real independent clone");

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        assert!(
            reports.iter().any(|r| r.real_path == clone_dir),
            "a deck-owned isolated clone sitting as a sibling of the repo must be discoverable \
             by examine_worktrees, got reports for paths: {:?}",
            reports.iter().map(|r| &r.real_path).collect::<Vec<_>>()
        );
    }

    /// Scenario: three sibling directories that must each NEVER be reported
    /// as a discovered isolated clone: a plain directory with no `.git` at
    /// all; an unrelated, independently-`git init`ed repo (its own `.git`
    /// DIRECTORY, structurally identical to a real isolated clone) with no
    /// marker; and a genuine `git clone` OF `repo` that carries no
    /// ownership marker at all (a clone the deck did not make, or whose
    /// marker write failed) -- alongside one genuine deck-owned isolated
    /// clone that MUST appear. Discovery must never mistake "has a `.git`
    /// directory" for "is a deck-owned isolated clone".
    #[spec("worktree/reclaim/050")]
    #[test]
    fn worktree_reclaim_050_only_owned_isolated_clones_are_discovered() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        // (a) plain directory, no `.git` at all.
        let plain_dir = scratch.path().join("repo-plain-sibling");
        std::fs::create_dir_all(&plain_dir).unwrap();

        // (b) an unrelated, independent repo -- its own `.git` DIRECTORY,
        // structurally identical to a real isolated clone, but not derived
        // from `repo` and carrying no marker.
        let unrelated_repo = scratch.path().join("repo-unrelated-sibling");
        init_repo_with_origin(&unrelated_repo);

        // (c) a genuine clone OF `repo`, but never marked owned -- the
        // shape a clone the deck didn't create (or whose marker write
        // failed) would have.
        let unmarked_clone = scratch.path().join("repo-isolated-unmarked");
        clone_repo_with_github_origin(&repo, &unmarked_clone);

        // The one genuine positive: a deck-owned isolated clone.
        let owned_clone = scratch.path().join("repo-isolated-owned");
        clone_repo_with_github_origin(&repo, &owned_clone);
        // Reviewer F6: `owned_clone` is the only candidate here that reaches
        // `resolve_pr_state` (the others are filtered by discovery before
        // that point), and this test's assertions don't depend on its
        // PR-state verdict -- a non-GitHub origin keeps `resolve_pr_state`
        // from spawning the real, ambient `gh`.
        set_non_github_origin(&owned_clone);
        mark_worktree_owned(&owned_clone, "issue-dispatch:isolated-owned#1000")
            .expect("mark_worktree_owned must succeed");

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let all_paths = || reports.iter().map(|r| &r.real_path).collect::<Vec<_>>();

        assert!(
            !reports.iter().any(|r| r.real_path == plain_dir),
            "a plain sibling directory with no .git at all must never be reported, got \
             paths: {:?}",
            all_paths()
        );
        assert!(
            !reports.iter().any(|r| r.real_path == unrelated_repo),
            "an unrelated independent repo (its own .git directory, no marker) must never be \
             reported, got paths: {:?}",
            all_paths()
        );
        assert!(
            !reports.iter().any(|r| r.real_path == unmarked_clone),
            "a genuine clone of the repo carrying NO ownership marker must never be reported, \
             got paths: {:?}",
            all_paths()
        );
        assert!(
            reports.iter().any(|r| r.real_path == owned_clone),
            "the one genuine deck-owned isolated clone must still be reported alongside the \
             exclusions above, got paths: {:?}",
            all_paths()
        );
    }

    /// Scenario: one ordinary linked worktree and one deck-owned isolated
    /// clone are examined together. A consumer of the report (a human
    /// table, or the `--json` document) must be able to tell them apart --
    /// this is the whole reason M4a exists as discovery, not just presence:
    /// treating an isolated clone as an ordinary worktree in the report
    /// would make `worktree reclaim` reason about it exactly like a linked
    /// worktree, which is the safety property M4a exists to prevent.
    /// Checked through the `--json` document as `serde_json::Value`
    /// (mirroring `worktree_reclaim_037`'s own precedent for a
    /// not-yet-existing field) rather than a `WorktreeReport` struct field,
    /// so this test's own RED signature is an assertion failure, not a
    /// build break: today both rows are missing whatever field
    /// distinguishes them, since neither the isolated clone (see
    /// `worktree_reclaim_049`) nor a `kind` discriminator exists yet.
    ///
    /// Design decision (flagged for coder, see the work-done report): the
    /// discriminator is a new `kind` field on `WorktreeReport`/the `--json`
    /// document, `"linked"` for an ordinary worktree and `"isolated_clone"`
    /// for a discovered isolated clone.
    #[spec("worktree/reclaim/051")]
    #[test]
    fn worktree_reclaim_051_isolated_clone_report_is_distinguishable_from_linked_worktree() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);
        // Reviewer F6: `linked_wt` below shares `repo`'s remotes (a linked
        // worktree, not a clone), so `resolve_pr_state` would otherwise
        // resolve `repo`'s GitHub-shaped origin and spawn the real, ambient
        // `gh` for it too -- this test's assertions don't depend on either
        // row's PR-state verdict.
        set_non_github_origin(&repo);

        let linked_wt = scratch.path().join("repo-linked");
        add_worktree(&repo, &linked_wt, "feat/linked");
        mark_worktree_owned(&linked_wt, "orch-linked").expect("mark_worktree_owned must succeed");

        let clone_dir = scratch.path().join("repo-isolated-distinguish");
        clone_repo_with_github_origin(&repo, &clone_dir);
        set_non_github_origin(&clone_dir);
        mark_worktree_owned(&clone_dir, "issue-dispatch:isolated-distinguish#1001")
            .expect("mark_worktree_owned must succeed");

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let json = serde_json::to_string(&WorktreeListDocument::new(reports)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let worktrees = parsed["worktrees"]
            .as_array()
            .expect("worktrees must be a JSON array");

        let find_kind = |path: &Path| -> Option<String> {
            let path_str = path.to_string_lossy().into_owned();
            worktrees
                .iter()
                .find(|w| w["path"].as_str() == Some(path_str.as_str()))
                .map(|w| w["kind"].to_string())
        };

        let linked_kind = find_kind(&linked_wt);
        let clone_kind = find_kind(&clone_dir);

        assert_ne!(
            linked_kind, clone_kind,
            "an isolated clone's report must carry a `kind` distinguishable from an ordinary \
             linked worktree's report -- got linked={linked_kind:?} isolated_clone={clone_kind:?}"
        );
        // The RED assertion: today no discriminator field exists at all, and
        // the isolated clone isn't even reported (see worktree_reclaim_049),
        // so `clone_kind` is `None` -- this fails until both land.
        assert_eq!(
            clone_kind.as_deref(),
            Some("\"isolated_clone\""),
            "the isolated clone's `kind` field must read exactly \"isolated_clone\" in the \
             --json document, got {clone_kind:?}"
        );
    }

    /// Scenario: a deck-owned isolated clone that is CLEAN and whose branch
    /// has a MERGED PR -- the exact combination that makes an ordinary
    /// linked worktree `Verdict::Remove` -- must never be automatically
    /// removed by `worktree reclaim`, with or without `--yes`, when the
    /// `gh` stub's merged-PR response carries no `headRefOid` field at all.
    /// CORRECTED (fork issue #546): this does NOT pin a guarantee that
    /// isolated clones are never auto-reclaimed -- `worktree/reclaim/062`
    /// proves the opposite once `headRefOid` genuinely matches the clone's
    /// own HEAD. This test only ever exercises the "head ref unresolvable"
    /// (`None`) branch, because [`write_merged_gh_stub`] omits the field
    /// entirely; it passes for that specific fixture shape, not because of
    /// any stronger guarantee the codebase actually makes (see
    /// `worktree/reclaim/072`, which names this same gap for the
    /// present-but-mismatched case).
    #[spec("worktree/reclaim/052")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_052_isolated_clone_never_gets_an_automatic_removal_verdict() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-never-removed");
        clone_repo_with_github_origin(&repo, &clone_dir);
        mark_worktree_owned(&clone_dir, "issue-dispatch:isolated-never-removed#1002")
            .expect("mark_worktree_owned must succeed");

        // `git clone` checks out the source's HEAD branch ("main") with no
        // local changes -- clean by construction. The gh stub answers
        // MERGED for that exact branch, so every gate an ordinary linked
        // worktree would need to reach `Verdict::Remove` is satisfied here.
        let bindir = scratch.path().join("bin");
        write_merged_gh_stub(&bindir, "main");
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );
        assert_ne!(
            clone_report.verdict.as_str(),
            "remove",
            "a clean, PR-merged isolated clone must never get an automatic-removal verdict, \
             got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );

        // Neither a bare reclaim nor `--yes` may actually delete it.
        let bare = run_reclaim(&repo, false, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !bare.removed.iter().any(|r| r.real_path == clone_dir),
            "a bare `worktree reclaim` must never remove an isolated clone, got removed: {:?}",
            bare.removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the isolated clone directory must still exist on disk after a bare reclaim"
        );

        let confirmed = run_reclaim(&repo, true, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !confirmed.removed.iter().any(|r| r.real_path == clone_dir),
            "`worktree reclaim --yes` must never remove an isolated clone either -- M4a defers \
             automatic isolated-clone reclaim to a documented follow-up, got removed: {:?}",
            confirmed
                .removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the isolated clone directory must still exist on disk after `reclaim --yes`"
        );
    }

    /// Scenario: fork issue #325 M4 final round (reviewer F13 / auditor
    /// A1/B1) -- the gap coder flagged after replacing
    /// `candidate_shares_history_with` with `candidate_has_attach_lock`.
    /// None of `worktree_reclaim_049`-`052` build their fixture through the
    /// real `provision_isolated_clone_sync`, so none of them ever produce a
    /// genuine attach-lock artifact and the new `owned: true` path had zero
    /// coverage. This test calls the real production provisioner directly
    /// against a real source repo, then asserts `examine_worktrees` reports
    /// the resulting clone with `owned: true` and the exact `owner`/
    /// `owner_kind` the provisioner's own marker write recorded --
    /// `candidate_has_attach_lock` recognizing a clone the deck's own code
    /// actually created is the security fix this round exists for.
    #[spec("worktree/reclaim/053")]
    #[test]
    fn worktree_reclaim_053_isolated_clone_with_real_attach_lock_reports_owned_true() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-real-lock");
        let creator = "issue-dispatch:real-lock#1003";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "real-lock-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );
        // Reviewer F6: this test's assertions don't depend on the PR-state
        // verdict, so a non-GitHub origin (set only AFTER provisioning has
        // already written the attach lock and the marker) keeps
        // `resolve_pr_state` from spawning the real, ambient `gh`.
        set_non_github_origin(&clone_dir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "a clone provisioned through the real provision_isolated_clone_sync must be \
             discovered by examine_worktrees",
        );

        assert!(
            clone_report.owned,
            "a genuine isolated clone carrying the real attach-lock artifact \
             provision_isolated_clone_sync writes must report owned: true -- if this is ever \
             false, candidate_has_attach_lock no longer recognizes the deck's own real \
             provisioning output, and this security fix has zero coverage; got {clone_report:?}"
        );
        assert_eq!(
            clone_report.owner.as_deref(),
            Some(creator),
            "owner must read back the exact creator identity the real marker write recorded, \
             got {:?}",
            clone_report.owner
        );
        assert_eq!(
            clone_report.owner_kind, "agent",
            "a marker-backed isolated clone's owner_kind must be \"agent\", got {:?}",
            clone_report.owner_kind
        );
        assert_eq!(clone_report.kind, KIND_ISOLATED_CLONE);
        assert!(
            clone_report.owner_reason.is_none(),
            "an agent-owned row carries no owner_reason, got {:?}",
            clone_report.owner_reason
        );
    }

    /// Scenario: fork issue #325 M4 final round, auditor A1/B1's exact
    /// forgery -- a sibling directory whose `.git` is a plain directory
    /// (no real git objects, refs, or config), a `HEAD` file containing a
    /// REAL but unrelated-to-this-candidate commit SHA as plain text (the
    /// shape the earlier, since-removed `candidate_shares_history_with`
    /// check could not distinguish from a genuine clone), and a
    /// hand-planted `dot-agent-deck-owner` marker claiming an arbitrary
    /// identity -- but no attach-lock artifact, since a same-uid attacker
    /// able only to plant a sibling directory cannot write into this
    /// process's own `state_dir()`. `examine_worktrees` must still discover it
    /// (discovery stays purely structural) but must report `owned: false`
    /// / `owner_kind: "unknown"` with `ISOLATED_CLONE_NO_ATTACH_LOCK_REASON`,
    /// and it must never satisfy `is_mine` for any identity -- including
    /// the exact one the forged, unread marker claims.
    #[spec("worktree/reclaim/054")]
    #[test]
    fn worktree_reclaim_054_forged_isolated_clone_without_attach_lock_reports_owned_false() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        // A real commit SHA the repo genuinely has -- unrelated to the
        // forged candidate below, which never actually holds it as a git
        // object of its own.
        let head_sha_out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse HEAD must spawn");
        assert!(
            head_sha_out.status.success(),
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&head_sha_out.stderr)
        );
        let head_sha = String::from_utf8_lossy(&head_sha_out.stdout)
            .trim()
            .to_string();

        let forged = scratch.path().join("repo-isolated-forged");
        let forged_git_dir = forged.join(".git");
        std::fs::create_dir_all(&forged_git_dir).unwrap();
        // No real git objects/refs/config anywhere -- a 2-file forgery
        // (HEAD + the owner marker), exactly auditor B1's reproduction:
        // no git invocation at all was needed to build this.
        std::fs::write(forged_git_dir.join("HEAD"), format!("{head_sha}\n")).unwrap();
        std::fs::write(
            forged_git_dir.join(OWNER_MARKER_FILENAME),
            "deck\ncreated-by: attacker-forged-identity\n",
        )
        .unwrap();

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let forged_report = reports.iter().find(|r| r.real_path == forged).expect(
            "a structurally-present sibling (.git directory + owner marker) must still be \
             discovered even with no attach lock at all -- discovery stays purely structural, \
             only `owned` is affected",
        );

        assert!(
            !forged_report.owned,
            "a forged marker with no matching attach-lock artifact must never report \
             owned: true, got {forged_report:?}"
        );
        assert_eq!(
            forged_report.owner_kind, "unknown",
            "got owner_kind {:?}",
            forged_report.owner_kind
        );
        assert_eq!(
            forged_report.owner, None,
            "the forged marker's content must never be read/trusted absent a matching attach \
             lock, got owner {:?}",
            forged_report.owner
        );
        assert_eq!(
            forged_report.owner_reason.as_deref(),
            Some(ISOLATED_CLONE_NO_ATTACH_LOCK_REASON),
            "got owner_reason {:?}",
            forged_report.owner_reason
        );
        assert!(
            !is_mine(forged_report, "attacker-forged-identity"),
            "the forged row must never satisfy --mine, even for the exact identity its own \
             (untrusted, unread) marker claims"
        );
        assert!(
            !is_mine(forged_report, "test-remover"),
            "the forged row must never satisfy --mine for any other identity either"
        );
    }

    /// Scenario: auditor B4 -- `examine_worktrees` invoked from a
    /// SUBDIRECTORY of the root checkout (not the checkout root itself)
    /// must still discover a sibling isolated clone. `discover_isolated_clones`
    /// derives its scan anchor via `resolve_common_dir`, never assumes
    /// `repo_dir` already IS the root checkout -- reviewer/auditor manually
    /// re-verified this in the fix round, but nothing in the test suite
    /// pinned it, so a regression back to anchoring on `repo_dir`'s own
    /// parent (auditor A2's original defect) would silently miss every real
    /// isolated clone whenever `examine_worktrees` is invoked from a
    /// subdirectory, with every other test in this file still passing.
    /// Reuses `worktree_reclaim_049`'s exact fixture shape, only calling
    /// `examine_worktrees` against a real subdirectory of `repo` instead of
    /// `repo` itself, and compares the two resulting reports directly.
    #[spec("worktree/reclaim/055")]
    #[test]
    fn worktree_reclaim_055_subdirectory_anchor_still_discovers_sibling_isolated_clone() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let subdir = repo.join("src");
        std::fs::create_dir_all(&subdir).unwrap();

        let clone_dir = scratch.path().join("repo-isolated-subdir-anchor");
        clone_repo_with_github_origin(&repo, &clone_dir);
        // Reviewer F6: this test's assertions don't depend on the PR-state
        // verdict, so a non-GitHub origin keeps `resolve_pr_state` from
        // spawning the real, ambient `gh`.
        set_non_github_origin(&clone_dir);
        mark_worktree_owned(&clone_dir, "issue-dispatch:subdir-anchor#1004")
            .expect("mark_worktree_owned must succeed against a real independent clone");

        let from_root =
            examine_worktrees(&repo).expect("examine_worktrees must succeed from the root");
        let from_subdir = examine_worktrees(&subdir)
            .expect("examine_worktrees must succeed from a subdirectory of the root");

        // Matched via `paths_refer_to_same_dir`, not raw `==` (this module's
        // own `discover_isolated_clones` doc comment, "M4a Windows/macOS
        // fix"): the subdirectory case's anchor comes from
        // `resolve_common_dir`'s git-resolved spelling, while `clone_dir`
        // here is built via plain `Path::join` on the caller's own
        // unresolved `scratch.path()` -- on macOS `/var/folders/...` is
        // itself a symlink to `/private/var/folders/...`, so the two
        // spellings differ even though they name the same directory.
        // Reproduced directly in CI: this test failed on `build-macos`
        // with a raw `==` comparison, passed on `build`/`build-windows`,
        // exactly the platform split that doc comment predicts.
        let find = |reports: &[WorktreeReport]| -> Option<WorktreeReport> {
            reports
                .iter()
                .find(|r| paths_refer_to_same_dir(&r.real_path, &clone_dir))
                .cloned()
        };
        let root_entry = find(&from_root).expect(
            "sanity: the isolated clone must be discovered when examine_worktrees is called \
             against the root checkout itself -- see worktree_reclaim_049",
        );
        let subdir_entry = find(&from_subdir).expect(
            "the isolated clone must be discovered identically when examine_worktrees is \
             called from a SUBDIRECTORY of the root checkout -- this is exactly the case \
             auditor A2's fix was for; a regression back to anchoring on repo_dir's own parent \
             would silently miss this row from a subdirectory while every root-anchored test \
             in this file kept passing",
        );

        assert_eq!(
            root_entry.kind, subdir_entry.kind,
            "kind must be identical whether examine_worktrees is invoked from the root or a \
             subdirectory"
        );
        assert_eq!(
            root_entry.owned, subdir_entry.owned,
            "owned must be identical whether examine_worktrees is invoked from the root or a \
             subdirectory"
        );
        assert_eq!(
            root_entry.owner, subdir_entry.owner,
            "owner must be identical whether examine_worktrees is invoked from the root or a \
             subdirectory"
        );
        assert_eq!(
            root_entry.owner_kind, subdir_entry.owner_kind,
            "owner_kind must be identical whether examine_worktrees is invoked from the root \
             or a subdirectory"
        );
    }

    /// Scenario: fork issue #325 M4b (reviewer P1) -- the attach-lock
    /// namespace `candidate_has_attach_lock` checks is the SAME one
    /// `create_worktree_sync` writes into for an ordinary linked worktree,
    /// not isolated-clone-specific. A linked worktree is created through
    /// the real production path (writing a genuine attach-lock artifact),
    /// removed normally via `git worktree remove`, and a same-uid attacker
    /// then plants a forged `.git` directory plus a forged ownership
    /// marker at that exact now-vacant path -- no attach lock forged at
    /// all, since it inherits the genuine, already-written one. This
    /// asserts the CORRECT, not-yet-shipped behavior (the forged occupant
    /// must never report `owned: true`), so it is RED today: the inherited
    /// lock currently makes it pass.
    #[spec("worktree/reclaim/056")]
    #[test]
    fn worktree_reclaim_056_forged_directory_inherits_a_vacated_linked_worktree_lock() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let worktree_dir = scratch.path().join("wt-later-forged");
        let creation = crate::issue_dispatch_run::create_worktree_sync(
            &repo,
            &worktree_dir,
            "feat/later-forged",
            "test-creator",
        )
        .expect("create_worktree_sync must succeed against a real git repo");
        assert!(
            matches!(
                creation,
                crate::issue_dispatch_run::WorktreeCreation::Created {
                    marker_warning: None
                }
            ),
            "sanity: the linked worktree must be created cleanly, got {creation:?}"
        );

        // Remove it normally -- the attach-lock artifact `create_worktree_sync`
        // wrote is never cleaned up on this path, which is the whole
        // residual under test.
        let remove = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "remove", "--force", "--"])
            .arg(&worktree_dir)
            .output()
            .expect("git worktree remove must spawn");
        assert!(
            remove.status.success(),
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&remove.stderr)
        );
        assert!(
            !worktree_dir.exists(),
            "sanity: the worktree directory must actually be gone after removal"
        );

        // Same-uid attacker plants a forged `.git` dir + forged ownership
        // marker at the exact now-vacant path.
        let forged_git_dir = worktree_dir.join(".git");
        std::fs::create_dir_all(&forged_git_dir).unwrap();
        std::fs::write(
            forged_git_dir.join(OWNER_MARKER_FILENAME),
            "deck\ncreated-by: attacker-forged-identity\n",
        )
        .unwrap();

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let forged_report = reports.iter().find(|r| r.real_path == worktree_dir).expect(
            "a structurally-present forged directory (.git dir + owner marker) must still \
                 be discovered -- discovery stays purely structural",
        );

        assert!(
            !forged_report.owned,
            "a forged directory occupying a path whose attach-lock artifact was written for a \
             PRIOR, now-removed linked worktree must never report owned: true merely by \
             inheriting that lock -- fork#325 M4b (reviewer P1); got {forged_report:?}"
        );
        assert!(
            !is_mine(forged_report, "attacker-forged-identity"),
            "the forged row must never satisfy --mine for the identity its own forged marker \
             claims"
        );
    }

    /// Scenario: fork issue #325 M4b (auditor C1) -- `provision_isolated_clone_sync`
    /// acquires the attach lock (writing the artifact unconditionally)
    /// BEFORE checking whether `clone_dir` already exists. A same-uid
    /// attacker who pre-plants a forged `.git` dir plus a forged ownership
    /// marker at the fully deterministic dispatch path gets the deck's own
    /// provisioner to write a genuine attach-lock artifact vouching for it,
    /// even though the dispatch itself then visibly refuses it and nothing
    /// ever actually attaches into the planted directory.
    ///
    /// PRD fork#544 M3: the refusal this asserts is now
    /// `Rejected(ResumeRejection::Stranger)` rather than the old flat
    /// `AlreadyClaimed` -- the forged in-tree `OWNER_MARKER_FILENAME` this
    /// fixture writes carries no weight with M3's eligibility check at all
    /// (it never reads that file), and the OUT-of-tree M4b evidence
    /// `isolated_clone_provenance_path` looks for was never written for
    /// this canonical path, so this is exactly the "stranger directory"
    /// case `orchestration/workspace/006` also covers -- the security
    /// property this test protects (never silently attach, never report
    /// `owned: true`) is unchanged by which specific refusal variant names
    /// it.
    #[spec("worktree/reclaim/057")]
    #[test]
    fn worktree_reclaim_057_pre_planted_directory_survives_already_claimed_as_owned() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let target_dir = scratch.path().join("repo-isolated-preplanted");
        let forged_git_dir = target_dir.join(".git");
        std::fs::create_dir_all(&forged_git_dir).unwrap();
        std::fs::write(
            forged_git_dir.join(OWNER_MARKER_FILENAME),
            "deck\ncreated-by: attacker-forged-identity\n",
        )
        .unwrap();

        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &target_dir,
            "real-branch",
            "legit-dispatcher",
        )
        .expect(
            "provision_isolated_clone_sync must not hard-error against a pre-existing directory",
        );
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Rejected(
                    crate::issue_dispatch_run::ResumeRejection::Stranger
                )
            ),
            "sanity: a pre-existing directory at the deterministic dispatch path, with no \
             out-of-tree M4b evidence, must be reported as Rejected(Stranger), not silently \
             cloned into, got {outcome:?}"
        );

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let forged_report = reports.iter().find(|r| r.real_path == target_dir).expect(
            "the pre-planted directory must still be discovered -- discovery stays purely \
             structural",
        );

        assert!(
            !forged_report.owned,
            "a directory that merely happened to occupy a path BEFORE a legitimate dispatch \
             attempt acquired the lock for it (bounded by a Stranger refusal) must never report \
             owned: true -- fork#325 M4b (auditor C1); got {forged_report:?}"
        );
        assert!(
            !is_mine(forged_report, "attacker-forged-identity"),
            "the forged row must never satisfy --mine for the identity its own forged marker \
             claims"
        );
        assert!(
            target_dir.exists(),
            "sanity: a Stranger refusal must never delete or modify the pre-existing directory"
        );
    }

    /// Scenario: fork issue #325 M4b (reviewer F2) -- `discover_isolated_clones`
    /// anchors its scan on the INVOKING repo's own common `.git` dir
    /// (`resolve_common_dir(repo_dir)`), so calling `examine_worktrees`
    /// with `repo_dir` set to an isolated clone's OWN directory resolves
    /// `common_dir` to the CLONE's own `.git`, not the root checkout's --
    /// where every attach-lock artifact this milestone relies on actually
    /// lives. The Nth-concurrent-orchestration gate this milestone exists
    /// to serve runs, by definition, FROM inside an isolated clone, so a
    /// sibling clone the SAME identity owns currently reports owned: false
    /// from there. Asserts the CORRECT, not-yet-shipped behavior -- RED
    /// today.
    #[spec("worktree/reclaim/058")]
    #[test]
    fn worktree_reclaim_058_mine_discoverable_from_inside_a_sibling_isolated_clone() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let creator = "issue-dispatch:from-inside-clone#1006";
        let clone_a = scratch.path().join("repo-isolated-a");
        crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo, &clone_a, "branch-a", creator,
        )
        .expect("provision_isolated_clone_sync must succeed for clone A");
        set_non_github_origin(&clone_a);

        let clone_b = scratch.path().join("repo-isolated-b");
        crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo, &clone_b, "branch-b", creator,
        )
        .expect("provision_isolated_clone_sync must succeed for clone B");
        set_non_github_origin(&clone_b);

        // Invoked with repo_dir set to clone A's OWN directory -- exactly
        // what the Nth-concurrent-orchestration gate does when it runs
        // `worktree list --mine` from inside its own isolated clone.
        let reports_from_a = examine_worktrees(&clone_a)
            .expect("examine_worktrees must succeed from inside clone A");

        let clone_b_report = reports_from_a
            .iter()
            .find(|r| r.real_path == clone_b)
            .expect(
                "sibling clone B must be discovered when examine_worktrees runs from inside \
                 clone A -- both are siblings of the same root checkout",
            );

        assert!(
            is_mine(clone_b_report, creator),
            "a sibling isolated clone owned by the SAME identity must satisfy --mine when \
             examine_worktrees runs from inside another isolated clone, not just from the root \
             checkout -- fork#325 M4b (reviewer F2); got {clone_b_report:?}"
        );
    }

    /// Scenario: fork issue #325 M4b (auditor B4, automating auditor A1
    /// item 2's original manual finding) -- `discover_isolated_clones`
    /// checks `entry.file_type().is_symlink()` (which does NOT traverse
    /// the symlink) before ever treating a sibling as a candidate, so a
    /// symlinked sibling pointing at a genuine, owned isolated clone is
    /// skipped rather than followed and reported under the symlink's own
    /// path. No automated test pinned this before -- it was only
    /// re-verified by hand.
    #[spec("worktree/reclaim/059")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_059_symlinked_sibling_is_skipped_not_followed() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let real_clone = scratch.path().join("repo-isolated-real");
        clone_repo_with_github_origin(&repo, &real_clone);
        set_non_github_origin(&real_clone);
        mark_worktree_owned(&real_clone, "issue-dispatch:symlink-real#1007")
            .expect("mark_worktree_owned must succeed");

        let symlinked_sibling = scratch.path().join("repo-isolated-symlink");
        std::os::unix::fs::symlink(&real_clone, &symlinked_sibling)
            .expect("create symlink sibling pointing at the real clone");

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");

        assert!(
            reports.iter().any(|r| r.real_path == real_clone),
            "sanity: the real, owned isolated clone must be discovered directly"
        );
        assert!(
            !reports.iter().any(|r| r.real_path == symlinked_sibling),
            "a symlinked sibling must be SKIPPED, never followed and reported under the \
             symlink's own path -- fork#325 M4b / auditor A1 item 2 (previously only manually \
             verified), got paths: {:?}",
            reports.iter().map(|r| &r.real_path).collect::<Vec<_>>()
        );
        assert_eq!(
            reports.iter().filter(|r| r.real_path == real_clone).count(),
            1,
            "the real clone must be reported exactly once, not once per alias"
        );
    }

    /// Scenario: fork issue #325 M4b (auditor B4, automating auditor A3's
    /// original manual PoC) -- `git_in_untrusted_dir` passes `-c
    /// core.fsmonitor=` on every git invocation against a discovery
    /// candidate, closing the code-execution vector auditor's own lab
    /// demonstrated by setting `core.fsmonitor` to an arbitrary program and
    /// observing it run during `worktree list`. This plants the same
    /// payload (`git -C <candidate> config core.fsmonitor <payload>`)
    /// against a real clone sibling and proves the payload's sentinel file
    /// is never created when `examine_worktrees` runs.
    #[spec("worktree/reclaim/060")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_060_forged_core_fsmonitor_payload_never_executes() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let hostile_clone = scratch.path().join("repo-isolated-hostile");
        clone_repo_with_github_origin(&repo, &hostile_clone);
        set_non_github_origin(&hostile_clone);
        mark_worktree_owned(&hostile_clone, "issue-dispatch:fsmonitor-poc#1008")
            .expect("mark_worktree_owned must succeed");

        let sentinel = scratch.path().join("pwned.txt");
        let payload_path = scratch.path().join("payload.sh");
        std::fs::write(
            &payload_path,
            format!("#!/bin/sh\nprintf 'PWNED\\n' >> '{}'\n", sentinel.display()),
        )
        .unwrap();
        std::fs::set_permissions(&payload_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Mirrors auditor A3's own PoC exactly: `git -C hostile-clone
        // config core.fsmonitor <payload>`, the shape `git status` (run by
        // `check_cleanliness` inside `isolated_clone_report`) would honour
        // absent the `-c core.fsmonitor=` override.
        let out = std::process::Command::new("git")
            .current_dir(&hostile_clone)
            .args([
                "config",
                "core.fsmonitor",
                &payload_path.display().to_string(),
            ])
            .output()
            .expect("git config must spawn");
        assert!(
            out.status.success(),
            "git config core.fsmonitor failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        assert!(
            reports.iter().any(|r| r.real_path == hostile_clone),
            "sanity: the hostile clone must still be discovered and processed \
             (check_cleanliness runs against it)"
        );

        assert!(
            !sentinel.exists(),
            "a forged core.fsmonitor payload on a discovery candidate must NEVER execute \
             during examine_worktrees -- fork#325 M4a auditor A3 / M4b B4 (previously only \
             manually verified)"
        );
    }

    /// Scenario: fork issue #325 M4b reviewer R1 / auditor D1 (blocker,
    /// PR #515) -- the bare 3-file forgery, with NO `git clone` and NO
    /// call into `provision_isolated_clone_sync` anywhere in the chain: a
    /// sibling `.git` directory, a hand-planted `dot-agent-deck-owner`
    /// marker claiming an arbitrary identity, and a self-planted, empty
    /// [`ISOLATED_CLONE_PROVENANCE_FILENAME`] file -- exactly auditor D1's
    /// Lab A reproduction. `candidate_has_attach_lock` currently checks
    /// only `.is_file()` on that last path, a path fully inside the
    /// candidate's own (attacker-controlled) `.git`, so today's forgery
    /// costs one extra `touch` over test 054's and currently reports
    /// `owned: true`, attributing the forged identity and satisfying
    /// `--mine` for it. Asserts the CORRECT, not-yet-shipped behavior --
    /// RED today.
    #[spec("worktree/reclaim/061")]
    #[test]
    fn worktree_reclaim_061_bare_three_file_forgery_of_provenance_artifact_itself_reports_owned_false()
     {
        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let forged = scratch.path().join("repo-isolated-bare-forged");
        let forged_git_dir = forged.join(".git");
        std::fs::create_dir_all(&forged_git_dir).unwrap();
        std::fs::write(
            forged_git_dir.join(OWNER_MARKER_FILENAME),
            "deck\ncreated-by: orchestration:victim\n",
        )
        .unwrap();
        // The whole point: the attacker plants the provenance artifact
        // itself -- no git invocation, no real provisioning call, nothing
        // outside these three attacker-written files.
        std::fs::write(forged_git_dir.join(ISOLATED_CLONE_PROVENANCE_FILENAME), b"").unwrap();

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let forged_report = reports.iter().find(|r| r.real_path == forged).expect(
            "a structurally-present sibling (.git directory + owner marker) must still be \
             discovered -- discovery stays purely structural",
        );

        assert!(
            !forged_report.owned,
            "a bare 3-file forgery -- .git dir, owner marker, and a self-planted provenance \
             artifact, with no git clone and no provisioning call involved at all -- must never \
             report owned: true; fork#325 M4b reviewer R1 / auditor D1, got {forged_report:?}"
        );
        assert_eq!(
            forged_report.owner, None,
            "the forged marker's content must never be read/trusted off a self-planted \
             provenance artifact, got owner {:?}",
            forged_report.owner
        );
        assert!(
            !is_mine(forged_report, "orchestration:victim"),
            "the forged row must never satisfy --mine, even for the exact identity its own \
             self-planted marker claims"
        );
        assert!(
            !is_mine(forged_report, "test-remover"),
            "the forged row must never satisfy --mine for any other identity either"
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3 (reviewer B2). An
    /// isolated clone that is OWNED (a real attach-lock artifact, via the
    /// real `provision_isolated_clone_sync` provisioner), CLEAN (no
    /// uncommitted/untracked content), has exactly one local branch
    /// matching its resolved branch with an EMPTY `git stash list` (no
    /// local-only refs `remove_dir_all` could destroy without a copy
    /// elsewhere), and whose own `git rev-parse HEAD` equals the merged
    /// PR's `headRefOid` exactly -- the PR branch's own head commit, not
    /// the merge commit GitHub creates on the base branch -- must report
    /// as auto-reclaim-eligible. Round 2's `mergeCommit`/tree-SHA
    /// comparison is dropped entirely here: reviewer B2 measured against
    /// this very repo that a deck-provisioned clone's HEAD is never equal
    /// to `mergeCommit.oid` under any GitHub merge strategy (PR #481 head
    /// `7339edd5f440` vs merge `1ceb919349ef`), so that comparison could
    /// (almost) never fire in production -- `headRefOid` is exactly the
    /// clone's own reachable commit history instead.
    #[spec("worktree/reclaim/062")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_062_owned_clean_single_branch_head_ref_oid_match_reports_reclaim_eligible()
    {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-tightened-positive");
        let creator = "issue-dispatch:tightened-positive#2001";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "tightened-positive-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        // A fresh provisioned clone has exactly one local branch (the one
        // just checked out), a clean working tree, and no stash -- this
        // fixture is the redesigned rule's full positive case with no extra
        // setup needed for the ownership/cleanliness/extraneous-refs gates.
        let clone_head_sha = git_rev_parse_head(&clone_dir);

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(
            &bindir,
            "tightened-positive-branch",
            &clone_head_sha,
        );
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone_reclaimable",
            "an owned, clean isolated clone with a single local branch, no stash, and its own \
             HEAD commit SHA equal to the merged PR's headRefOid must report as \
             auto-reclaim-eligible (fork#325 M4c, PR #526 round 3, reviewer B2) -- got verdict \
             {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3 (reviewer B2, the
    /// same headRefOid gate `worktree/reclaim/062` proves positively). An
    /// isolated clone that is owned, clean, and carries no extraneous
    /// local refs -- otherwise a full match against the redesigned rule --
    /// but whose CURRENT HEAD commit has diverged from the merged PR's
    /// `headRefOid` (an extra local commit made after the point the PR
    /// merged) must stay exactly as conservative as `"isolated_clone"`.
    /// Built through the real provisioner so this isolates the
    /// headRefOid-equality gate from the ownership/cleanliness/
    /// extraneous-refs gates `worktree_reclaim_064`-`067` each cover on
    /// their own.
    #[spec("worktree/reclaim/063")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_063_head_commit_diverged_from_pr_head_ref_oid_stays_conservative() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-head-diverged");
        let creator = "issue-dispatch:head-diverged#2002";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "head-diverged-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        // The merged PR's headRefOid is the clone's own HEAD right after
        // provisioning -- captured BEFORE the extra local commit below, so
        // the fixture genuinely diverges from it afterward.
        let head_ref_oid = git_rev_parse_head(&clone_dir);

        // An extra local commit made after the merge -- HEAD now points
        // past the merge point, the exact "diverged" shape the redesigned
        // rule must not treat as eligible. Still clean (`git status
        // --porcelain` is empty for a committed change) and still a single
        // local branch with no stash, so this isolates the headRefOid-
        // equality gate from the other three.
        //
        // `git clone` does not inherit the source's local `user.email`/
        // `user.name` (those are per-repo config in `init_repo_with_origin`,
        // never global), and a CI runner has no global git identity either
        // -- so committing here needs its own identity config, exactly like
        // `init_repo_with_origin`'s own two `git config` calls.
        let config_email = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["config", "user.email", "test@example.com"])
            .output()
            .expect("git config user.email must spawn");
        assert!(
            config_email.status.success(),
            "git config user.email failed: {}",
            String::from_utf8_lossy(&config_email.stderr)
        );
        let config_name = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["config", "user.name", "Test"])
            .output()
            .expect("git config user.name must spawn");
        assert!(
            config_name.status.success(),
            "git config user.name failed: {}",
            String::from_utf8_lossy(&config_name.stderr)
        );

        std::fs::write(clone_dir.join("extra.txt"), "local work\n").unwrap();
        let add_out = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["add", "extra.txt"])
            .output()
            .expect("git add must spawn");
        assert!(
            add_out.status.success(),
            "git add failed: {}",
            String::from_utf8_lossy(&add_out.stderr)
        );
        let commit_out = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["commit", "--quiet", "-m", "local work after merge"])
            .output()
            .expect("git commit must spawn");
        assert!(
            commit_out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit_out.stderr)
        );

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "head-diverged-branch", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone",
            "an owned, clean isolated clone with no extraneous local refs, but whose HEAD \
             commit has diverged past the merged PR's headRefOid, must stay exactly as \
             conservative as worktree_reclaim_052 -- never an automatic-removal verdict, and \
             never the redesigned M4c reclaim-eligible verdict either -- got {:?} \
             (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );

        // Neither a bare reclaim nor `--yes` may actually delete it,
        // mirroring worktree_reclaim_052's own removal assertions.
        let bare = run_reclaim(&repo, false, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !bare.removed.iter().any(|r| r.real_path == clone_dir),
            "a bare `worktree reclaim` must never remove a head-diverged isolated clone, got \
             removed: {:?}",
            bare.removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the head-diverged isolated clone directory must still exist on disk after a bare \
             reclaim"
        );

        let confirmed = run_reclaim(&repo, true, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !confirmed.removed.iter().any(|r| r.real_path == clone_dir),
            "`worktree reclaim --yes` must never remove a head-diverged isolated clone either, \
             got removed: {:?}",
            confirmed
                .removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the head-diverged isolated clone directory must still exist on disk after \
             `reclaim --yes`"
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3 -- an isolated clone
    /// that is owned, has a single local branch matching its resolved
    /// branch, no stash entries, and a HEAD commit equal to the merged
    /// PR's headRefOid -- otherwise a full match against the redesigned
    /// rule -- but carries an UNCOMMITTED change must stay exactly as
    /// conservative as `"isolated_clone"`, never the reclaim-eligible
    /// verdict. `remove_dir_all` has no equivalent of `git worktree
    /// remove`'s own refusal against a dirty tree, so eligibility itself
    /// must consult cleanliness rather than leaving it a display-only
    /// field.
    #[spec("worktree/reclaim/064")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_064_dirty_otherwise_matching_isolated_clone_stays_conservative() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-dirty");
        let creator = "issue-dispatch:dirty#2003";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "dirty-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);

        // Uncommitted, untracked content -- captured AFTER the SHA the
        // headRefOid stub below claims to match, so this isolates the
        // cleanliness gate from the headRefOid-equality gate.
        std::fs::write(clone_dir.join("uncommitted.txt"), "dirty work\n").unwrap();

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "dirty-branch", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert!(
            !clone_report.clean,
            "sanity: the fixture's uncommitted file must actually register as dirty, got \
             clean: {}",
            clone_report.clean
        );
        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone",
            "an owned isolated clone with a matching HEAD commit, a single local branch, and no \
             stash, but an UNCOMMITTED change, must never report the redesigned M4c \
             reclaim-eligible verdict -- `remove_dir_all` has no equivalent of `git worktree \
             remove`'s own refusal against a dirty tree -- got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3 -- the
    /// security-relevant case. An isolated clone that LOOKS otherwise fully
    /// eligible -- a genuine `git clone`, a single local branch matching
    /// its resolved branch, clean, no stash, and a HEAD commit equal to
    /// the merged PR's headRefOid -- but carries NO deck attach-lock
    /// provenance artifact (the exact M4b forgery shape
    /// `worktree_reclaim_054` already pins for the DISPLAY `owned` field,
    /// now asserted against the DELETION decision instead) must stay
    /// exactly as conservative as `"isolated_clone"`. The fixture is built
    /// the SAME way as a genuine deck-owned clone in every other respect --
    /// it is missing only the one signal a same-uid attacker able to plant
    /// a sibling directory cannot forge, so this proves eligibility cannot
    /// rest entirely on evidence the candidate directory itself controls.
    #[spec("worktree/reclaim/065")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_065_unowned_but_otherwise_matching_isolated_clone_stays_conservative() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        // Deliberately NOT provision_isolated_clone_sync -- a real `git
        // clone` plus a hand-planted ownership marker (mirroring the
        // pre-tightening `worktree_reclaim_062`/`063` fixture shape)
        // produces a clone that is genuine, single-branch, clean, and
        // content-matching in every respect EXCEPT the one artifact only
        // the real provisioner ever writes: the attach-lock provenance
        // under `state_dir()`.
        let clone_dir = scratch.path().join("repo-isolated-unowned");
        clone_repo_with_github_origin(&repo, &clone_dir);
        mark_worktree_owned(&clone_dir, "issue-dispatch:unowned#2004")
            .expect("mark_worktree_owned must succeed");

        let head_ref_oid = git_rev_parse_head(&clone_dir);

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "main", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert!(
            !clone_report.owned,
            "sanity: the fixture must carry no attach-lock provenance artifact -- got \
             owned: {}",
            clone_report.owned
        );
        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone",
            "a clean, single-branch, content-matching isolated clone that carries NO deck \
             attach-lock provenance artifact must never report the redesigned M4c \
             reclaim-eligible verdict -- eligibility must never rest entirely on evidence the \
             candidate directory itself controls -- got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3 -- an isolated
    /// clone that is owned, clean, content-matching, and carries no stash,
    /// but has a SECOND local branch beyond the one it resolved/checked
    /// out, must stay exactly as conservative as `"isolated_clone"`. `git
    /// rev-parse HEAD` proves ONE ref is safe to discard; `remove_dir_all`
    /// destroys the WHOLE clone, including every other local branch -- a
    /// second branch may hold commits with no copy anywhere else.
    #[spec("worktree/reclaim/066")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_066_extra_local_branch_stays_conservative() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-extra-branch");
        let creator = "issue-dispatch:extra-branch#2005";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "extra-branch-main",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);

        // A second local branch, never checked out -- HEAD stays on
        // `extra-branch-main`, the working tree stays clean, but the clone
        // now holds a local ref `remove_dir_all` would destroy along with
        // everything else, with no copy anywhere the deck can point to.
        let branch_out = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["branch", "extra-local-branch"])
            .output()
            .expect("git branch must spawn");
        assert!(
            branch_out.status.success(),
            "git branch failed: {}",
            String::from_utf8_lossy(&branch_out.stderr)
        );

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "extra-branch-main", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone",
            "an owned, clean, content-matching isolated clone carrying a SECOND local branch \
             must never report the redesigned M4c reclaim-eligible verdict -- HEAD-equality \
             proves one ref safe, never the whole clone `remove_dir_all` would destroy -- got \
             {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3 -- an isolated
    /// clone that is owned, clean (a stash push leaves the working tree
    /// exactly as committed), content-matching, and has exactly one local
    /// branch, but carries a NON-EMPTY `git stash list`, must stay
    /// exactly as conservative as `"isolated_clone"`. A stash entry is
    /// local-only content `remove_dir_all` would destroy with no copy
    /// anywhere else -- the same hazard as an extra branch
    /// (`worktree_reclaim_066`), through git's other local-only ref
    /// namespace.
    #[spec("worktree/reclaim/067")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_067_stash_entry_present_stays_conservative() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-stash");
        let creator = "issue-dispatch:stash#2006";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "stash-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);

        // `git stash` builds a commit object, which needs its own identity
        // -- `git clone` never inherits the source's local `user.email`/
        // `user.name` (`worktree_reclaim_063`'s own note).
        let config_email = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["config", "user.email", "test@example.com"])
            .output()
            .expect("git config user.email must spawn");
        assert!(
            config_email.status.success(),
            "git config user.email failed: {}",
            String::from_utf8_lossy(&config_email.stderr)
        );
        let config_name = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["config", "user.name", "Test"])
            .output()
            .expect("git config user.name must spawn");
        assert!(
            config_name.status.success(),
            "git config user.name failed: {}",
            String::from_utf8_lossy(&config_name.stderr)
        );

        // A tracked-file edit, stashed away -- `git stash` leaves the
        // working tree exactly as it was at HEAD (clean), but records the
        // edit as a stash entry: local-only content with no copy anywhere
        // else.
        std::fs::write(clone_dir.join("README.md"), "seed\nstashed edit\n").unwrap();
        let stash_out = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["stash", "push", "--quiet", "-m", "local work"])
            .output()
            .expect("git stash push must spawn");
        assert!(
            stash_out.status.success(),
            "git stash push failed: {}",
            String::from_utf8_lossy(&stash_out.stderr)
        );

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "stash-branch", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert!(
            clone_report.clean,
            "sanity: a stash push must leave the working tree exactly as committed (clean), \
             got clean: {}",
            clone_report.clean
        );
        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone",
            "an owned, clean, content-matching, single-branch isolated clone carrying a \
             NON-EMPTY `git stash list` must never report the redesigned M4c reclaim-eligible \
             verdict -- a stash entry is local-only content `remove_dir_all` would destroy \
             with no copy anywhere else -- got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3, reviewer B2 -- the
    /// case that PROVES the fix rather than merely adding a gate. Reviewer
    /// B2 measured live against this very repo that `gh pr list`'s
    /// `mergeCommit.oid` -- the commit GitHub's merge creates ON THE BASE
    /// BRANCH -- is never equal to a PR branch's own tip under any GitHub
    /// merge strategy (PR #481 head `7339edd5f440` vs merge
    /// `1ceb919349ef`; PR #477 head `11d6327f2421` vs merge
    /// `5742ad1f93dd`), which is why round 2's rule (comparing against
    /// that field's tree) could almost never fire for a genuine
    /// deck-provisioned clone. This fixture's `gh` stub carries a
    /// `headRefOid` that matches the clone's own HEAD commit AND a
    /// mismatched decoy `mergeCommit.oid` alongside it, even though the
    /// redesigned `gh pr list --json` call no longer requests that field
    /// -- proving eligibility is decided from `headRefOid` and would stay
    /// correct even if a raw `gh` response happened to carry other,
    /// unrelated-and-differing commit data alongside it.
    #[spec("worktree/reclaim/068")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_068_head_ref_oid_match_survives_a_mismatched_decoy_merge_commit_field() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-decoy-merge-commit");
        let creator = "issue-dispatch:decoy-merge-commit#2007";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "decoy-merge-commit-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);

        // A decoy `mergeCommit.oid` that deliberately does NOT match the
        // clone's own HEAD -- reviewer B2's own live measurement shows real
        // `gh` always returns a `mergeCommit` for a MERGED PR, and never one
        // equal to the PR branch's own tip. Reversing the real SHA keeps it
        // a plausible-looking 40-char hex string while guaranteeing it
        // differs from `head_ref_oid` (not merely by construction --
        // asserted below).
        let decoy_merge_commit_sha: String = head_ref_oid.chars().rev().collect();
        assert_ne!(
            decoy_merge_commit_sha, head_ref_oid,
            "sanity: the decoy mergeCommit SHA must actually differ from the clone's own HEAD, \
             or this test would not be exercising the B2 regression at all"
        );

        let branch = "decoy-merge-commit-branch";
        let gh_script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then\n    printf '%s\\n' \
             '[{{\"state\":\"MERGED\",\"headRefName\":\"{branch}\",\"headRepositoryOwner\":{{\"login\":\"test-org\"}},\"headRefOid\":\"{head_ref_oid}\",\"mergeCommit\":{{\"oid\":\"{decoy_merge_commit_sha}\"}}}}]'\n    \
             exit 0\nfi\nexit 1\n"
        );
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let gh_path = bindir.join("gh");
        std::fs::write(&gh_path, gh_script).unwrap();
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone_reclaimable",
            "an owned, clean, single-branch, no-stash isolated clone whose HEAD commit equals \
             the merged PR's headRefOid must report the redesigned M4c reclaim-eligible \
             verdict even when gh's raw response also carries a mismatched mergeCommit.oid -- \
             proving eligibility is decided from headRefOid, never mergeCommit (fork#325 M4c, \
             PR #526 round 3, reviewer B2) -- got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3, reviewer H1 -- the
    /// new deletion primitive (`remove_isolated_clone_dir`) had zero test
    /// coverage; neither new `run_reclaim` arm was exercised. A genuinely
    /// eligible isolated clone (owned, clean, single-branch, no stash,
    /// HEAD commit equal to the merged PR's headRefOid) run through
    /// `worktree reclaim --yes` must actually be removed from disk -- not
    /// merely reported as removed -- and the returned `ReclaimOutcome`
    /// must record it under `removed` with `removed_by` set to the
    /// caller-supplied remover identity.
    #[spec("worktree/reclaim/069")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_069_yes_actually_removes_an_eligible_isolated_clone_from_disk() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-yes-removes");
        let creator = "issue-dispatch:yes-removes#2008";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "yes-removes-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);
        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "yes-removes-branch", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let remover = "test-remover";
        let outcome = run_reclaim(&repo, true, remover)
            .expect("run_reclaim must succeed against a real git repo");

        assert!(
            outcome.removed.iter().any(|r| r.real_path == clone_dir),
            "an eligible isolated clone run through `worktree reclaim --yes` must be reported \
             under `removed`, got removed={:?} pending={:?} kept={:?}",
            outcome
                .removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>(),
            outcome
                .pending
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>(),
            outcome
                .kept
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        let removed_report = outcome
            .removed
            .iter()
            .find(|r| r.real_path == clone_dir)
            .unwrap();
        assert_eq!(
            removed_report.removed_by.as_deref(),
            Some(remover),
            "the removed isolated clone's report must record who ran the reclaim, got {:?}",
            removed_report.removed_by
        );
        assert!(
            !clone_dir.exists(),
            "the isolated clone directory must actually be gone from disk after \
             `worktree reclaim --yes`, not merely reported as removed"
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3, reviewer H1 -- the
    /// bare-run half of the same coverage gap `worktree/reclaim/069` closes
    /// for `--yes`. A genuinely eligible isolated clone run through a BARE
    /// `worktree reclaim` (no `--yes`) must be left on disk untouched and
    /// reported under `pending`, never under `removed` -- eligibility alone
    /// is not consent; the isolated-clone path is gated on `--yes` exactly
    /// like an ordinary foreign worktree's `Ask` verdict is.
    #[spec("worktree/reclaim/070")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_070_bare_reclaim_leaves_an_eligible_isolated_clone_pending() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-bare-pending");
        let creator = "issue-dispatch:bare-pending#2009";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "bare-pending-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);
        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "bare-pending-branch", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let outcome = run_reclaim(&repo, false, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");

        assert!(
            !outcome.removed.iter().any(|r| r.real_path == clone_dir),
            "a bare `worktree reclaim` (no --yes) must never remove an eligible isolated \
             clone, got removed={:?}",
            outcome
                .removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            outcome.pending.iter().any(|r| r.real_path == clone_dir),
            "an eligible isolated clone left untouched by a bare reclaim must be reported \
             under `pending`, got pending={:?} kept={:?}",
            outcome
                .pending
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>(),
            outcome
                .kept
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the isolated clone directory must still exist on disk after a bare reclaim"
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3, reviewer M1/H1 --
    /// `remove_isolated_clone_dir`'s own last-moment structural check,
    /// exercised directly rather than through `run_reclaim`. When the path
    /// it is asked to delete no longer has a `.git` DIRECTORY at removal
    /// time (simulating the TOCTOU window reviewer M1 flagged between
    /// `examine_worktrees`' examination pass and the actual delete -- here
    /// reproduced by a `.git` FILE, the shape of a linked worktree's
    /// admin-dir redirect, not a plain clone's own repository), the
    /// function must refuse rather than delete: it returns an `Err`, and
    /// the directory (including its other contents) is left untouched.
    #[spec("worktree/reclaim/071")]
    #[test]
    fn worktree_reclaim_071_remove_isolated_clone_dir_refuses_when_git_is_not_a_directory() {
        let scratch = tempfile::tempdir().unwrap();
        let clone_dir = scratch.path().join("no-longer-a-clone");
        std::fs::create_dir_all(&clone_dir).unwrap();
        // A `.git` FILE (a linked worktree's own admin-dir redirect shape),
        // not a `.git` DIRECTORY (an isolated clone's own repository) --
        // exactly what `remove_isolated_clone_dir`'s own check requires
        // immediately before deleting.
        std::fs::write(clone_dir.join(".git"), b"gitdir: /elsewhere\n").unwrap();
        std::fs::write(clone_dir.join("some-file.txt"), b"still here\n").unwrap();

        let result = remove_isolated_clone_dir(&clone_dir, "test-remover", None);

        let err = result.expect_err(
            "remove_isolated_clone_dir must refuse when `.git` is no longer a directory at \
             removal time",
        );
        assert!(
            err.contains("no `.git` directory found"),
            "the refusal must fail at the `.git`-shape check, not some other guard, got: {err}"
        );
        assert!(
            clone_dir.exists(),
            "the directory must be left on disk when the refusal fires, not partially or \
             fully removed"
        );
        assert!(
            clone_dir.join("some-file.txt").exists(),
            "the refusal must not touch the directory's other contents"
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 3, reviewer M3 --
    /// `worktree_reclaim_052` passes for an undocumented reason: its `gh`
    /// stub omits `headRefOid` entirely, so it only ever exercises the
    /// "head ref unresolvable" (`None`) path, never the case where
    /// `headRefOid` is genuinely PRESENT in the response but simply does
    /// not match the clone's own HEAD. This test closes that gap
    /// explicitly: an owned, clean, single-branch, no-stash isolated clone
    /// whose merged PR carries a well-formed but MISMATCHED `headRefOid`
    /// must stay exactly as conservative as `"isolated_clone"`.
    #[spec("worktree/reclaim/072")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_072_head_ref_oid_present_but_mismatched_stays_conservative() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-mismatched-head-ref-oid");
        let creator = "issue-dispatch:mismatched-head-ref-oid#2010";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "mismatched-head-ref-oid-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let clone_head_sha = git_rev_parse_head(&clone_dir);

        // A well-formed 40-char hex `headRefOid` that is deliberately NOT
        // the clone's own HEAD -- present, unlike worktree_reclaim_052's
        // fixture, and reversing the real SHA guarantees it differs (not
        // merely by construction -- asserted below), mirroring
        // worktree_reclaim_068's own decoy technique.
        let mismatched_head_ref_oid: String = clone_head_sha.chars().rev().collect();
        assert_ne!(
            mismatched_head_ref_oid, clone_head_sha,
            "sanity: the mismatched headRefOid must actually differ from the clone's own HEAD, \
             or this test would not be exercising the present-but-mismatched path at all"
        );

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(
            &bindir,
            "mismatched-head-ref-oid-branch",
            &mismatched_head_ref_oid,
        );
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone",
            "an owned, clean, single-branch, no-stash isolated clone whose merged PR carries a \
             well-formed but MISMATCHED headRefOid must never report the redesigned M4c \
             reclaim-eligible verdict -- got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );

        let bare = run_reclaim(&repo, false, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !bare.removed.iter().any(|r| r.real_path == clone_dir),
            "a bare `worktree reclaim` must never remove a mismatched-headRefOid isolated \
             clone, got removed: {:?}",
            bare.removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the mismatched-headRefOid isolated clone directory must still exist on disk after \
             a bare reclaim"
        );

        let confirmed = run_reclaim(&repo, true, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !confirmed.removed.iter().any(|r| r.real_path == clone_dir),
            "`worktree reclaim --yes` must never remove a mismatched-headRefOid isolated clone \
             either, got removed: {:?}",
            confirmed
                .removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the mismatched-headRefOid isolated clone directory must still exist on disk after \
             `reclaim --yes`"
        );
    }

    /// Scenario: fork issue #325 M4c, PR #526 round 4 (reviewer/auditor N2)
    /// -- `remove_isolated_clone_dir`'s own TOCTOU re-verification
    /// (cleanliness, single local branch, empty stash, and HEAD-vs-merged-PR
    /// headRefOid, each re-derived fresh immediately before deletion) had
    /// zero test coverage: deleting that whole block would leave every
    /// existing test in this file green. This test genuinely opens the
    /// TOCTOU window rather than handing the primitive an already-bad
    /// fixture: a real isolated clone is provisioned and confirmed via
    /// `examine_worktrees` to be reclaim-eligible, THEN an untracked file is
    /// written into it -- simulating work happening in the window between
    /// examination and deletion -- and only then is
    /// `remove_isolated_clone_dir` called directly, carrying exactly the
    /// stale eligibility `run_reclaim` would have handed it. The clone must
    /// be refused, not deleted.
    #[spec("worktree/reclaim/073")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_073_toctou_reverification_refuses_a_clone_dirtied_after_examination() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-toctou-dirty");
        let creator = "issue-dispatch:toctou-dirty#2011";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "toctou-dirty-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);
        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "toctou-dirty-branch", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        // Confirm the clone is genuinely eligible at examination time --
        // proves this is a real TOCTOU window, not a fixture that was
        // already ineligible when handed to the removal primitive.
        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports
            .iter()
            .find(|r| r.real_path == clone_dir)
            .expect("the isolated clone must be present in the report");
        assert_eq!(
            clone_report.verdict.as_str(),
            VERDICT_ISOLATED_CLONE_RECLAIMABLE,
            "sanity: the clone must be reclaim-eligible at examination time, before the TOCTOU \
             mutation below, or this test would not be exercising the re-verification's refusal \
             path at all -- got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );

        // Open the TOCTOU window: dirty the clone (an untracked file) after
        // examination confirmed it clean, before removal actually runs.
        std::fs::write(clone_dir.join("dirtied-after-examination.txt"), b"oops\n").unwrap();

        // Call the removal primitive directly, carrying the now-stale
        // eligibility `run_reclaim` would have carried from the report
        // above -- this is exactly the sequence `run_reclaim` follows
        // internally, with the mutation inserted in the window between its
        // two steps.
        let result = remove_isolated_clone_dir(
            &clone_dir,
            "test-remover",
            derive_repo_slug(&repo).as_deref(),
        );

        let err = result.expect_err(
            "remove_isolated_clone_dir must refuse when the clone has been dirtied since it \
             was examined, not trust the stale examination-time verdict",
        );
        assert!(
            err.contains("no longer clean"),
            "the refusal must fail at the cleanliness re-check, not some other guard, got: {err}"
        );
        assert!(
            clone_dir.exists(),
            "the clone directory must be left on disk when the TOCTOU refusal fires"
        );
        assert!(
            clone_dir.join("dirtied-after-examination.txt").exists(),
            "the refusal must not touch the directory's other contents"
        );
    }

    /// Scenario: fork issue #546 hazard 1 -- `remove_isolated_clone_dir`
    /// deletes an isolated clone's directory via `remove_dir_all` but never
    /// clears the M4b provenance artifact
    /// (`issue_dispatch_run::isolated_clone_provenance_path`) that vouches
    /// for that path, even though the artifact lives entirely outside the
    /// directory being deleted (in `state_dir()`, by design -- see that
    /// function's own doc comment). A later, unrelated directory created at
    /// the same path would then be silently vouched for by this stale
    /// evidence -- exactly the hazard PRD #544's own Risks section names,
    /// reachable here via the heuristic-reclaim path that PRD never
    /// touched. A genuinely eligible isolated clone (owned, clean,
    /// single-branch, no stash, HEAD equal to the merged PR's
    /// `headRefOid`) removed via `worktree reclaim --yes` must leave no
    /// provenance artifact behind.
    #[spec("worktree/reclaim/074")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_074_removal_clears_the_provenance_artifact() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-provenance-cleared");
        let creator = "issue-dispatch:provenance-cleared#2012";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "provenance-cleared-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let provenance_path = crate::issue_dispatch_run::isolated_clone_provenance_path(&clone_dir);
        assert!(
            provenance_path.is_file(),
            "sanity: the real provisioner must have written a provenance artifact at {} before \
             removal is exercised, or this test isn't exercising the clearing behavior at all",
            provenance_path.display()
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);
        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "provenance-cleared-branch", &head_ref_oid);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let outcome = run_reclaim(&repo, true, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            outcome.removed.iter().any(|r| r.real_path == clone_dir),
            "sanity: the clone must actually be removed for this test to prove anything about \
             what removal leaves behind, got removed={:?} pending={:?} kept={:?}",
            outcome
                .removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>(),
            outcome
                .pending
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>(),
            outcome
                .kept
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        assert!(
            !clone_dir.exists(),
            "the isolated clone directory must be gone from disk after `worktree reclaim --yes`"
        );

        assert!(
            !provenance_path.is_file(),
            "the M4b provenance artifact at {} must be cleared when the isolated clone it \
             vouches for is removed -- a later, unrelated directory created at the same path \
             would otherwise be silently vouched for by this stale evidence (fork issue #546 \
             hazard 1)",
            provenance_path.display()
        );
    }

    /// Scenario: fork issue #546 hazard 2 (RED, maintainer-decided design).
    /// An isolated clone that satisfies every one of M4c's five existing
    /// reclaim-eligibility gates (owned, clean, single local branch, empty
    /// stash, HEAD equal to a merged PR's `headRefOid`) -- the exact
    /// fixture `worktree_reclaim_062` proves reclaim-eligible on its own --
    /// must NEVER report as auto-reclaim-eligible once it has been
    /// explicitly pinned via `issue_dispatch_run::pin_isolated_clone`.
    /// `name=` in the provenance artifact is populated for every isolated
    /// clone, intentionally named or not, so it can't be used on its own to
    /// tell "ephemeral clone, fine to delete" apart from "the user is
    /// deliberately still resuming this by name" -- an explicit pin flag
    /// is the signal that does.
    #[spec("worktree/reclaim/075")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_075_pinned_clone_never_reclaim_eligible_even_when_all_five_gates_hold() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-pinned");
        let creator = "issue-dispatch:pinned#2013";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "pinned-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        // Same fixture shape as worktree_reclaim_062's full positive case:
        // a fresh provisioned clone is already clean, single-branch, and
        // stash-empty, so only the merged-PR headRefOid match needs
        // setting up before pinning is layered on top.
        let clone_head_sha = git_rev_parse_head(&clone_dir);

        crate::issue_dispatch_run::pin_isolated_clone(&clone_dir)
            .expect("pin_isolated_clone must succeed against a real, just-provisioned clone");

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(&bindir, "pinned-branch", &clone_head_sha);
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_ne!(
            clone_report.verdict.as_str(),
            "isolated_clone_reclaimable",
            "a PINNED isolated clone must never report as auto-reclaim-eligible, even when all \
             five M4c gates hold exactly as worktree_reclaim_062's fixture proves them \
             sufficient on their own -- got verdict {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );

        // Never actually removed either, bare or with --yes -- mirrors
        // worktree_reclaim_063's own removal assertions for the other
        // negative gates.
        let bare = run_reclaim(&repo, false, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !bare.removed.iter().any(|r| r.real_path == clone_dir),
            "a bare `worktree reclaim` must never remove a pinned isolated clone, got removed: \
             {:?}",
            bare.removed
                .iter()
                .map(|r| &r.real_path)
                .collect::<Vec<_>>()
        );
        let yes = run_reclaim(&repo, true, "test-remover")
            .expect("run_reclaim must succeed against a real git repo");
        assert!(
            !yes.removed.iter().any(|r| r.real_path == clone_dir),
            "`worktree reclaim --yes` must never remove a pinned isolated clone, got removed: \
             {:?}",
            yes.removed.iter().map(|r| &r.real_path).collect::<Vec<_>>()
        );
        assert!(
            clone_dir.exists(),
            "the pinned isolated clone must still exist on disk after both reclaim attempts"
        );
    }

    /// Scenario: fork issue #546 hazard 2 (RED), regression guard for the
    /// explicit-off case. An isolated clone whose provenance artifact has
    /// been explicitly written to schema=3 with `pinned=false` -- via
    /// `issue_dispatch_run::unpin_isolated_clone`, called here on a clone
    /// that was never pinned to begin with -- must behave exactly like
    /// today's unpinned (schema=2) clone: reclaim-eligible once all five
    /// existing M4c gates hold. Only `pinned=true` is meant to narrow
    /// eligibility; `pinned=false` (whether written by an explicit unpin
    /// or never touched at all) must never change it relative to today's
    /// schema=2 behavior (`worktree_reclaim_062`, which remains the
    /// regression guard for the untouched-schema=2 case and needs no
    /// duplicate here).
    #[spec("worktree/reclaim/076")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_076_explicitly_unpinned_schema3_clone_stays_reclaim_eligible() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-explicitly-unpinned");
        let creator = "issue-dispatch:explicitly-unpinned#2014";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "explicitly-unpinned-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let clone_head_sha = git_rev_parse_head(&clone_dir);

        // This clone was never pinned -- calling unpin on it must still
        // succeed and rewrite the artifact to schema=3 with `pinned=false`
        // explicitly, rather than requiring a prior pin call first.
        crate::issue_dispatch_run::unpin_isolated_clone(&clone_dir).expect(
            "unpin_isolated_clone must succeed against a real, just-provisioned clone even when \
             it was never pinned",
        );

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(
            &bindir,
            "explicitly-unpinned-branch",
            &clone_head_sha,
        );
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_eq!(
            clone_report.verdict.as_str(),
            "isolated_clone_reclaimable",
            "an explicitly-unpinned (schema=3, pinned=false) isolated clone must behave exactly \
             like today's schema=2 clone once all five existing M4c gates hold -- reclaim- \
             eligible, unchanged by adding the pin mechanism -- got verdict {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #546 hazard 2, review round (RED -- reviewer F1 /
    /// auditor B1, blocker). `remove_isolated_clone_dir`'s TOCTOU
    /// re-verification re-derives cleanliness, the local branch list, the
    /// stash list, and HEAD-vs-merged-PR headRefOid fresh immediately before
    /// deletion, but never re-derives the pin gate `isolated_clone_report`
    /// added -- so a clone examined as reclaim-eligible while unpinned, then
    /// pinned during the examination-to-removal window (the doc comment's
    /// own "seconds to minutes" batch window), is still deleted. This test
    /// opens exactly that window: examine the clone unpinned and confirm it
    /// is reclaim-eligible, THEN pin it, THEN call `remove_isolated_clone_dir`
    /// directly, carrying the now-stale eligibility `run_reclaim` would have
    /// handed it -- the clone must be refused, not deleted.
    #[spec("worktree/reclaim/077")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_077_toctou_reverification_refuses_a_clone_pinned_after_examination() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch
            .path()
            .join("repo-isolated-pinned-after-examination");
        let creator = "issue-dispatch:pinned-after-examination#2015";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "pinned-after-examination-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let head_ref_oid = git_rev_parse_head(&clone_dir);
        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(
            &bindir,
            "pinned-after-examination-branch",
            &head_ref_oid,
        );
        let _path_guard = PathEnvGuard::prepend(&bindir);

        // Confirm the clone is genuinely eligible at examination time, still
        // unpinned -- proves this is a real TOCTOU window opened by pinning
        // AFTER examination, not a fixture that was already ineligible when
        // handed to the removal primitive.
        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
        let clone_report = reports
            .iter()
            .find(|r| r.real_path == clone_dir)
            .expect("the isolated clone must be present in the report");
        assert_eq!(
            clone_report.verdict.as_str(),
            VERDICT_ISOLATED_CLONE_RECLAIMABLE,
            "sanity: the clone must be reclaim-eligible at examination time, before it is \
             pinned below, or this test would not be exercising the TOCTOU refusal path at all \
             -- got {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );

        // Open the TOCTOU window: pin the clone after examination confirmed
        // it eligible, before removal actually runs -- the exact scenario
        // reviewer F1 / auditor B1 describe: "the manual signal that a clone
        // is still in use" arriving during the batch window.
        crate::issue_dispatch_run::pin_isolated_clone(&clone_dir)
            .expect("pin_isolated_clone must succeed against a real, just-provisioned clone");

        // Call the removal primitive directly, carrying the now-stale
        // eligibility `run_reclaim` would have carried from the report
        // above -- this is exactly the sequence `run_reclaim` follows
        // internally, with the pin inserted in the window between its two
        // steps.
        let result = remove_isolated_clone_dir(
            &clone_dir,
            "test-remover",
            derive_repo_slug(&repo).as_deref(),
        );

        let err = result.expect_err(
            "remove_isolated_clone_dir must refuse when the clone has been pinned since it was \
             examined, not trust the stale examination-time verdict",
        );
        assert!(
            err.contains("has been pinned"),
            "the refusal must fail at the pin re-check, not some other guard, got: {err}"
        );
        assert!(
            clone_dir.exists(),
            "the clone directory must be left on disk when the TOCTOU refusal fires for a pin \
             applied during the removal window"
        );
    }

    /// Scenario: fork issue #546 hazard 2, review round (RED -- reviewer F2).
    /// Every one of the six gates `isolated_clone_report` ANDs together is
    /// documented to fail CLOSED (unresolvable -> not eligible) except the
    /// pin gate, which reads `.ok()` on the provenance artifact and so fails
    /// OPEN: an unreadable artifact (permissions, race, corruption) becomes
    /// `None` -> "not pinned" -> reclaim-eligible. This test builds a clone
    /// that satisfies every other gate, strips read permission from its own
    /// provenance artifact (the same file `has_attach_lock`'s `is_file()`
    /// check already proved present, which needs no read permission to
    /// stat), and asserts the clone must NOT report as reclaim-eligible --
    /// fails closed, exactly like every other unresolvable signal in this
    /// chain.
    #[spec("worktree/reclaim/078")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_078_unreadable_provenance_artifact_fails_closed_not_reclaim_eligible() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-unreadable-provenance");
        let creator = "issue-dispatch:unreadable-provenance#2016";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "unreadable-provenance-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let clone_head_sha = git_rev_parse_head(&clone_dir);

        let provenance_path = crate::issue_dispatch_run::isolated_clone_provenance_path(&clone_dir);
        assert!(
            provenance_path.is_file(),
            "sanity: the real provisioner must have written a provenance artifact at {} before \
             its read permission is stripped below, or this test isn't exercising the \
             fail-open gap at all",
            provenance_path.display()
        );
        std::fs::set_permissions(&provenance_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let bindir = scratch.path().join("bin");
        write_merged_gh_stub_with_head_ref_oid(
            &bindir,
            "unreadable-provenance-branch",
            &clone_head_sha,
        );
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");

        // Restore read permission before any assertion (and before the
        // tempdir is dropped) so a failure here doesn't leave an unreadable
        // artifact behind for a later test sharing the same scratch cleanup
        // path.
        std::fs::set_permissions(&provenance_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let clone_report = reports.iter().find(|r| r.real_path == clone_dir).expect(
            "the isolated clone must be present in the report at all -- see \
             worktree_reclaim_049",
        );

        assert_ne!(
            clone_report.verdict.as_str(),
            VERDICT_ISOLATED_CLONE_RECLAIMABLE,
            "an isolated clone whose provenance artifact exists but cannot be READ must fail \
             closed on the pin gate exactly like every other unresolvable signal in this \
             six-way chain -- not be treated as unpinned and reclaim-eligible -- got verdict \
             {:?} (reason: {:?})",
            clone_report.verdict,
            clone_report.reason
        );
    }

    /// Scenario: fork issue #546 hazard 2, review round (coverage gap --
    /// reviewer F4 / auditor M1/M2). `worktree_reclaim_075`/`076` each call
    /// pin or unpin exactly once against a fresh schema=2 artifact, so
    /// `set_isolated_clone_pinned` never reads back an artifact it itself
    /// already rewrote to schema=3 -- exactly the case its own doc comment
    /// reasons about. This test provisions a clone with a deliberately
    /// non-default `name`/`creator`, then pins, unpins, and re-pins it
    /// (three successive rewrites, the second and third each against a
    /// schema=3 artifact the previous call produced), and asserts the four
    /// preserved fields (`name=`, `creator=`, `root-hash=`, `path=`) are
    /// byte-identical to their pre-pin values after all three rewrites --
    /// the blast radius `resume_existing_isolated_clone`'s `creator=` check
    /// and `forget_isolated_workspace`'s `path=` check both depend on.
    #[spec("worktree/reclaim/079")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_079_pin_unpin_repin_round_trip_preserves_inherited_fields() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);

        let clone_dir = scratch.path().join("repo-isolated-round-trip");
        let name = "distinctive-workspace-name";
        let creator = "issue-dispatch:round-trip#2017";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo, &clone_dir, name, creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let provenance_path = crate::issue_dispatch_run::isolated_clone_provenance_path(&clone_dir);
        let original_content = std::fs::read_to_string(&provenance_path)
            .expect("sanity: the freshly provisioned artifact must be readable");
        let original_name =
            crate::issue_dispatch_run::isolated_clone_provenance_field(&original_content, "name")
                .expect("sanity: a freshly provisioned artifact must carry a name= field");
        let original_creator = crate::issue_dispatch_run::isolated_clone_provenance_field(
            &original_content,
            "creator",
        )
        .expect("sanity: a freshly provisioned artifact must carry a creator= field");
        let original_root_hash = crate::issue_dispatch_run::isolated_clone_provenance_field(
            &original_content,
            "root-hash",
        )
        .expect("sanity: a freshly provisioned artifact must carry a root-hash= field");
        let original_path =
            crate::issue_dispatch_run::isolated_clone_provenance_field(&original_content, "path")
                .expect("sanity: a freshly provisioned artifact must carry a path= field");
        assert_eq!(
            original_name, name,
            "sanity: the provisioned artifact's name= must reflect the deliberately \
             non-default name this test provisioned with, or this test proves nothing about \
             preservation"
        );

        crate::issue_dispatch_run::pin_isolated_clone(&clone_dir)
            .expect("pin_isolated_clone must succeed against a real, just-provisioned clone");
        crate::issue_dispatch_run::unpin_isolated_clone(&clone_dir).expect(
            "unpin_isolated_clone must succeed against a schema=3 clone this same test already \
             pinned",
        );
        crate::issue_dispatch_run::pin_isolated_clone(&clone_dir).expect(
            "pin_isolated_clone must succeed a SECOND time, against a schema=3 clone this same \
             test already pinned and unpinned -- the exact case set_isolated_clone_pinned's own \
             doc comment reasons about",
        );

        let final_content = std::fs::read_to_string(&provenance_path)
            .expect("the provenance artifact must still be readable after three rewrites");
        assert!(
            final_content.lines().any(|l| l.trim() == "schema=3"),
            "after pin/unpin/re-pin the artifact must carry schema=3, got:\n{final_content}"
        );
        assert_eq!(
            crate::issue_dispatch_run::isolated_clone_provenance_field(&final_content, "pinned")
                .as_deref(),
            Some("true"),
            "after pin/unpin/re-pin the artifact must record pinned=true (the final call was a \
             pin), got:\n{final_content}"
        );
        assert_eq!(
            crate::issue_dispatch_run::isolated_clone_provenance_field(&final_content, "name")
                .as_deref(),
            Some(original_name.as_str()),
            "name= must survive three successive rewrites (pin, unpin, re-pin) byte-identical \
             to its pre-pin value, got:\n{final_content}"
        );
        assert_eq!(
            crate::issue_dispatch_run::isolated_clone_provenance_field(&final_content, "creator")
                .as_deref(),
            Some(original_creator.as_str()),
            "creator= must survive three successive rewrites byte-identical to its pre-pin \
             value -- a corrupted creator= would make this clone permanently unresumable by \
             name (resume_existing_isolated_clone's NameCollision guard), got:\n{final_content}"
        );
        assert_eq!(
            crate::issue_dispatch_run::isolated_clone_provenance_field(&final_content, "root-hash")
                .as_deref(),
            Some(original_root_hash.as_str()),
            "root-hash= must survive three successive rewrites byte-identical to its pre-pin \
             value, got:\n{final_content}"
        );
        assert_eq!(
            crate::issue_dispatch_run::isolated_clone_provenance_field(&final_content, "path")
                .as_deref(),
            Some(original_path.as_str()),
            "path= must survive three successive rewrites byte-identical to its pre-pin value \
             -- a corrupted path= would make this clone permanently un-forgettable \
             (forget_isolated_workspace's stale-evidence guard), got:\n{final_content}"
        );
    }

    /// Scenario: fork issue #597 wires a `pinned` field onto `WorktreeReport`
    /// so pin state is discoverable through `worktree list --json` without
    /// substring-matching the human-readable `reason` string. Chosen shape:
    /// an always-present plain `bool` (not `Option<bool>`), mirroring
    /// `owned`/`clean` rather than `owner` -- because this field's
    /// "unresolvable" case already has an established, unambiguous meaning
    /// in this codebase (`isolated_clone_report`'s own doc comment: an
    /// unreadable pin signal "fails closed, treated as pinned", fork issue
    /// #546 hazard 2) rather than the genuinely-unknown meaning `owner`'s
    /// `None` carries, so collapsing it to a bare `true` loses no
    /// information `Option<bool>` would have preserved. Four fixtures, one
    /// per state: an isolated clone explicitly pinned (`true`); one
    /// explicitly unpinned via `unpin_isolated_clone` (`false`); one whose
    /// provenance artifact exists but cannot be read, `worktree_reclaim_078`'s
    /// own fixture shape (`true` -- fails closed); and an ordinary `"linked"`
    /// row, a real `git worktree add` with no isolated clone involved at all
    /// (`false` -- not applicable; the repo's own main worktree is excluded
    /// from `list_linked_worktrees` by design, so it can never produce a
    /// `KIND_LINKED` row to assert on). References `WorktreeReport.pinned`
    /// directly, which does not exist yet, so this is a compile-error RED.
    #[spec("worktree/reclaim/081")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_081_pinned_field_reflects_pin_state_and_fails_closed_when_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // (a) Explicitly pinned isolated clone -> pinned == true.
        {
            let scratch = tempfile::tempdir().unwrap();
            let repo = scratch.path().join("repo");
            init_repo_with_origin(&repo);

            let clone_dir = scratch.path().join("repo-isolated-pinned-field");
            let creator = "issue-dispatch:pinned-field#597";
            crate::issue_dispatch_run::provision_isolated_clone_sync(
                &repo,
                &clone_dir,
                "pinned-field-branch",
                creator,
            )
            .expect("provision_isolated_clone_sync must succeed against a real source repo");
            crate::issue_dispatch_run::pin_isolated_clone(&clone_dir)
                .expect("pin_isolated_clone must succeed against a real, just-provisioned clone");

            let clone_head_sha = git_rev_parse_head(&clone_dir);
            let bindir = scratch.path().join("bin");
            write_merged_gh_stub_with_head_ref_oid(&bindir, "pinned-field-branch", &clone_head_sha);
            let _path_guard = PathEnvGuard::prepend(&bindir);

            let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
            let clone_report = reports
                .iter()
                .find(|r| r.real_path == clone_dir)
                .expect("the isolated clone must be present in the report at all");
            assert!(
                clone_report.pinned,
                "an explicitly-pinned isolated clone must report pinned: true, got {:?}",
                clone_report.pinned
            );
        }

        // (b) Explicitly unpinned (schema=3, pinned=false) isolated clone,
        // never previously pinned -- mirrors worktree_reclaim_076's own
        // fixture shape -- -> pinned == false.
        {
            let scratch = tempfile::tempdir().unwrap();
            let repo = scratch.path().join("repo");
            init_repo_with_origin(&repo);

            let clone_dir = scratch.path().join("repo-isolated-unpinned-field");
            let creator = "issue-dispatch:unpinned-field#597";
            crate::issue_dispatch_run::provision_isolated_clone_sync(
                &repo,
                &clone_dir,
                "unpinned-field-branch",
                creator,
            )
            .expect("provision_isolated_clone_sync must succeed against a real source repo");
            crate::issue_dispatch_run::unpin_isolated_clone(&clone_dir).expect(
                "unpin_isolated_clone must succeed even against a clone that was never pinned",
            );

            let clone_head_sha = git_rev_parse_head(&clone_dir);
            let bindir = scratch.path().join("bin");
            write_merged_gh_stub_with_head_ref_oid(
                &bindir,
                "unpinned-field-branch",
                &clone_head_sha,
            );
            let _path_guard = PathEnvGuard::prepend(&bindir);

            let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
            let clone_report = reports
                .iter()
                .find(|r| r.real_path == clone_dir)
                .expect("the isolated clone must be present in the report at all");
            assert!(
                !clone_report.pinned,
                "an explicitly-unpinned isolated clone must report pinned: false, got {:?}",
                clone_report.pinned
            );
        }

        // (c) Provenance artifact exists but is unreadable (chmod 0o000) --
        // worktree_reclaim_078's own fixture -- fails closed -> pinned ==
        // true, matching isolated_clone_report's own "treated as pinned"
        // language for this exact signal.
        {
            let scratch = tempfile::tempdir().unwrap();
            let repo = scratch.path().join("repo");
            init_repo_with_origin(&repo);

            let clone_dir = scratch.path().join("repo-isolated-unreadable-field");
            let creator = "issue-dispatch:unreadable-field#597";
            crate::issue_dispatch_run::provision_isolated_clone_sync(
                &repo,
                &clone_dir,
                "unreadable-field-branch",
                creator,
            )
            .expect("provision_isolated_clone_sync must succeed against a real source repo");

            let clone_head_sha = git_rev_parse_head(&clone_dir);
            let provenance_path =
                crate::issue_dispatch_run::isolated_clone_provenance_path(&clone_dir);
            assert!(
                provenance_path.is_file(),
                "sanity: the real provisioner must have written a provenance artifact at {} \
                 before its read permission is stripped below",
                provenance_path.display()
            );
            std::fs::set_permissions(&provenance_path, std::fs::Permissions::from_mode(0o000))
                .unwrap();

            let bindir = scratch.path().join("bin");
            write_merged_gh_stub_with_head_ref_oid(
                &bindir,
                "unreadable-field-branch",
                &clone_head_sha,
            );
            let _path_guard = PathEnvGuard::prepend(&bindir);

            let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");

            // Restore read permission before any assertion, mirroring
            // worktree_reclaim_078, so a failure here doesn't leave an
            // unreadable artifact behind for a later test's cleanup.
            std::fs::set_permissions(&provenance_path, std::fs::Permissions::from_mode(0o644))
                .unwrap();

            let clone_report = reports
                .iter()
                .find(|r| r.real_path == clone_dir)
                .expect("the isolated clone must be present in the report at all");
            assert!(
                clone_report.pinned,
                "an isolated clone whose provenance artifact cannot be read must fail closed \
                 and report pinned: true, exactly like isolated_clone_report's own eligibility \
                 gate already treats this signal, got {:?}",
                clone_report.pinned
            );
        }

        // (d) Parity: an ordinary "linked" row (a real `git worktree add`,
        // no isolated clone involved at all) -> pinned == false, never
        // applicable. `list_linked_worktrees` deliberately excludes the
        // repo's own main working tree (`all.remove(0)`), so a linked row
        // only ever exists for an additional worktree -- hence
        // `add_worktree` here rather than asserting on the main checkout.
        // `set_non_github_origin` keeps `resolve_pr_state` from spawning
        // the real, ambient `gh` (reviewer F6's precedent).
        {
            let scratch = tempfile::tempdir().unwrap();
            let repo = scratch.path().join("repo");
            init_repo_with_origin(&repo);
            set_non_github_origin(&repo);

            let wt = scratch.path().join("wt-pinned-field-parity");
            add_worktree(&repo, &wt, "pinned-field-parity-branch");

            // Matched by `kind` alone, not by path: on macOS `real_path`
            // comes back through git's own realpath resolution
            // (`/private/var/...`) while `wt` is the plain, unresolved
            // tempdir join, so a path-equality match is a false negative
            // there (see `discover_isolated_clones`'s own doc comment for
            // the same class of mismatch). The main working tree is
            // excluded from `list_linked_worktrees` by design, so this
            // added worktree is the only `KIND_LINKED` row that can exist.
            let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
            let linked_report = reports
                .iter()
                .find(|r| r.kind == KIND_LINKED)
                .expect("the added worktree must report as a linked row");
            assert!(
                !linked_report.pinned,
                "an ordinary linked-worktree row must report pinned: false -- pinning is not \
                 applicable to it at all, got {:?}",
                linked_report.pinned
            );
        }
    }

    /// Scenario: fork issue #533 -- unlike `isolated_clone_report`, which
    /// (auditor B3) refuses to spend a `gh` call at all unless the
    /// candidate's own derived repo slug equals the root checkout's,
    /// `remove_isolated_clone_dir`'s TOCTOU re-verification calls
    /// `resolve_pr_state` directly against the candidate's own (untrusted)
    /// `origin`, with no slug-equality guard at removal time. This test
    /// simulates the attack the issue describes: a candidate that was
    /// genuinely reclaim-eligible at examination time has its `origin`
    /// repointed at a DIFFERENT (attacker-controlled) repo slug before
    /// removal runs, and a `gh` stub answers a well-formed MERGED PR --
    /// matching branch, owner and `headRefOid` -- for that attacker slug
    /// only. `remove_isolated_clone_dir` must refuse when the candidate's
    /// derived slug no longer equals the root checkout's own, exactly as
    /// `isolated_clone_report` already does at examination time -- it must
    /// never query, let alone trust, a `gh` reply resolved against a repo
    /// slug the untrusted candidate's own `origin` chose. This exercises the
    /// intended fixed signature (`root_repo_slug: Option<&str>`, mirroring
    /// `isolated_clone_report`'s own `repo_slug: Option<&str>` parameter),
    /// which does not exist yet -- this is a compile-error RED, the same
    /// shape as `worktree_reclaim_081`'s `WorktreeReport.pinned` reference.
    #[spec("worktree/reclaim/082")]
    #[test]
    #[cfg(unix)]
    fn worktree_reclaim_082_removal_refuses_when_candidate_slug_no_longer_matches_root() {
        let _lock = GH_PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let scratch = tempfile::tempdir().unwrap();
        let repo = scratch.path().join("repo");
        init_repo_with_origin(&repo);
        let root_repo_slug = derive_repo_slug(&repo);
        assert_eq!(
            root_repo_slug.as_deref(),
            Some("test-org/test-repo"),
            "sanity: init_repo_with_origin's own fixture origin must derive to this slug"
        );

        let clone_dir = scratch.path().join("repo-isolated-slug-swapped");
        let creator = "issue-dispatch:slug-swapped#533";
        let outcome = crate::issue_dispatch_run::provision_isolated_clone_sync(
            &repo,
            &clone_dir,
            "slug-swapped-branch",
            creator,
        )
        .expect("provision_isolated_clone_sync must succeed against a real source repo");
        assert!(
            matches!(
                outcome,
                crate::issue_dispatch_run::IsolatedCloneOutcome::Created {
                    marker_warning: None,
                    ..
                }
            ),
            "the real provisioner must succeed and write the ownership marker with no warning, \
             got {outcome:?}"
        );

        let clone_head_sha = git_rev_parse_head(&clone_dir);

        // Confirm the clone is genuinely eligible at examination time, while
        // its `origin` still matches the root checkout's own slug -- proves
        // this is a real attack window, not a fixture that was already
        // ineligible when handed to the removal primitive.
        {
            let bindir = scratch.path().join("bin");
            write_merged_gh_stub_with_head_ref_oid(&bindir, "slug-swapped-branch", &clone_head_sha);
            let _path_guard = PathEnvGuard::prepend(&bindir);

            let reports = examine_worktrees(&repo).expect("examine_worktrees must succeed");
            let clone_report = reports
                .iter()
                .find(|r| r.real_path == clone_dir)
                .expect("the isolated clone must be present in the report");
            assert_eq!(
                clone_report.verdict.as_str(),
                VERDICT_ISOLATED_CLONE_RECLAIMABLE,
                "sanity: the clone must be reclaim-eligible at examination time, before its \
                 origin is swapped below, or this test would not be exercising the removal-time \
                 slug guard at all -- got {:?} (reason: {:?})",
                clone_report.verdict,
                clone_report.reason
            );
        }

        // Attack: a same-uid actor repoints the candidate's own `origin` at
        // a DIFFERENT, attacker-controlled repo slug between examination
        // and removal -- exactly the hazard `isolated_clone_report`'s own
        // slug-equality guard exists to reject at examination time (auditor
        // B3), reproduced here at removal time instead.
        let out = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args([
                "remote",
                "set-url",
                "origin",
                "https://github.com/attacker-org/evil-repo.git",
            ])
            .output()
            .unwrap_or_else(|e| panic!("git remote set-url failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git remote set-url failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let swapped_slug = derive_repo_slug(&clone_dir);
        assert_eq!(
            swapped_slug.as_deref(),
            Some("attacker-org/evil-repo"),
            "sanity: the candidate's own derived slug must now differ from the root checkout's"
        );

        // A `gh` stub answering a well-formed MERGED PR -- matching branch,
        // owner, AND headRefOid -- but only for the ATTACKER slug's owner.
        // If `remove_isolated_clone_dir` queried this at all, it would
        // resolve as merged-with-matching-head, i.e. exactly the shape that
        // would otherwise let removal proceed.
        let bindir = scratch.path().join("bin2");
        write_merged_gh_stub_with_owner_and_head_ref_oid(
            &bindir,
            "slug-swapped-branch",
            "attacker-org",
            &clone_head_sha,
        );
        let _path_guard = PathEnvGuard::prepend(&bindir);

        let result =
            remove_isolated_clone_dir(&clone_dir, "test-remover", root_repo_slug.as_deref());

        let err = result.expect_err(
            "remove_isolated_clone_dir must refuse when the candidate's own derived repo slug \
             no longer matches the root checkout's, exactly as isolated_clone_report already \
             does at examination time -- it must never resolve PR state against a repo the \
             untrusted candidate's own origin chose",
        );
        assert!(
            err.contains("repo slug"),
            "the refusal must fail at the slug guard, not some other guard, got: {err}"
        );
        assert!(
            clone_dir.exists(),
            "the candidate directory must be left on disk when the slug-mismatch refusal fires"
        );
        // Auditor's ordering-coverage suggestion (PR #686 fix round): the
        // guard's own message claims `gh` is never queried against the
        // attacker slug at all, not merely that the eventual result is
        // discarded. Prove it: the stub touches this marker on ANY
        // invocation, unconditionally, so its absence here means `gh` was
        // never spawned -- the guard genuinely runs BEFORE
        // `resolve_pr_state`, not merely before the result is trusted.
        assert!(
            !bindir.join(GH_INVOKED_MARKER_NAME).exists(),
            "gh must never be spawned at all when the slug guard refuses -- the marker's \
             presence would prove resolve_pr_state ran (and thus that the untrusted attacker \
             slug was queried) before the guard's refusal short-circuited it"
        );
    }
}
