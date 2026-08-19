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
pub const SCHEMA_VERSION: u32 = 3;

/// The name of the marker file that proves the deck created a worktree. Lives
/// in the worktree's OWN git metadata dir (`<repo>/.git/worktrees/<name>/`,
/// found via `git rev-parse --git-dir` run inside the worktree) — outside the
/// working tree, so it never makes `git status --porcelain` report the
/// worktree dirty, and it is removed automatically by `git worktree remove`.
/// Written by [`mark_worktree_owned`] at worktree-creation time
/// (`issue_dispatch_run::create_worktree` / `create_worktree_sync`), read by
/// [`ownership_of`].
pub const OWNER_MARKER_FILENAME: &str = "dot-agent-deck-owner";

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
     the deck's own attach-lock artifact for this candidate does exist, so this is treated the \
     same as a linked worktree's legacy marker (fork#325 M4a, auditor A4)";

/// A marker file exists for this candidate, but no matching attach-lock
/// artifact does (fork#325 M4a, final round -- auditor A1/B1). Either the
/// deck never attached this clone (a hand-planted or forged marker,
/// auditor B1's exact scenario), or it genuinely was deck-created by a
/// build predating this check, or was moved/renamed since (the lock's
/// filename hashes the canonical path). Discovery still lists the row
/// (structural criteria alone gate inclusion -- see
/// `discover_isolated_clones`'s own doc comment), but the marker's content
/// is deliberately never read in this case: without the attach lock there
/// is nothing to trust it against, so treating an unread marker as
/// evidence would reopen exactly the misattribution auditor B1
/// demonstrated.
const ISOLATED_CLONE_NO_ATTACH_LOCK_REASON: &str = "no matching attach-lock artifact exists for this candidate under the root checkout's own \
     .git -- its ownership marker (if any) is not read, since without that artifact there is \
     nothing to trust the marker's content against (fork#325 M4a, auditor A1/B1)";

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
    /// [`decide`]) for a `kind == "linked"` row, or the literal
    /// `"isolated_clone"` (from [`isolated_clone_report`], never through
    /// [`decide`] at all) for a `kind == "isolated_clone"` row — see
    /// [`SCHEMA_VERSION`]'s own doc comment (reviewer F2) for why this
    /// fourth value forced the bump to `SCHEMA_VERSION` 3. Two independent
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

/// Build a `PathBuf` from raw bytes read from `git`'s output (a `-z` path
/// field, or a `rev-parse --git-dir` line) without a lossy UTF-8 round-trip.
/// On Unix a path is an arbitrary byte sequence, so this goes straight
/// through `OsStr`; elsewhere (Windows paths are UTF-16, and `git` there
/// emits UTF-8 on the wire) a lossy fallback is the best available.
#[cfg(unix)]
fn path_from_bytes(field: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(field))
}

/// Strip a single trailing `\n` (or `\r\n`) from a `git` command's raw
/// stdout, at the byte level — no UTF-8 round-trip, so the bytes that
/// precede the line ending survive untouched regardless of what they are.
fn trim_trailing_newline(bytes: &[u8]) -> &[u8] {
    bytes
        .strip_suffix(b"\n")
        .map(|b| b.strip_suffix(b"\r").unwrap_or(b))
        .unwrap_or(bytes)
}

#[cfg(not(unix))]
fn path_from_bytes(field: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(field).into_owned())
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
/// (fork#325 M4a, auditor A3): `-c core.fsmonitor=` is a no-op for a linked
/// worktree (already reachable only via `git worktree list`, never a
/// directory an outside party could plant), and is the exact vector the
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

/// Whether a persistent attach-lock artifact exists for `candidate`, keyed
/// by its own canonical path under the enumerating repo's common `.git` dir
/// (fork#325 M4a, final round — reviewer F13 / auditor A1/B1, replacing
/// the earlier `candidate_shares_history_with`, since removed).
///
/// `provision_isolated_clone_sync` already writes this exact artifact
/// before it ever clones: `worktree_attach_lock_path(source_dir, clone_dir)`
/// resolves under `<root checkout's common dir>/dot-agent-deck-worktree-locks/`,
/// created owner-only (0700/0600) and never removed on the acquire path
/// (`src/issue_dispatch_run.rs`). This function recomputes that exact path
/// via the SAME hash the provisioner uses
/// ([`crate::issue_dispatch_run::worktree_attach_lock_path_from_common_dir`])
/// and checks for its existence — nothing here reimplements the hashing.
///
/// This is a stronger binding than the shared-history check it replaces,
/// for the same reason [`owned_git_dir`]'s containment check is strong for
/// a linked worktree: the evidence lives in a location the ENUMERATING
/// party controls (`repo_dir`'s own `.git`), not one the candidate itself
/// can write to. A `same-uid` attacker able only to plant a sibling
/// directory cannot forge this file — closing auditor B1's 4-file/1-SHA
/// forgery, which the shared-history check could not (it only checked that
/// the candidate NAMED a commit `repo_dir` had, never that it HELD one).
/// And unlike the shared-history check, this has no dependency on the
/// candidate's current `HEAD` at all — a genuine clone that has since
/// committed real, local-only work is still recognized, closing reviewer
/// F13.
///
/// Honest limits, stated plainly rather than overclaimed (the exact
/// overclaim auditor B1 flagged in the doc comment this replaces):
/// - **Same-uid still wins.** An attacker with write access to `repo_dir`'s
///   own `.git` defeats this exactly as it defeats `owned_git_dir` — this
///   is categorically as strong as the linked-worktree case, not stronger,
///   and never absolute.
/// - **Best-effort provenance, not proof of current identity.** A clone
///   made by a build predating this check, or moved/renamed after creation
///   (the hash is over the canonical path), has no matching lock file and
///   correctly reports `owned: false` rather than being hidden — mirroring
///   how `owned_git_dir` returning `None` yields `owned: false`, never a
///   dropped row.
/// - **Stale entries.** The lock file is never removed when a clone is
///   later deleted, so it would vouch for an unrelated directory recreated
///   at the same path afterward — weaker than containment in that one
///   respect.
fn candidate_has_attach_lock(common_dir: &Path, candidate: &Path) -> bool {
    crate::issue_dispatch_run::worktree_attach_lock_path_from_common_dir(common_dir, candidate)
        .is_file()
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
/// via `worktree list --mine` the way auditor B1 demonstrated. Nothing on
/// this path can ever reach a deletion regardless (see
/// [`isolated_clone_report`]'s own doc comment and [`run_reclaim`]'s
/// exhaustive match trace), so a bogus row surviving discovery is a display
/// nuisance, never a safety issue.
///
/// Auditor A5 (not fixed here, deliberately): per-sibling work is uncapped.
/// [`isolated_clone_report`] spawns 2-4 `git`/`gh` processes per accepted
/// clone (branch, clean, PR state, and `gh pr list`'s network round trip
/// when a branch resolves) — this function itself no longer spawns
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
        // doc comment for why it no longer gates inclusion.
        let has_attach_lock = candidate_has_attach_lock(&common_dir, &path);
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
/// Final round (reviewer F13 / auditor A1/B1): `owned` and `owner`/
/// `owner_kind` below are now backed by `candidate.has_attach_lock` —
/// whether [`candidate_has_attach_lock`] found the deck's own attach-lock
/// artifact for this candidate under `repo_dir`'s own `.git` — resolved
/// once in [`discover_isolated_clones`] and carried on
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
        verdict: KIND_ISOLATED_CLONE.to_string(),
        reason: Some(
            "isolated clone: automatic reclaim is deferred to a documented follow-up \
             milestone -- never auto-removed by `worktree reclaim`, with or without --yes, \
             regardless of PR state or cleanliness (mirrors RemovalPolicy::IsolatedClone's \
             daemon-side precedent; fork#325 M4a)"
                .to_string(),
        ),
        owner,
        owner_kind,
        owner_reason,
        real_path,
        removed_by: None,
        kind: KIND_ISOLATED_CLONE.to_string(),
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
/// raw subprocess stderr) -- so each is routed through
/// [`sanitize_for_terminal_display`] / [`sanitize_path_for_terminal_display`]
/// (issue #232) before it reaches this TAB-separated row: an unescaped raw
/// TAB in any of them would also forge a column boundary and shift every
/// later cell. `PR`, `CLEAN`, `OWNED` and `VERDICT` are internal
/// enum/boolean labels this crate produces itself, never attacker content,
/// so they are not sanitized.
pub fn format_list_human(reports: &[WorktreeReport]) -> String {
    if reports.is_empty() {
        return "no worktrees found\n".to_string();
    }
    let mut out = String::new();
    out.push_str("PATH\tBRANCH\tPR\tCLEAN\tOWNED\tOWNER\tVERDICT\tREASON\n");
    for r in reports {
        let path = sanitize_path_for_terminal_display(&r.path);
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
/// receives still does: [`run_reclaim`]'s match sends `kind ==
/// "isolated_clone"` rows to its `kept` arm unconditionally and never calls
/// this function for them (see [`isolated_clone_report`]'s own doc
/// comment), so the conclusion holds even though its old justification —
/// "every path here" — no longer describes every row this crate examines.
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
            out.push_str(&format!(
                "  - {}\n",
                sanitize_path_for_terminal_display(&r.path)
            ));
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
                    sanitize_path_for_terminal_display(&r.path),
                    sanitize_for_terminal_display(remover)
                )),
                None => out.push_str(&format!(
                    "  - {}\n",
                    sanitize_path_for_terminal_display(&r.path)
                )),
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
                sanitize_path_for_terminal_display(&r.path),
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
            kind: KIND_LINKED.to_string(),
            real_path: PathBuf::from("/repo/wt-a"),
            removed_by: None,
        }];
        let json = serde_json::to_string(&WorktreeListDocument::new(reports)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["schema_version"], 3);
        assert!(json.contains("wt-a"));
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
    #[spec("worktree/reclaim/024")]
    #[test]
    fn worktree_reclaim_024_same_config_type_same_directory_records_distinct_owners() {
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
            .strip_prefix("  - /repo/wt-normal (removed by ")
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
    /// removed by `worktree reclaim`, with or without `--yes`. This is
    /// M4a's deliberate, conservative stopping point: whether an isolated
    /// clone ever becomes safely auto-reclaimable (and under what stricter
    /// condition) is left to a documented follow-up, not implemented here.
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
        assert!(
            crate::issue_dispatch_run::worktree_attach_lock_path_from_common_dir(
                &resolve_common_dir(&repo).expect("repo must resolve a common dir"),
                &clone_dir,
            )
            .is_file(),
            "sanity: the real provisioner must have left a real attach-lock artifact on disk"
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
    /// able only to plant a sibling directory cannot write into the root
    /// checkout's own `.git`. `examine_worktrees` must still discover it
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
}
