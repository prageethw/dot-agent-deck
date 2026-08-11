//! `dot-agent-deck issue claim <n> [--repo <owner/name>] [--takeover]
//! [--confirm-stopped]` — PRD fork#235 M3.
//!
//! Turns PRD #421's issue claim from a record into a LOCK that refuses when
//! a different identity already holds an issue. Pure CLI-subprocess
//! operation over `git`/`gh`, synchronous like `worktree_reclaim`'s CLI
//! verbs (rule 12: no daemon/protocol involvement — identity resolves
//! entirely from local signals: `DOT_AGENT_DECK_PANE_ID`, the worktree's own
//! ownership marker, and `gh api user`).
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
    issue_view_claim_state_argv, parse_claim_state, validate_gh_login,
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
    /// deck flow. Identity unknown; refuse rather than guess.
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
        return ClaimDecision::RefuseNoIdentity;
    };
    if held.identity == caller_identity {
        return ClaimDecision::Claim {
            takeover_from: None,
        };
    }
    if takeover && confirm_stopped {
        return ClaimDecision::Claim {
            takeover_from: Some(held.identity.clone()),
        };
    }
    ClaimDecision::RefuseHeldByOther {
        holder: held.identity.clone(),
        takeover_requested: takeover,
    }
}

// ---------------------------------------------------------------------------
// Caller identity resolution
// ---------------------------------------------------------------------------

/// Resolve the caller's own [`Identity`] from local signals only (rule 12 —
/// no daemon lookup). Two local signals decide the shape (PRD fork#235 M3):
///
/// | `DOT_AGENT_DECK_PANE_ID` | Owner marker | Identity |
/// |---|---|---|
/// | absent | — | `human:<login>@<host>` |
/// | present | present | `orchestration:<name>@<host>:<wt>` |
/// | present | absent | refuse |
///
/// The third row is the one to get right:
/// [`crate::worktree_reclaim::mark_worktree_owned`] is best-effort by
/// design, so a MISSING marker must never be read as "this is a human" —
/// every agent on one deck whose marker write failed would otherwise
/// resolve to the SAME identity, and the lock would read "held by me" and
/// wave them all through while appearing to work (`issue/claim/006`).
fn resolve_caller_identity(cwd: &Path) -> Result<Identity, String> {
    let host = crate::issue_dispatch_run::local_hostname();
    match std::env::var(crate::agent_pty::DOT_AGENT_DECK_PANE_ID) {
        Err(_) => {
            // No pane env: a human terminal. The login IS part of the
            // identity here, so — unlike the orchestration branch below —
            // failing to resolve it means we have no valid identity at all.
            let login = resolve_gh_login()?;
            Ok(Identity::human(&login, &host))
        }
        Ok(_) => match crate::worktree_reclaim::owner_of(cwd, cwd) {
            None => Err(format!(
                "DOT_AGENT_DECK_PANE_ID is set but {} carries no ownership marker — refusing \
                 rather than assuming a human caller; a worktree whose marker write failed is \
                 still a perfectly good worktree, but a missing marker must never fall back to \
                 a human identity (every agent on this deck whose marker write failed would \
                 then resolve to the SAME identity and the lock would wave them all through)",
                cwd.display()
            )),
            Some(owner) => {
                let name = owner.strip_prefix("orchestration:").unwrap_or(&owner);
                Ok(Identity::orchestration(name, &host, cwd))
            }
        },
    }
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

fn resolve_gh_login() -> Result<String, String> {
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

fn read_current_claim(repo: &str, issue: u64) -> Result<(bool, Option<ParsedClaim>), String> {
    let json = run_gh_capture(&issue_view_claim_state_argv(repo, issue))?;
    parse_claim_state(&json)
}

// ---------------------------------------------------------------------------
// The write path
// ---------------------------------------------------------------------------

/// Write the label, replace-to-one the assignee (best-effort), and append
/// the claim comment — the same three-write order PRD fork#235 M2's async
/// `claim_issue` uses, reimplemented synchronously here since M3's CLI has
/// no daemon/async runtime to reuse it through (rule 12).
fn do_claim(
    repo: &str,
    issue: u64,
    identity: &Identity,
    login: Option<&str>,
    prior_login: Option<&str>,
    takeover_from: Option<&str>,
) -> Result<(), String> {
    run_gh_status(&issue_edit_add_label_argv(repo, issue, IN_PROGRESS_LABEL))?;

    if let Some(login) = login {
        let assignee_argv = issue_edit_assignee_argv(repo, issue, Some(login), prior_login);
        // Best-effort (PRD fork#235 M2's discipline): GitHub silently drops
        // an assignee lacking repo access and `gh` may still exit 0, so a
        // genuine `gh` failure here is surfaced but must not undo the claim
        // that already succeeded.
        if let Err(e) = run_gh_status(&assignee_argv) {
            eprintln!("issue claim: warning: failed to write the assignee: {e}");
        }
    }

    let host = crate::issue_dispatch_run::local_hostname();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let body = claim_comment_body(identity, &host, &timestamp, login, takeover_from);
    run_gh_status(&issue_comment_argv(repo, issue, &body))?;
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
    let (label_present, held) = read_current_claim(&repo, issue)?;

    match decide_claim(
        label_present,
        held.as_ref(),
        &identity.to_string(),
        takeover,
        confirm_stopped,
    ) {
        ClaimDecision::Claim { takeover_from } => {
            let login = resolve_assignee_login(&identity);
            let prior_login = held.as_ref().and_then(|h| h.login.clone());
            do_claim(
                &repo,
                issue,
                &identity,
                login.as_deref(),
                prior_login.as_deref(),
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
            let since = held
                .as_ref()
                .map(|h| format!(" since {}", h.timestamp))
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
            Err(format!(
                "issue #{issue} of {repo} is held by `{holder}`{since} — {instruction}"
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
            host: "host-1".to_string(),
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
}
