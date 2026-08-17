//! `dot-agent-deck issue claim <n> [--repo <owner/name>] [--takeover]
//! [--confirm-stopped]` — PRD fork#235 M3.
//!
//! Turns PRD #421's issue claim from a record into a LOCK that refuses when
//! a different identity already holds an issue. Pure CLI-subprocess
//! operation over `git`/`gh`, synchronous like `worktree_reclaim`'s CLI
//! verbs (rule 12: no daemon/protocol involvement — identity resolves
//! entirely from local signals: `DOT_AGENT_DECK_PANE_ID`, the worktree's own
//! absolute path and branch, and `gh api user`). Round 3
//! (`prds/235-issue-claim-lock.md`) reads NO ownership marker at all — see
//! [`resolve_caller_identity`].
//!
//! Decision table (see [`decide_claim`] for the pure form):
//!
//! | State | Flags | Result |
//! |---|---|---|
//! | Unlabelled | — | claim |
//! | Held by THIS identity | — | idempotent refresh (claim again) |
//! | Held by a DIFFERENT identity | — | refuse |
//! | Labelled, no claim comment | — | refuse (identity unknown) |
//! | Held by a different identity | `--takeover` | still refuses |
//! | Held by a different identity | `--takeover --confirm-stopped` | claim, recording the takeover |
//!
//! The exit code is the mechanism — [`run_issue_claim`] returns `Err` for
//! every refusal AND every operational failure alike, so the CLI wrapper in
//! `main.rs` can map both uniformly to a non-zero exit; the two are
//! distinguished only by the message text.

use std::path::Path;
use std::process::Command;

use crate::issue_dispatch::{
    IN_PROGRESS_LABEL, Identity, ParsedClaim, claim_comment_body, gh_current_login_argv,
    issue_comment_argv, issue_edit_add_label_argv, issue_edit_assignee_argv,
    issue_view_claim_state_argv, parse_claim_state, sanitize_claimant_name, validate_gh_login,
};

// ---------------------------------------------------------------------------
// Pure decision (unit-testable independent of any subprocess)
// ---------------------------------------------------------------------------

/// The pure lock decision (PRD fork#235 M3), driven only by whether
/// `in-progress` is present, who (if anyone) the newest claim comment names,
/// and the caller's own composed identity string. Comparison is on the WHOLE
/// identity string, never a component of it — see [`Identity`]'s doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimDecision {
    /// Claim (or re-claim). `takeover_from` names the identity being
    /// displaced, `Some` only for a genuine takeover of a DIFFERENT holder —
    /// `None` for an unlabelled issue or an idempotent refresh of the
    /// caller's own claim.
    Claim { takeover_from: Option<String> },
    /// The issue is labelled `in-progress` but no claim comment names a
    /// holder — a hand-typed claim (CLAUDE.md rule 14) applied outside any
    /// deck flow, OR a prior claim whose comment write failed after its
    /// label write already landed (`do_claim` writes comment-before-label
    /// specifically to make this state rarer, but a transient failure on
    /// the FIRST write can still produce it). Identity unknown; refuse
    /// rather than guess — unless escaped via `--takeover --confirm-stopped`
    /// (reviewer/auditor F4, `issue/claim/013`): the SAME two-step override
    /// that recovers a known-holder refusal, so one transient failure can
    /// never permanently wedge the issue for every future claim attempt.
    RefuseNoIdentity,
    /// Held by a different identity. `takeover_requested` distinguishes the
    /// two REFUSE rows that differ only in message: a bare refusal vs.
    /// `--takeover` alone (deliberate friction — an agent must not be able
    /// to satisfy the override in the same breath it discovers the
    /// conflict).
    RefuseHeldByOther {
        holder: String,
        takeover_requested: bool,
    },
}

/// The pure decision function backing [`ClaimDecision`]'s doc table.
pub fn decide_claim(
    label_present: bool,
    held: Option<&ParsedClaim>,
    caller_identity: &str,
    takeover: bool,
    confirm_stopped: bool,
) -> ClaimDecision {
    if !label_present {
        return ClaimDecision::Claim {
            takeover_from: None,
        };
    }
    let Some(held) = held else {
        // Escapable with the SAME two-step override as a known-holder
        // refusal — see `RefuseNoIdentity`'s own doc. Nothing to name as
        // `takeover_from`: there is no known prior holder to attribute the
        // takeover to.
        return if takeover && confirm_stopped {
            ClaimDecision::Claim {
                takeover_from: None,
            }
        } else {
            ClaimDecision::RefuseNoIdentity
        };
    };
    if held.identity == caller_identity {
        return ClaimDecision::Claim {
            takeover_from: None,
        };
    }
    // Auditor F3: `held.identity` is reconstructed from fields PARSED out of
    // a comment this deck does not control the authorship of (this PRD's
    // Threat model places forgery of a comment's AUTHOR out of scope, but
    // not corruption of what the PARSER hands back). Sanitize before it is
    // echoed into a NEW comment's `taking over from …` tail or a refusal
    // message printed to the operator's terminal — the comparison just
    // above already happened against the raw value, so this cannot change
    // the decision itself, only what gets displayed.
    let holder = sanitize_claimant_name(&held.identity);
    if takeover && confirm_stopped {
        return ClaimDecision::Claim {
            takeover_from: Some(holder),
        };
    }
    ClaimDecision::RefuseHeldByOther {
        holder,
        takeover_requested: takeover,
    }
}

// ---------------------------------------------------------------------------
// Caller identity resolution
// ---------------------------------------------------------------------------

/// Resolve the caller's own [`Identity`] from local signals only (rule 12 —
/// no daemon lookup). Two local signals decide the shape (PRD fork#235 round
/// 3, `prds/235-issue-claim-lock.md`'s "Identity, round 2" section):
///
/// | `DOT_AGENT_DECK_PANE_ID` | Linked worktree? | Identity |
/// |---|---|---|
/// | absent | — | `human:<login>@<host>` |
/// | present (any value, even blank) | yes | worktree path + branch |
/// | present (any value, even blank) | no | refuse |
///
/// The third row is the one to get right: an agent-shaped caller (the env
/// var present at all, regardless of its actual value — `issue/claim/006`
/// pins that even a blank value counts as present) whose `cwd` is NOT a
/// genuinely linked worktree — the main/root checkout, or any other
/// non-worktree directory — must never fall back to `human:<login>`. Round
/// 3 reads NO ownership marker at all (unlike round 1/2): the anchor is the
/// worktree itself, so [`crate::worktree_reclaim::owned_git_dir`]'s
/// containment check (does `cwd` even resolve as a linked worktree?) is the
/// only thing consulted — a marker-less but genuinely linked worktree
/// claims successfully (`issue/claim/009`, the orchestrator's own dominant
/// real path under CLAUDE.md rule 1's hand-made `git worktree add` flow).
/// Falling back to `human` here would collapse every agent on one deck to
/// the SAME identity and the lock would wave them all through while
/// appearing to work.
///
/// `pub(crate)`: the only callers are [`resolve_remover_identity`] (same
/// module) and `issue_claim`'s own claim-lock entry point. `main.rs`'s
/// `run_worktree_reclaim_cli` calls `resolve_remover_identity` instead
/// (issue #325 / reviewer NEW-3 / auditor), which stays `pub` because it —
/// not this function — is the one crossing the binary/library crate
/// boundary; `pub(crate)` here would not reach `main.rs` either, but nothing
/// there needs it to. Keeping the lock resolver un-exported is also the
/// cheapest way to enforce the warning below: a function nothing outside
/// this module can even name is a function nothing outside this module can
/// misuse.
pub(crate) fn resolve_caller_identity(cwd: &Path) -> Result<Identity, String> {
    match std::env::var(crate::agent_pty::DOT_AGENT_DECK_PANE_ID) {
        Err(_) => {
            // No pane env: a human terminal. The login IS part of the
            // identity here, so — unlike the worktree branch below —
            // failing to resolve it means we have no valid identity at all.
            let host = crate::issue_dispatch_run::local_hostname();
            let login = resolve_gh_login()?;
            Ok(Identity::human(&login, &host))
        }
        Ok(_) => {
            if crate::worktree_reclaim::owned_git_dir(cwd, cwd).is_none() {
                return Err(format!(
                    "DOT_AGENT_DECK_PANE_ID is set but {} is not a linked worktree — refusing \
                     rather than assuming a human caller; round 3's identity anchor is the \
                     worktree's own absolute path and branch (CLAUDE.md rule 23), and this \
                     directory carries none (a `human:<login>` fallback here would collapse \
                     every agent on this deck to the SAME identity and the lock would wave \
                     them all through)",
                    cwd.display()
                ));
            }
            // Round-3 audit R1/A5 (`issue/claim/017`): anchor on the
            // worktree ROOT, never `cwd` verbatim — otherwise a claim from
            // `<wt>/src` resolves a DIFFERENT identity than one from `<wt>`
            // itself, splitting one actor into as many identities as it has
            // subdirectories, contradicting CLAUDE.md rule 1's "every pane
            // in one worktree shares that worktree's identity".
            let root = resolve_worktree_root(cwd)?;
            let branch = resolve_branch(&root)?;
            Ok(Identity::worktree(&root, &branch))
        }
    }
}

/// Diagnostic-only identity resolution for issue #325's `worktree reclaim`
/// attribution (reviewer P2-1 / auditor F2's disposition) — deliberately
/// NOT [`resolve_caller_identity`], and never call sites of the two
/// interchangeably. That function is a LOCK identity: refusing (rather than
/// guessing) when a pane-shaped caller's `cwd` is not a linked worktree is
/// the whole point, because a `human:<login>` fallback there would collapse
/// every agent on the deck onto one identity and wave them all through
/// `issue_claim`'s claim check. `worktree reclaim` has no such lock to
/// protect, and the refused case is its DOMINANT one — `run_reclaim`
/// enumerates the whole repo's worktrees, so the natural place to run it is
/// the root checkout, which is never itself a linked worktree. Recording
/// `"unknown"` for exactly that case would degrade the one scenario issue
/// #325 was filed over back to the status quo it exists to fix.
///
/// So this always returns something, never fails: on [`resolve_caller_identity`]'s
/// `Err`, it falls back to whatever local signal IS available rather than a
/// bare constant — `DOT_AGENT_DECK_PANE_ID` plus the hostname and `cwd`
/// (an agent-shaped caller outside a linked worktree, the refused case
/// above) if a pane id is set, or the bare `"unknown"` sentinel only when
/// even that is absent (a human caller whose `gh api user` lookup failed —
/// there is no local signal left to name them by). This is diagnostic data
/// for a human reading `DOT_AGENT_DECK_LOG` after the fact, not a value
/// anything gates on, so collapsing two distinct agents onto the same
/// `pane:<id>@<host>` string costs nothing here the way it would for the
/// lock.
pub fn resolve_remover_identity(cwd: &Path) -> String {
    match resolve_caller_identity(cwd) {
        Ok(identity) => identity.to_string(),
        Err(_) => match std::env::var(crate::agent_pty::DOT_AGENT_DECK_PANE_ID) {
            Ok(pane_id) => {
                let host = crate::issue_dispatch_run::local_hostname();
                format!("pane:{pane_id}@{host} (cwd {})", cwd.display())
            }
            Err(_) => "unknown".to_string(),
        },
    }
}

/// Resolve `cwd`'s worktree ROOT via `git rev-parse --show-toplevel` — the
/// other half of round 3's identity anchor alongside [`resolve_branch`].
/// Works identically from the worktree root or any subdirectory of it (git
/// resolves the toplevel relative to the repo, not the caller's own `cwd`),
/// which is exactly why anchoring on this rather than `cwd` verbatim fixes
/// `issue/claim/017`.
fn resolve_worktree_root(cwd: &Path) -> Result<std::path::PathBuf, String> {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("failed to run `git rev-parse --show-toplevel`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git rev-parse --show-toplevel` failed in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root.is_empty() {
        return Err(format!(
            "`git rev-parse --show-toplevel` returned empty output in {}",
            cwd.display()
        ));
    }
    Ok(std::path::PathBuf::from(root))
}

/// Resolve `cwd`'s current branch via `git rev-parse --abbrev-ref HEAD` — the
/// other half of round 3's identity anchor alongside `cwd` itself. Fails
/// closed on a detached HEAD (`HEAD` is exactly what `--abbrev-ref` prints
/// when there is no branch to name) — the anchor needs a real branch name,
/// and rule 1's worktrees are always created with `-b <branch>`.
fn resolve_branch(cwd: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| format!("failed to run `git rev-parse --abbrev-ref HEAD`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git rev-parse --abbrev-ref HEAD` failed in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err(format!(
            "{} has no resolvable branch (detached HEAD?) — round 3's identity anchor needs a \
             real branch name",
            cwd.display()
        ));
    }
    Ok(branch)
}

/// Resolve the human login to write as the assignee (PRD fork#235 M2's
/// semantics, reused here): already known for a [`Identity::Human`] caller
/// (it IS the identity); resolved best-effort via `gh api user` for an
/// orchestration caller, mirroring the async dispatch path's discipline — a
/// failure here must never fail the claim, only leave it unassigned.
fn resolve_assignee_login(identity: &Identity) -> Option<String> {
    match identity {
        Identity::Human { login, .. } => Some(login.clone()),
        _ => resolve_gh_login().ok(),
    }
}

/// `pub(crate)`, not private: PRD fork#298 M1.0's `worktree_reclaim::resolve_human_owner`
/// reuses this rather than duplicating the `gh api user` capture logic, so
/// the two subsystems' "no marker → resolve a human caller" paths share one
/// implementation instead of two that could silently drift apart.
pub(crate) fn resolve_gh_login() -> Result<String, String> {
    let out = run_gh_capture(&gh_current_login_argv())?;
    let login = out.trim();
    if login.is_empty() {
        return Err("`gh api user` returned an empty login".to_string());
    }
    if !validate_gh_login(login) {
        return Err(format!(
            "`gh api user` returned a login that fails validation: {login:?}"
        ));
    }
    Ok(login.to_string())
}

// ---------------------------------------------------------------------------
// Subprocess helpers
// ---------------------------------------------------------------------------

fn run_gh_capture(args: &[String]) -> Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = Command::new("gh")
        .args(&refs)
        .output()
        .map_err(|e| format!("failed to run `gh`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`gh {}` failed: {}",
            refs.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_gh_status(args: &[String]) -> Result<(), String> {
    run_gh_capture(args).map(|_| ())
}

fn read_current_claim(
    repo: &str,
    issue: u64,
) -> Result<(bool, Option<ParsedClaim>, Vec<String>), String> {
    let json = run_gh_capture(&issue_view_claim_state_argv(repo, issue))?;
    parse_claim_state(&json)
}

// ---------------------------------------------------------------------------
// The write path
// ---------------------------------------------------------------------------

/// Post the claim comment, write the label, then replace-to-one the
/// assignee (best-effort) — reviewer/auditor F4: comment-FIRST,
/// label-SECOND, the inverse of PRD fork#235 M2's async `claim_issue`
/// order. A failed comment write (the first, required call) simply leaves
/// the issue unlabelled — harmless, since `decide_claim` only reads `held`
/// when `label_present` is true (`decide_claim_unlabelled_always_claims`).
/// The OLD order (label-then-comment) could instead leave the issue
/// LABELLED with no discoverable comment if the SECOND, comment call
/// failed — exactly `RefuseNoIdentity`'s wedge state, now also escapable
/// (see that variant's doc) but better avoided than merely recovered from.
fn do_claim(
    repo: &str,
    issue: u64,
    identity: &Identity,
    login: Option<&str>,
    remove: &[String],
    takeover_from: Option<&str>,
) -> Result<(), String> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let body = claim_comment_body(identity, &timestamp, login, takeover_from);
    run_gh_status(&issue_comment_argv(repo, issue, &body))?;

    run_gh_status(&issue_edit_add_label_argv(repo, issue, IN_PROGRESS_LABEL))?;

    if let Some(login) = login {
        // `remove` is already `current assignees − {login}` (PRD fork#235
        // FINAL round 5) — a same-identity refresh's own login is excluded
        // by the set difference itself, so reviewer F3's self-cancelling
        // `--add-assignee X --remove-assignee X` pair (`issue/claim/011`)
        // cannot arise here; no special-case guard needed.
        let assignee_argv = issue_edit_assignee_argv(repo, issue, Some(login), remove);
        // Best-effort (PRD fork#235 M2's discipline): GitHub silently drops
        // an assignee lacking repo access and `gh` may still exit 0, so a
        // genuine `gh` failure here is surfaced but must not undo the claim
        // that already succeeded.
        if let Err(e) = run_gh_status(&assignee_argv) {
            eprintln!("issue claim: warning: failed to write the assignee: {e}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run `issue claim <issue>` against `repo` (or, when `None`, the
/// `owner/name` derived from `cwd`'s `origin` remote). `Ok(message)` is the
/// success text for stdout (exit 0); `Err(message)` covers BOTH a refusal
/// and an operational failure (exit non-zero) — the exit code is the
/// mechanism an agent's shell notices; which of the two occurred is
/// distinguishable only by reading the message.
pub fn run_issue_claim(
    cwd: &Path,
    repo: Option<&str>,
    issue: u64,
    takeover: bool,
    confirm_stopped: bool,
) -> Result<String, String> {
    let repo = match repo {
        Some(r) => r.to_string(),
        None => crate::worktree_reclaim::derive_repo_slug(cwd).ok_or_else(|| {
            format!(
                "could not derive an `owner/name` repo slug from {}'s `origin` remote; pass \
                 --repo explicitly",
                cwd.display()
            )
        })?,
    };

    let identity = resolve_caller_identity(cwd)?;
    let (label_present, held, current_assignees) = read_current_claim(&repo, issue)?;

    match decide_claim(
        label_present,
        held.as_ref(),
        &identity.to_string(),
        takeover,
        confirm_stopped,
    ) {
        ClaimDecision::Claim { takeover_from } => {
            let login = resolve_assignee_login(&identity);
            // PRD fork#235 FINAL round 5: the removal target is `current
            // GitHub assignees − {claimant}`, read from `gh issue view`'s own
            // `assignees` field — never from any claim comment's content or
            // authorship (the round-4 author gate this superseded did not
            // narrow that removal, it disabled it; see the PRD's "FINAL
            // (round 5)" section). A same-identity refresh is handled by the
            // set difference itself: `login` is excluded from `remove`
            // structurally, with no special-casing required.
            let remove: Vec<String> = match login.as_deref() {
                Some(l) => current_assignees.into_iter().filter(|a| a != l).collect(),
                None => Vec::new(),
            };
            do_claim(
                &repo,
                issue,
                &identity,
                login.as_deref(),
                &remove,
                takeover_from.as_deref(),
            )?;
            Ok(format!(
                "claimed issue #{issue} of {repo} as `{identity}`\n"
            ))
        }
        ClaimDecision::RefuseNoIdentity => Err(format!(
            "issue #{issue} of {repo} is labelled `{IN_PROGRESS_LABEL}` but no claim comment \
             names a holder — refusing (identity unknown); this is likely a hand-typed claim \
             applied outside `dot-agent-deck issue claim`"
        )),
        ClaimDecision::RefuseHeldByOther {
            holder,
            takeover_requested,
        } => {
            // Auditor A4 (`issue/claim/021`): `holder` above is already
            // sanitized via `sanitize_claimant_name` (in `decide_claim`),
            // but `timestamp` is a SIBLING field parsed from the same
            // untrusted comment and reaches this same operator-facing
            // refusal message — sanitize it too, rather than re-deciding
            // per field what the parser boundary (item 1) already settles.
            let since = held
                .as_ref()
                .map(|h| format!(" since {}", sanitize_claimant_name(&h.timestamp)))
                .unwrap_or_default();
            let instruction = if takeover_requested {
                "`--takeover` alone does not release it — this is deliberate friction, so an \
                 agent can't satisfy the override in the same breath it discovers the \
                 conflict; re-run with `--takeover --confirm-stopped` once you have confirmed \
                 the other agent has stopped"
            } else {
                "pass `--takeover --confirm-stopped` once you have confirmed the other agent \
                 has stopped"
            };
            // Auditor F7.3: when the holder is an `issue_dispatch`-fired
            // claim, say so plainly — the most common way to hit this
            // refusal is running `issue claim` by hand inside a worktree the
            // dispatch that spawned you already claimed automatically
            // (`claim_issue`, `issue_dispatch_run.rs`), which needs no
            // manual claim at all. This is explanatory text only — it must
            // NEVER make the two compare equal (that would defeat the lock);
            // the decision above has already run against the raw identity.
            let dispatch_note = held
                .as_ref()
                .filter(|h| h.raw.contains("issue-dispatch task"))
                .map(|_| {
                    " — this looks like an `issue_dispatch` fire's own automatic claim; if you \
                     were spawned BY that dispatch, it already claimed the issue for you and you \
                     do not need to run `issue claim` yourself"
                })
                .unwrap_or_default();
            Err(format!(
                "issue #{issue} of {repo} is held by `{holder}`{since} — {instruction}{dispatch_note}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(identity: &str, login: Option<&str>, timestamp: &str) -> ParsedClaim {
        ParsedClaim {
            identity: identity.to_string(),
            timestamp: timestamp.to_string(),
            login: login.map(str::to_string),
            raw: String::new(),
        }
    }

    #[test]
    fn decide_claim_unlabelled_always_claims() {
        assert_eq!(
            decide_claim(false, None, "orchestration:a@h:1", false, false),
            ClaimDecision::Claim {
                takeover_from: None
            }
        );
        // Even with a stray held comment present, unlabelled wins.
        let held = claim("orchestration:b@h:2", Some("bob"), "ts");
        assert_eq!(
            decide_claim(false, Some(&held), "orchestration:a@h:1", false, false),
            ClaimDecision::Claim {
                takeover_from: None
            }
        );
    }

    #[test]
    fn decide_claim_labelled_no_comment_refuses_identity_unknown() {
        assert_eq!(
            decide_claim(true, None, "orchestration:a@h:1", false, false),
            ClaimDecision::RefuseNoIdentity
        );
    }

    #[test]
    fn decide_claim_held_by_self_is_idempotent_refresh() {
        let held = claim("orchestration:a@h:1", Some("alice"), "ts");
        assert_eq!(
            decide_claim(true, Some(&held), "orchestration:a@h:1", false, false),
            ClaimDecision::Claim {
                takeover_from: None
            }
        );
    }

    #[test]
    fn decide_claim_held_by_other_no_flags_refuses() {
        let held = claim("orchestration:a@h:1", Some("alice"), "ts");
        assert_eq!(
            decide_claim(true, Some(&held), "orchestration:b@h:2", false, false),
            ClaimDecision::RefuseHeldByOther {
                holder: "orchestration:a@h:1".to_string(),
                takeover_requested: false,
            }
        );
    }

    #[test]
    fn decide_claim_takeover_alone_still_refuses() {
        let held = claim("orchestration:a@h:1", Some("alice"), "ts");
        assert_eq!(
            decide_claim(true, Some(&held), "orchestration:b@h:2", true, false),
            ClaimDecision::RefuseHeldByOther {
                holder: "orchestration:a@h:1".to_string(),
                takeover_requested: true,
            }
        );
    }

    #[test]
    fn decide_claim_takeover_and_confirm_stopped_claims_with_provenance() {
        let held = claim("orchestration:a@h:1", Some("alice"), "ts");
        assert_eq!(
            decide_claim(true, Some(&held), "orchestration:b@h:2", true, true),
            ClaimDecision::Claim {
                takeover_from: Some("orchestration:a@h:1".to_string())
            }
        );
    }

    // --- PRD fork#235 round 3 / reviewer-auditor F4: RefuseNoIdentity escape ---

    #[test]
    fn decide_claim_no_identity_takeover_alone_still_refuses() {
        assert_eq!(
            decide_claim(true, None, "orchestration:a@h:1", true, false),
            ClaimDecision::RefuseNoIdentity
        );
    }

    #[test]
    fn decide_claim_no_identity_escapes_with_takeover_and_confirm_stopped() {
        assert_eq!(
            decide_claim(true, None, "orchestration:a@h:1", true, true),
            ClaimDecision::Claim {
                takeover_from: None
            }
        );
    }

    // --- auditor F3: a held identity's display is sanitized before reuse ---

    #[test]
    fn decide_claim_sanitizes_held_identity_before_displaying_it() {
        let held = claim("worktree:/work/evil\ninjected@branch", Some("alice"), "ts");
        match decide_claim(true, Some(&held), "orchestration:b@h:2", false, false) {
            ClaimDecision::RefuseHeldByOther { holder, .. } => {
                assert!(
                    !holder.contains('\n'),
                    "a raw newline parsed out of a held claim comment must not survive into a \
                     new refusal message/comment tail, got {holder:?}"
                );
            }
            other => panic!("expected RefuseHeldByOther, got {other:?}"),
        }
    }

    // --- issue #325 / reviewer NEW-2 / auditor: pin resolve_remover_identity's
    // fallback branch, so reverting main.rs's call site back to the bare
    // resolve_caller_identity().unwrap_or_else(|_| "unknown") cannot pass
    // silently. `std::env::set_var`/`remove_var` are process-global, so
    // serialize against PANE_ID_ENV_LOCK the same way agent_pty.rs's
    // ENV_TEST_LOCK does.
    static PANE_ID_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_remover_identity_falls_back_to_pane_id_outside_a_worktree() {
        let _g = PANE_ID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // SAFETY: serialized by PANE_ID_ENV_LOCK; prior value is restored
        // before the lock is released.
        let prior = std::env::var(crate::agent_pty::DOT_AGENT_DECK_PANE_ID).ok();
        unsafe {
            std::env::set_var(crate::agent_pty::DOT_AGENT_DECK_PANE_ID, "7");
        }

        // A bare tempdir is not a linked worktree, so `owned_git_dir` returns
        // `None` and `resolve_caller_identity` takes its `Err(_)` branch —
        // exactly the root-checkout shape issue #325 was filed over.
        let scratch = tempfile::tempdir().unwrap();
        let result = resolve_remover_identity(scratch.path());

        unsafe {
            match prior {
                Some(v) => std::env::set_var(crate::agent_pty::DOT_AGENT_DECK_PANE_ID, v),
                None => std::env::remove_var(crate::agent_pty::DOT_AGENT_DECK_PANE_ID),
            }
        }

        assert!(
            result.starts_with("pane:7@"),
            "expected the pane-id fallback (not the bare \"unknown\" degradation this test \
             exists to catch a regression to), got {result:?}"
        );
        assert!(
            result.contains(&scratch.path().display().to_string()),
            "fallback string should carry cwd for post-incident diagnosis, got {result:?}"
        );
    }
}
