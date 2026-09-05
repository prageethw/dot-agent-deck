use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent_pty::AgentPtyRegistry;
use crate::event::BroadcastMsg;
use crate::issue_dispatch_run::{
    IsolatedCloneOutcome, RemovalPolicy, RemoveOutcome, WorktreeCreation, WorktreeRegistry,
    attempt_isolated_clone_cleanup, create_worktree, provision_isolated_clone_sync_resolved,
    record_worktree, remove_worktree, run_status, worktree_still_in_use,
};
use crate::scheduler::StderrNotifier;
use crate::spawn::{SpawnKind, SpawnRequest, SpawnShapeOverride, spawn};
use crate::worktree_owner::Creator;

/// PRD #220: the orchestrations a dispatch out of `dir` could start, by resolved
/// name. Empty means only a single agent is available.
///
/// `dir` must be the CALLER's repo dir — the same directory `handle_dispatch`
/// resolves its target from. An earlier cut computed this in the CLI process from
/// its own `current_dir()` and let the spawn resolve names against the WORKTREE
/// dir instead; because `load_project_config` normalises an unnamed orchestration
/// to its directory basename, the same entry was then `myrepo` in the listing and
/// `myrepo-dispatch-<slug>` at spawn time — a name the listing offered and the
/// spawn could never match. The listing is now answered by the daemon
/// ([`list_targets_response`]) precisely so both sides share one basis.
///
/// Roleless `[[orchestrations]]` are filtered out because the spawn skips them
/// too — listing one would offer a target that cannot be spawned.
///
/// Issue #704: each entry also carries whether it is the one a dispatch that names
/// nothing would open, resolved through the SAME
/// [`crate::project_config::default_orchestration`] the spawn uses. Deriving the
/// marker here independently is exactly the two-lists-that-drift shape this issue
/// is about, so it is derived from the selector or not at all.
pub fn available_orchestrations(
    config: Option<&crate::project_config::ProjectConfig>,
    dir: &Path,
) -> Vec<crate::event::ListedOrchestration> {
    let Some(cfg) = config else {
        return Vec::new();
    };
    // Matched by INDEX, not by name: duplicate orchestration names are only a
    // validation warning, so comparing names would put `[default]` on every
    // namesake and tell the reader the choice is ambiguous when it is not.
    let default_index = crate::project_config::default_orchestration(cfg, dir).map(|d| d.index);
    cfg.orchestrations
        .iter()
        .enumerate()
        .filter(|(_, o)| !o.roles.is_empty())
        .map(|(i, o)| crate::event::ListedOrchestration {
            default: default_index == Some(i),
            name: crate::project_config::resolve_orchestration_name(&o.name, dir),
            roles: o.roles.len(),
        })
        .collect()
}

/// Human-readable `--list-targets` output, read by the dispatcher agent and
/// relayed to the user.
///
/// Schedule/authoring modes are absent by construction: a schedule creates a
/// FUTURE task, so it is not something a dispatch can start, and the dispatcher
/// option itself is not a target either. Only real spawn shapes appear.
pub fn render_available_targets(orchestrations: &[crate::event::ListedOrchestration]) -> String {
    let mut out = String::from("Available dispatch targets:\n");
    out.push_str("  single            one agent (--single)\n");
    if orchestrations.is_empty() {
        out.push_str(
            "\nNo orchestrations are defined here, so `single` is the only target.\n\
             Dispatch with `--single`.\n",
        );
        return out;
    }
    for o in orchestrations {
        // The name is SINGLE-QUOTED in the suggested command, not bare: an
        // orchestration named `code review` produced `--orchestration code review`,
        // which clap reads as the name `code` plus a stray positional and rejects
        // outright — leaving no way to pick the target just offered.
        //
        // Issue #704: the `[default]` marker is what makes "dispatch without
        // naming one" a legible choice rather than a coin flip the reader has to
        // reconstruct from the file's order.
        let marker = if o.default { "  [default]" } else { "" };
        out.push_str(&format!(
            "  orchestration     '{name}' — {roles} roles (--orchestration '{name}'){marker}\n",
            name = o.name,
            roles = o.roles,
        ));
    }
    out.push_str(
        "\nAsk the user which they want before dispatching, then pass the matching flag.\n",
    );
    out
}

/// Build the daemon's reply to a `--list-targets` request for `cwd`.
///
/// Four states the caller must be able to tell apart, none of which an empty list
/// alone can express:
///
/// * pane cwd UNKNOWN (no matching agent record) → say so. Rendering this as "no
///   orchestrations are defined here" would be a claim about a repo we never
///   looked at, and the agent would relay it as fact;
/// * no config file → only `single` is available, which is the truth;
/// * config present but UNPARSEABLE → `error` is set and named, because
///   `load_config_for_dir` swallows the parse error and a silent "no orchestrations
///   here" would walk the user past a broken config without ever learning it is
///   broken;
/// * config parsed → every role-bearing orchestration, under the name the spawn
///   will resolve it to.
pub fn list_targets_response(cwd: Option<&Path>) -> crate::event::ListTargetsResponse {
    use crate::event::ListTargetsResponse;
    let Some(dir) = cwd else {
        let msg = "could not determine this pane's working directory".to_string();
        return ListTargetsResponse {
            rendered: "Could not determine this pane's working directory, so the available \
                       orchestrations are unknown. This is NOT the same as the repo having \
                       none — do not report it that way. Dispatch `--single` to start one \
                       agent, or `--orchestration <name>` if you know the name.\n"
                .to_string(),
            orchestrations: Vec::new(),
            error: Some(msg),
        };
    };
    match crate::project_config::load_project_config(dir) {
        Ok(config) => {
            let found = available_orchestrations(config.as_ref(), dir);
            let mut rendered = render_available_targets(&found);
            // Issue #704: the same sentence the dispatch reply and the daemon log
            // carry, shown to whoever is CHOOSING rather than after the fact.
            if let Some(note) = config
                .as_ref()
                .and_then(|c| crate::project_config::default_orchestration(c, dir))
                .and_then(|chosen| chosen.diagnostic())
            {
                rendered.push_str(&format!("\nNote: {note}\n"));
            }
            ListTargetsResponse {
                rendered,
                orchestrations: found,
                error: None,
            }
        }
        Err(e) => {
            let msg = format!("{e}");
            ListTargetsResponse {
                rendered: format!(
                    "Could not read this repo's .dot-agent-deck.toml, so the available \
                     orchestrations are unknown:\n  {msg}\n\nFix the config, or dispatch \
                     `--single` (which needs no config).\n"
                ),
                orchestrations: Vec::new(),
                error: Some(msg),
            }
        }
    }
}

/// The command a single-agent dispatch runs.
///
/// `SpawnRequest.command: None` means `$SHELL` in the spawn path, so passing None
/// here starts a **shell**, not an agent: the worktree appears, a pane appears,
/// and the `--task` prompt is typed into a bash prompt. Before the shape selector
/// this repo never took the single-agent branch (role commands win for an
/// orchestration), which is why it went unnoticed — but any repo with no
/// `[[orchestrations]]` already hit it.
///
/// So resolve a real agent command: the deck's configured `default_command` when
/// set, else the Claude default, mirroring what the interactive new-pane form does
/// for a blank Command field (`resolve_authoring_command`). "Single agent" has to
/// mean an agent.
pub fn resolve_single_agent_command(configured: Option<&str>) -> String {
    let trimmed = configured.unwrap_or_default().trim();
    if trimmed.is_empty() {
        crate::agent_registry::CLAUDE_CODE
            .default_command
            .unwrap_or("claude")
            .to_string()
    } else {
        trimmed.to_string()
    }
}

fn sanitize_name(name: &str) -> String {
    let slug_chars: String = name
        .replace("..", "_")
        .replace('\0', "")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug_chars.is_empty() || slug_chars.chars().all(|c| c == '-') {
        "dispatch".to_string()
    } else {
        slug_chars.trim_matches('-').to_string()
    }
}

struct DispatchPaths {
    worktree_dir: PathBuf,
    branch: String,
}

/// Derive the sibling worktree dir + head branch for one dispatch.
///
/// Sibling layout (`../<repo>-dispatch-<slug>`) rather than nested inside the
/// caller's checkout: a nested tree would be walked by every `rg`, IDE index and
/// file watcher in the parent, and `git clean -xdff` would take it along with any
/// uncommitted agent work. This matches `/worktree-prd`'s `create.sh`.
///
/// `file_name()` is absent for a filesystem root (`/`) and for a path ending in
/// `..`; fall back to a fixed stem rather than panicking, since `working_dir`
/// comes from an agent record and a daemon must not die on a surprising cwd.
fn derive_dispatch_paths(working_dir: &Path, name: &str) -> DispatchPaths {
    let clean_name = sanitize_name(name);
    let slug = format!("dispatch-{clean_name}");
    let stem = working_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let worktree_dir = working_dir
        .parent()
        .unwrap_or(working_dir)
        .join(format!("{stem}-{slug}"));
    let branch = format!("agent/{slug}");
    DispatchPaths {
        worktree_dir,
        branch,
    }
}

pub struct DispatchResult {
    pub worktree_dir: PathBuf,
    pub success: bool,
    pub message: String,
}

pub struct DispatchContext {
    pub working_dir: PathBuf,
    pub registry: Arc<AgentPtyRegistry>,
    pub event_tx: tokio::sync::broadcast::Sender<BroadcastMsg>,
    /// The daemon-wide worktree registry the tab-close handler reads. Uses the
    /// [`WorktreeRegistry`] alias rather than spelling the map out, so the entry
    /// type cannot drift away from the registry it has to interoperate with.
    pub worktrees: WorktreeRegistry,
    /// The deck's configured `default_command`, resolved by the caller (mirroring
    /// the issue-dispatch precedent in `daemon.rs`). Used ONLY when the dispatch
    /// starts a single agent — an orchestration's role commands win. Passed in
    /// rather than read here so [`handle_dispatch`] does not depend on global
    /// config. See [`resolve_single_agent_command`].
    pub default_command: Option<String>,
    /// The daemon's [`AppState`](crate::state::AppState), so a dispatched
    /// ORCHESTRATION's role panes are registered in the maps `handle_delegate`
    /// routes on. Without it the dispatch produces an orchestrator that has been
    /// handed a delegation protocol it cannot use — see
    /// [`crate::state::AppState::register_orchestration_role`]. `None` in unit
    /// tests, which assert on the worktree/spawn result rather than on routing.
    pub state: Option<crate::state::SharedState>,
}

/// Translate the wire choice into the spawn-side override.
///
/// `None` on the wire means "whatever the dispatched worktree's config implies",
/// which is [`SpawnShapeOverride`]-absent — i.e. exactly the pre-selector
/// behaviour, so an older CLI keeps working against a newer daemon.
fn shape_override_of(shape: Option<&crate::event::DispatchShape>) -> Option<SpawnShapeOverride> {
    match shape {
        None => None,
        Some(crate::event::DispatchShape::SingleAgent) => Some(SpawnShapeOverride::SingleAgent),
        Some(crate::event::DispatchShape::Orchestration { name }) => {
            Some(SpawnShapeOverride::Orchestration(name.clone()))
        }
    }
}

/// Shared failure reporting for `IsolatedCloneOutcome::TimedOut` and
/// `::Failed`: both tell the caller to retry, log automatic cleanup when it
/// already happened (Model A parity, `src/ui.rs`'s matching arm -- P3-2),
/// and name the worktree path rather than just "try again" (P3-2). They
/// differ only in the outcome name spliced into the message/log line and in
/// whether git's own captured error text is present.
///
/// `error` is git's own captured stderr where present -- a lower-trust
/// source than the same-uid processes the rest of `handle_dispatch`'s
/// messages come from (repo content can end up in it), so it is sanitized
/// like any other untrusted text written into the caller's pane (issue #325
/// auditor C1).
fn isolated_clone_failure_result(
    worktree_dir: &Path,
    reason: &str,
    error: Option<&str>,
    cleaned_up_by: Option<&str>,
) -> DispatchResult {
    let path = crate::terminal_sanitize::sanitize_path_for_terminal_display(worktree_dir);
    let detail = if let Some(remover) = cleaned_up_by {
        tracing::info!(
            path = %path,
            remover = %crate::terminal_sanitize::sanitize_for_terminal_display(remover),
            "dispatch: isolated clone {reason}; half-created directory removed automatically"
        );
        "the half-created directory was removed automatically — try again".to_string()
    } else {
        format!("run `rm -rf {path}` to clear it, then try again")
    };
    let message = match error {
        Some(error) => format!(
            "dispatch: isolated clone {reason} at {path} — {} ({detail})",
            crate::terminal_sanitize::sanitize_for_terminal_display(error)
        ),
        None => format!("dispatch: isolated clone {reason} at {path} — {detail}"),
    };
    DispatchResult {
        worktree_dir: worktree_dir.to_path_buf(),
        success: false,
        message,
    }
}

pub async fn handle_dispatch(
    ctx: &DispatchContext,
    name: &str,
    task: &str,
    shape: Option<&crate::event::DispatchShape>,
) -> DispatchResult {
    // Fork issue #595 fix round 2 (reviewer F3): `ctx.working_dir` is the
    // calling pane's own registered cwd — after this fix that can
    // legitimately be a nested subdirectory of its repo's toplevel, since
    // any pane already running inside a resolved isolated clone at a
    // nested prefix now has one. Left unresolved, `derive_dispatch_paths`
    // below places the new worktree as a sibling of that NESTED directory
    // — a full clone materialised inside the calling pane's own working
    // tree — the same defect class F1 fixed in `src/ui.rs`'s
    // `Action::SpawnPane`, one level down. Resolve once, up front, exactly
    // as that call site does, and thread the result through: the toplevel
    // becomes the sibling base and the clone source, `relative_subpath`
    // reproduces the calling pane's own position inside the dispatched
    // worktree so the dispatched agent's cwd matches where the caller
    // actually is rather than always landing at the worktree root.
    //
    // `git rev-parse --show-toplevel` also CANONICALIZES its answer
    // (symlinks resolved — e.g. macOS `/var` -> `/private/var`, the shape
    // GitHub's macOS runners use for their temp dir), so the resolved
    // toplevel is only substituted in below when `ctx.working_dir` is a
    // GENUINE subdirectory of it (a non-empty relative prefix). When
    // `ctx.working_dir` already IS the toplevel (or isn't inside a git
    // repository at all), it stays the base unchanged — otherwise a
    // canonicalization-only difference at the always-was-the-root case
    // would change `derive_dispatch_paths`' output spelling with no change
    // to which directory it names (the same regression this exact
    // reasoning was added to `src/ui.rs`'s `Action::SpawnPane` to avoid).
    // Fork issue #595 fix round 3 (reviewer N2): `resolve_git_toplevel`
    // spawns a `git` subprocess and blocks on it for up to
    // `WORKTREE_GIT_TIMEOUT` (30s). Every other blocking git/socket op in
    // this async path already runs via `spawn_blocking` (see the
    // live-sibling gate below, whose own comment states the rule this call
    // was the one exception to) -- run this one the same way rather than
    // parking a tokio worker thread on a stalled filesystem.
    let working_dir_for_probe = ctx.working_dir.clone();
    let toplevel_resolution = match tokio::task::spawn_blocking(move || {
        crate::issue_dispatch_run::resolve_git_toplevel(&working_dir_for_probe)
    })
    .await
    {
        Ok(resolution) => resolution,
        Err(join_err) => {
            return DispatchResult {
                worktree_dir: ctx.working_dir.clone(),
                success: false,
                message: format!(
                    "dispatch: git-toplevel resolution task panicked: {}",
                    crate::terminal_sanitize::sanitize_for_terminal_display(&join_err.to_string())
                ),
            };
        }
    };
    let relative_subpath = toplevel_resolution
        .as_ref()
        .map(|(_, prefix)| prefix.clone())
        .filter(|prefix| !prefix.as_os_str().is_empty());
    let mut resolved_working_dir = if relative_subpath.is_some() {
        toplevel_resolution
            .as_ref()
            .map(|(toplevel, _)| toplevel.clone())
            .unwrap_or_else(|| ctx.working_dir.clone())
    } else {
        ctx.working_dir.clone()
    };
    let mut paths = derive_dispatch_paths(&resolved_working_dir, name);
    // Fork issue #595 fix round 3 (auditor R1): same containment check as
    // `src/ui.rs`'s `Action::SpawnPane` -- see that call site's comment
    // for the full reasoning. The "root case stays unchanged" carve-out
    // above decides "safe to use `ctx.working_dir` raw" by asking whether
    // the computed prefix came out empty, which a `ctx.working_dir`
    // reached via a symlink PLANTED INSIDE the repo (pointing back at the
    // repo's own root) also satisfies -- deriving the sibling worktree
    // from that raw path would reopen F1/F3 one level down. Check the
    // actual property instead: would the DERIVED worktree dir land inside
    // the canonicalized toplevel?
    //
    // Fork issue #595 fix round 4 (reviewer N7 / auditor S1): round 3's
    // fallback on `.canonicalize()` failure used `paths.worktree_dir`'s
    // own RAW spelling, which is only symmetric with the canonicalized
    // toplevel when `ctx.working_dir` was itself reached by a physical
    // path -- a `ctx.working_dir` reached through a SYMLINKED ANCESTOR
    // shares no textual prefix with the canonicalized toplevel either
    // way, so the guard stayed silent for that narrower shape. Canonicalize
    // `paths.worktree_dir`'s PARENT instead (it exists on disk -- it is
    // the same parent `ctx.working_dir` itself lives in) and re-attach the
    // worktree's own file name, matching the fix in `src/ui.rs`.
    if relative_subpath.is_none()
        && let Some((toplevel, _)) = toplevel_resolution.as_ref()
    {
        let canonical_toplevel = toplevel.canonicalize().unwrap_or_else(|_| toplevel.clone());
        let canonical_worktree_dir =
            match (paths.worktree_dir.parent(), paths.worktree_dir.file_name()) {
                (Some(parent), Some(file_name)) => parent
                    .canonicalize()
                    .map(|canonical_parent| canonical_parent.join(file_name))
                    .unwrap_or_else(|_| paths.worktree_dir.clone()),
                _ => paths.worktree_dir.clone(),
            };
        if canonical_worktree_dir.starts_with(&canonical_toplevel) {
            resolved_working_dir = toplevel.clone();
            paths = derive_dispatch_paths(&resolved_working_dir, name);
        }
    }
    let clone_dir = resolved_working_dir;

    // Resolve the shape from the CALLER's repo config, BEFORE any git work.
    //
    // Caller-side because that is the config the user chose from: the worktree is a
    // HEAD checkout (uncommitted config invisible) and `load_project_config`
    // normalises an unnamed orchestration to its directory basename, so the same
    // entry is `myrepo` here and `myrepo-dispatch-<slug>` there.
    //
    // Before the worktree because a rejected shape must not leave debris: validating
    // inside `spawn` meant a typo'd `--orchestration` created a worktree and branch,
    // rolled them back, and reported "failed to spawn agent" for what is a plain
    // validation error.
    let single_command = resolve_single_agent_command(ctx.default_command.as_deref());
    let caller_config = crate::spawn::load_config_for_dir(&clone_dir);
    // Issue #704: when the caller named no orchestration and the config left the
    // choice to file order, the reply says which one was opened and what else was
    // there. This message is written straight into the caller's pane and repeated
    // to the user verbatim, so it is the one surface where an implicit choice can
    // still be corrected before the work starts. Computed only for the shapes that
    // actually consult the default — an explicit `--single` or
    // `--orchestration <name>` chose for itself and needs no note.
    let default_note = match shape {
        None | Some(crate::event::DispatchShape::Orchestration { name: None }) => caller_config
            .as_ref()
            .and_then(|c| crate::project_config::default_orchestration(c, &clone_dir))
            .and_then(|chosen| chosen.diagnostic()),
        _ => None,
    };
    let resolved_target = match crate::spawn::decide_target_with_override(
        caller_config.as_ref(),
        &clone_dir,
        Some(single_command.as_str()),
        shape_override_of(shape).as_ref(),
    ) {
        Ok(t) => t,
        Err(e) => {
            return DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: false,
                message: format!("dispatch: {e}"),
            };
        }
    };

    // Fork #166: stamp this worktree with the creator identity `dispatch`
    // used, mirroring `issue-dispatch:<task>#<issue>` and
    // `orchestration:<name>` — `dispatch:<name>` names this command's own
    // dispatched worktree the same way.
    let creator_ident = Creator::dispatch(name);
    let creator = crate::worktree_reclaim::sanitize_marker_creator(&format!(
        "{}:{}",
        creator_ident.kind, creator_ident.subject
    ));

    // PRD fork#325 M3 "Model B" (issue #490): the same Nth-concurrent-
    // orchestration gate Model A (`src/ui.rs`'s `Action::SpawnPane`) already
    // applies to interactive spawns -- does `clone_dir` already share its
    // `.git` object store with a LIVE orchestration's worktree? Answered by
    // the SAME `root_checkout_has_live_sibling` gate Model A uses, so the
    // fail-open/fail-closed decisions stay byte-for-byte identical rather
    // than a second, potentially-drifting implementation. Run via
    // `spawn_blocking`: that function does synchronous socket I/O bounded by
    // several seconds, which must never block this tokio worker thread the
    // way every other blocking git/socket op in this async path already
    // avoids (see `create_worktree`'s own `spawn_blocking` use above it).
    let live_sibling_check = {
        let clone_dir_for_check = clone_dir.clone();
        tokio::task::spawn_blocking(move || {
            crate::ui::root_checkout_has_live_sibling(
                &clone_dir_for_check,
                crate::ui::SiblingScope::AnySharedCommonDir,
            )
        })
        .await
    };
    let has_live_sibling = match live_sibling_check {
        Ok(Ok(has_sibling)) => has_sibling,
        // The daemon query failed, or answered in an untrustworthy shape --
        // fail CLOSED, matching Model A's `Err(reason)` branch: refuse to
        // provision at all rather than silently falling back to the
        // ordinary shared-checkout path.
        Ok(Err(reason)) => {
            return DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: false,
                message: format!(
                    "dispatch: could not confirm no other live orchestration already \
                     shares {} — {reason}",
                    crate::terminal_sanitize::sanitize_path_for_terminal_display(&clone_dir)
                ),
            };
        }
        Err(join_err) => {
            return DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: false,
                message: format!(
                    "dispatch: live-sibling check task panicked: {}",
                    crate::terminal_sanitize::sanitize_for_terminal_display(&join_err.to_string())
                ),
            };
        }
    };

    // PRD fork#325 fix round (reviewer B3 / auditor B1): `Some` when
    // `point_isolated_clone_origin`/`remove_isolated_clone_origin_default`
    // failed inside `provision_isolated_clone_sync` -- the clone's `origin`
    // is still the plain `git clone` default, a local filesystem path
    // pointing back at `clone_dir` (the user's own root checkout). Captured
    // here (mirroring `src/ui.rs`'s `worktree_origin_warning`) so it can be
    // folded into the SUCCESS message below: this function's caller is the
    // dispatched agent itself, which CLAUDE.md rule 1 tells to run `git push
    // origin HEAD:refs/heads/<branch>` -- exactly the command this hazard
    // would silently misdirect.
    let mut clone_origin_warning: Option<String> = None;

    if has_live_sibling {
        // A live sibling already shares `clone_dir`'s git-common-dir --
        // isolate this dispatch into its own fresh clone instead of a plain
        // `git worktree add` sibling, mirroring Model A's `Ok(true)` branch
        // (`provision_isolated_clone_sync_resolved`). Same resolved sibling
        // path (`paths.worktree_dir`) as the shared-checkout arm below --
        // only the provisioning mechanism differs. It is sync (no async
        // twin exists), so it runs on the blocking pool exactly like the
        // daemon-query gate above.
        //
        // Fork issue #595 fix round 3 (reviewer N2): `clone_dir` is
        // `resolved_working_dir` above, already resolved to the real git
        // toplevel (or left as `ctx.working_dir` unchanged when that IS
        // the toplevel or it isn't inside a git repository at all) -- so
        // call the `_resolved` entry point directly rather than the
        // self-resolving `provision_isolated_clone_sync`, which would
        // otherwise re-run `resolve_git_toplevel` on an already-resolved,
        // idempotent input: a second 30s-bounded `git rev-parse`
        // subprocess per dispatch, and a reopened TOCTOU window between
        // the two resolutions that the entry-point split exists to close.
        //
        // Fix round 2 (reviewer P2-7): this arm inherits attach-not-refuse
        // branch-reuse behaviour from `provision_isolated_clone_sync_resolved`
        // (it `git checkout`s the branch if it already exists, same as
        // Model A's own isolated arm), while the `else` arm below
        // (`create_worktree` with `reuse_existing_branch: false`) REFUSES
        // via `WorktreeCreation::BranchExists` to protect possibly-committed
        // work. Which of the two runs is decided by `has_live_sibling`, a
        // runtime/timing-dependent daemon query -- so `dispatch <name>`
        // against a reused branch name can refuse on one invocation and
        // silently attach to prior commits on the next, with nothing in the
        // output distinguishing which semantics just applied. Known,
        // deliberately deferred -- see the PRD's Out of scope section.
        let source_dir = clone_dir.clone();
        let clone_target = paths.worktree_dir.clone();
        let branch = paths.branch.clone();
        let creator_for_clone = creator.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            provision_isolated_clone_sync_resolved(
                &source_dir,
                &clone_target,
                &branch,
                &creator_for_clone,
            )
        })
        .await;
        // PRD fork#544 M3 fix round: release this process-local resume
        // registration the moment provisioning returns, on every outcome —
        // the has-live-sibling daemon query above already durably
        // established liveness for this path before provisioning even ran,
        // so the registry's brief defense-in-depth job is done here
        // regardless of whether provisioning resumed, created, or refused.
        // See `resumed_isolated_clones`'s doc comment
        // (`src/issue_dispatch_run.rs`) for why this is correct rather than
        // a weakening of the race protection.
        crate::issue_dispatch_run::release_resumed_isolated_clone_registration(&paths.worktree_dir);
        match outcome {
            // Issue #164: a marker-write warning is not surfaced on this ad
            // hoc `dispatch` CLI path, matching the shared-checkout arm's
            // identical `marker_warning: _` below -- out of scope here.
            // `origin_warning` is NOT dropped the same way; see the comment
            // above `clone_origin_warning`.
            Ok(Ok(IsolatedCloneOutcome::Created {
                marker_warning: _,
                origin_warning,
            })) => {
                clone_origin_warning = origin_warning;
            }
            Ok(Ok(IsolatedCloneOutcome::AlreadyClaimed)) => {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: an isolated clone from an earlier dispatch of this name is \
                         still on disk at {} — it may not be running anymore (isolated clones \
                         are never removed automatically). Run `rm -rf {}` once its work is \
                         captured, or dispatch under a different name.",
                        crate::terminal_sanitize::sanitize_path_for_terminal_display(
                            &paths.worktree_dir
                        ),
                        crate::terminal_sanitize::sanitize_path_for_terminal_display(
                            &paths.worktree_dir
                        )
                    ),
                };
            }
            // PRD fork#544 M3: resuming an existing, eligible isolated clone
            // is a success just like `Created` above — this ad hoc
            // `dispatch` CLI path does not surface a warning on `Created`
            // either (`marker_warning: _` above), so `fetch_warning` is
            // dropped here for the same reason.
            Ok(Ok(IsolatedCloneOutcome::Resumed { fetch_warning: _ })) => {}
            Ok(Ok(IsolatedCloneOutcome::Rejected(reason))) => {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: cannot use the isolated clone at {} — {}",
                        paths.worktree_dir.display(),
                        reason.describe(),
                    ),
                };
            }
            Ok(Ok(IsolatedCloneOutcome::TimedOut { cleaned_up_by })) => {
                return isolated_clone_failure_result(
                    &paths.worktree_dir,
                    "timed out",
                    None,
                    cleaned_up_by.as_deref(),
                );
            }
            // Issue #325 fix round 3 parity (reviewer C2 / auditor C2): a
            // genuine (non-timeout) clone/checkout failure is reported
            // separately from `TimedOut`, with git's own captured error text
            // rather than a generic message.
            Ok(Ok(IsolatedCloneOutcome::Failed {
                error,
                cleaned_up_by,
            })) => {
                return isolated_clone_failure_result(
                    &paths.worktree_dir,
                    "failed",
                    Some(&error),
                    cleaned_up_by.as_deref(),
                );
            }
            Ok(Err(e)) => {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: failed to provision isolated clone: {}",
                        crate::terminal_sanitize::sanitize_for_terminal_display(&e)
                    ),
                };
            }
            Err(join_err) => {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: isolated-clone provisioning task panicked: {}",
                        crate::terminal_sanitize::sanitize_for_terminal_display(
                            &join_err.to_string()
                        )
                    ),
                };
            }
        }
    } else {
        match create_worktree(
            &clone_dir,
            &paths.worktree_dir,
            &paths.branch,
            false,
            creator_ident,
        )
        .await
        {
            // Issue #164: a marker-write warning is not surfaced on this
            // ad hoc `dispatch` CLI path -- out of scope here, which covers
            // only the TUI's `SpawnPane` creation and the scheduled
            // `issue_dispatch` task (see `src/ui.rs` / `src/issue_dispatch_run.rs`).
            // `ownership_of` still fails closed for it regardless.
            Ok(WorktreeCreation::Created { marker_warning: _ }) => {}
            Ok(WorktreeCreation::AlreadyClaimed) => {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: worktree {} is already claimed by another dispatch. \
                         Wait for it to finish, or dispatch under a different name.",
                        paths.worktree_dir.display()
                    ),
                };
            }
            // The worktree dir is GONE but its branch survived — `git worktree
            // remove` never deletes the branch, so this is the ordinary state after a
            // previous dispatch of the same name was cleaned up. Say so, and name
            // both fixes: the branch is not deleted implicitly because it may hold
            // that dispatch's committed work.
            Ok(WorktreeCreation::BranchExists) => {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: branch {branch} already exists from an earlier dispatch named \
                         '{name}' (its worktree is already gone). That branch may hold committed \
                         work, so it is left alone. Dispatch under a different name, or run \
                         `git -C {clone} branch -D {branch}` first if you are done with it.",
                        branch = paths.branch,
                        name = name,
                        clone = clone_dir.display(),
                    ),
                };
            }
            // Fork #282: the async `create_worktree` now bounds its `git worktree
            // add` on `WORKTREE_GIT_TIMEOUT` and reports `TimedOut` when it fires,
            // so this arm is genuinely reachable from this call site.
            Ok(WorktreeCreation::TimedOut { cleaned_up_by }) => {
                let detail = if cleaned_up_by.is_some() {
                    "the half-created directory was removed automatically — try again".to_string()
                } else {
                    format!(
                        "run `git -C {} worktree remove --force {}` to clear it, then try again",
                        clone_dir.display(),
                        paths.worktree_dir.display()
                    )
                };
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!("dispatch: worktree add timed out — {detail}"),
                };
            }
            Err(e) => {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: failed to create worktree: {}",
                        crate::terminal_sanitize::sanitize_for_terminal_display(&e)
                    ),
                };
            }
        }
    }

    // PRD fork#325 M3 (issue #490 fix round, reviewer B2/A3 / auditor A3):
    // the isolated-clone arm gets its OWN policy. `paths.worktree_dir` is an
    // independent `git clone`, not a linked worktree of `clone_dir` -- tab
    // close must never run `git worktree remove` against it (it isn't one),
    // and unlike the shared-checkout arm, removing it also destroys its own
    // `.git`, so `KeepIfDirty`'s dirty-only protection is not enough. See
    // [`RemovalPolicy::IsolatedClone`].
    //
    // `RemovalPolicy::KeepIfDirty`: on the shared-checkout arm this worktree
    // is a sibling of the user's own checkout and its name was chosen by an
    // LLM, so closing the tab must not destroy uncommitted work. See
    // [`RemovalPolicy`].
    record_worktree(
        &ctx.worktrees,
        &paths.worktree_dir,
        &clone_dir,
        if has_live_sibling {
            RemovalPolicy::IsolatedClone
        } else {
            RemovalPolicy::KeepIfDirty
        },
    );

    // Fork issue #595 fix round 2: reproduce the calling pane's own
    // position inside the freshly-provisioned worktree, mirroring
    // `src/ui.rs`'s `provision_isolated_clone_or_status` existence check —
    // a `git clone`/`git worktree add` only ever reproduces TRACKED
    // content, so a `relative_subpath` that is gitignored, untracked, or
    // simply empty (git tracks no empty directories) would otherwise hand
    // the dispatched agent a nonexistent cwd silently.
    let dispatch_working_dir = match relative_subpath.as_deref() {
        Some(rel) => {
            let joined = paths.worktree_dir.join(rel);
            // Fork issue #595 fix round 3 (auditor R4): `is_dir()` follows
            // symlinks, and a `git clone`/`git worktree add` faithfully
            // reproduces TRACKED symlinks -- including ones whose committed
            // target is an absolute path outside the repository entirely.
            // Canonicalize `joined` and require it to still be contained in
            // the canonicalized worktree dir, mirroring `src/ui.rs`'s
            // identical hardening of its own existence check.
            let canonical_base = paths
                .worktree_dir
                .canonicalize()
                .unwrap_or_else(|_| paths.worktree_dir.clone());
            let canonical_joined = joined.canonicalize().unwrap_or_else(|_| joined.clone());
            if !joined.is_dir() {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: {} was not found inside the provisioned worktree at {} — \
                         the calling pane's own subdirectory may be untracked or excluded by \
                         .gitignore",
                        crate::terminal_sanitize::sanitize_path_for_terminal_display(rel),
                        crate::terminal_sanitize::sanitize_path_for_terminal_display(
                            &paths.worktree_dir
                        ),
                    ),
                };
            }
            // Fork issue #595 fix round 4 (reviewer N9): a distinct
            // message from the not-found case above -- the path WAS
            // found, but a tracked symlink at this subpath resolves
            // outside the provisioned worktree, so "untracked or
            // gitignored" is not the cause and ".gitignore" is not the
            // remedy.
            if !canonical_joined.starts_with(&canonical_base) {
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message: format!(
                        "dispatch: {} escapes the provisioned worktree at {} — a tracked \
                         symlink at this subpath resolves outside the isolated worktree",
                        crate::terminal_sanitize::sanitize_path_for_terminal_display(rel),
                        crate::terminal_sanitize::sanitize_path_for_terminal_display(
                            &paths.worktree_dir
                        ),
                    ),
                };
            }
            joined
        }
        None => paths.worktree_dir.clone(),
    };

    let prompt = task.to_string();

    let req = SpawnRequest {
        task_name: format!("dispatch-{name}"),
        working_dir: dispatch_working_dir.to_string_lossy().into_owned(),
        // A real agent command, never `None` — see `resolve_single_agent_command`.
        // Ignored when the dispatch starts an orchestration (role commands win).
        command: Some(single_command),
        prompt,
        resolved_target: Some(resolved_target),
        // PRD #222 parity, dispatch-only for now — see the field's docs.
        compose_orchestrator_context: true,
        // Fork #166 M2.4: the SAME string just written into the worktree's
        // `created-by:` marker above (`create_worktree`), not a second
        // derivation of it. Cloned rather than moved: issue #469's
        // spawn-rollback arm below also needs `creator`, as the `remover`
        // identity for its own `remove_worktree` call.
        owner: Some(creator.clone()),
    };

    let notifier = StderrNotifier;

    match spawn(
        req,
        &ctx.registry,
        &notifier,
        Some(&ctx.event_tx),
        false,
        ctx.state.as_ref(),
    )
    .await
    {
        Ok(handle) => {
            // Report what was ACTUALLY opened, from the spawn's own verdict.
            // `spawn` → `decide_target` branches on the dispatched worktree's
            // `.dot-agent-deck.toml`: a repo defining `[[orchestrations]]` gets a
            // full multi-role orchestration, anything else a single agent (PRD
            // #220 M1.1). Hardcoding either word makes this message a lie in the
            // other case — and it is written straight into the caller's pane, so
            // the dispatching agent repeats it to the user verbatim.
            let mut message = match &handle.kind {
                SpawnKind::Orchestration { name: orch } => format!(
                    "dispatch: spawned isolated orchestration '{}' for '{}' in {}",
                    crate::terminal_sanitize::sanitize_for_terminal_display(orch),
                    crate::terminal_sanitize::sanitize_for_terminal_display(name),
                    crate::terminal_sanitize::sanitize_path_for_terminal_display(
                        &paths.worktree_dir
                    )
                ),
                SpawnKind::SingleAgent => format!(
                    "dispatch: spawned isolated agent for '{}' in {}",
                    crate::terminal_sanitize::sanitize_for_terminal_display(name),
                    crate::terminal_sanitize::sanitize_path_for_terminal_display(
                        &paths.worktree_dir
                    )
                ),
            };
            // Only for an orchestration: a `--single` dispatch consulted no
            // default, so a note about which one it would have picked is
            // noise the caller then relays to the user as if it mattered.
            if let (SpawnKind::Orchestration { .. }, Some(note)) = (&handle.kind, &default_note) {
                message.push_str(&format!("\ndispatch: {note}"));
            }
            // PRD fork#325 fix round (reviewer B3 / auditor B1): surface the
            // origin-fixup warning captured above. This message reaches the
            // dispatching agent verbatim (`DispatchResult.message` is written
            // straight into the caller's pane), and that agent is precisely
            // the one CLAUDE.md rule 1 tells to run `git push origin
            // HEAD:refs/heads/<branch>` -- so it must not silently push into
            // the user's own root checkout. Matches `src/ui.rs`'s handling of
            // the same field.
            if let Some(error) = &clone_origin_warning {
                message.push_str(&format!(
                    " (warning: this clone's `origin` could not be pointed at the real remote \
                     ({}) — manually run `git remote set-url origin <url>` (or `git remote \
                     remove origin` if none exists) before pushing from inside it)",
                    crate::terminal_sanitize::sanitize_for_terminal_display(error)
                ));
            }
            DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: true,
                message,
            }
        }
        Err(e) => {
            // `Force` on the rollback path, unlike the tab-close path: for a
            // SINGLE-role dispatch no agent has been handed this worktree
            // yet, so there is no user work to protect and it MUST actually
            // go, or the leftover dir and branch wedge this name for every
            // later dispatch. That is NOT true for a multi-role
            // orchestration, though (PRD 236 review) — `spawn()`'s
            // orchestration branch spawns roles in a loop with `?`
            // (`spawn.rs:536`), so a later role's spawn failure can find
            // EARLIER roles already live as PTY children rooted in this
            // worktree. Issue #469: guarded with `worktree_still_in_use` —
            // when a live sibling is still rooted here it may hold
            // committed work whose only record is this worktree, its
            // branch, or its registry entry, so all three are left alone
            // rather than yanked out from under it.
            if worktree_still_in_use(&ctx.registry.agent_records(), &paths.worktree_dir) {
                tracing::warn!(
                    worktree = %paths.worktree_dir.display(),
                    "spawn rollback: a live sibling role is still rooted in this worktree; \
                     skipping cleanup"
                );
                let mut message = format!(
                    "dispatch: spawn failed: {e} (cleanup skipped: {} is still rooted by a \
                     live sibling role — worktree, branch, and registry entry left in place)",
                    crate::terminal_sanitize::sanitize_path_for_terminal_display(
                        &paths.worktree_dir
                    )
                );
                // Fork#325 fix round 2 (reviewer P1-2 / auditor R3): this is
                // exactly the path where live roles remain inside the clone
                // -- the agents CLAUDE.md rule 1 tells to `git push origin`
                // -- so the origin warning matters here at least as much as
                // on the success path below, and must not be dropped.
                if let Some(error) = &clone_origin_warning {
                    message.push_str(&format!(
                        " (warning: this clone's `origin` could not be pointed at the real \
                         remote ({}) — manually run `git remote set-url origin <url>` (or `git \
                         remote remove origin` if none exists) before pushing from inside it)",
                        crate::terminal_sanitize::sanitize_for_terminal_display(error)
                    ));
                }
                return DispatchResult {
                    worktree_dir: paths.worktree_dir.clone(),
                    success: false,
                    message,
                };
            }

            // PRD fork#325 M3 (issue #490 fix round, reviewer B2 / auditor
            // A1/A2): the two mechanisms need entirely different rollback
            // shapes. `clone_dir` (the root checkout) never created
            // `paths.worktree_dir` or `paths.branch` on the isolated-clone
            // arm -- `provision_isolated_clone_sync` created both INSIDE the
            // clone itself -- so running `git worktree remove`/`branch -D`
            // against `clone_dir` there is not merely ineffective, it is
            // actively wrong: `remove_worktree` fails (exit 128, the target
            // is not a linked worktree of `clone_dir`), and `branch -D`
            // deletes whatever branch of that name happens to already exist
            // IN THE ROOT CHECKOUT -- unrelated committed work, force-deleted
            // with no error surfaced, since the delete itself succeeds
            // against the wrong repository (auditor A1, verified
            // empirically).
            // issue #473 (shared-checkout arm) / issue #563 (isolated-clone
            // arm): `should_drop_registry` tracks whether the worktree was
            // actually removed from disk -- the isolated-clone arm's cleanup
            // attempt is unconditional (see its own comment below), but the
            // attempt itself can still fail (`attempt_isolated_clone_cleanup`
            // returning `None`), just as the shared-checkout arm's
            // `remove_worktree` can genuinely fail (e.g. a locked worktree).
            // Dropping the registry entry in either failure case would lose
            // the only record that the worktree is still on disk.
            let should_drop_registry;
            let cleanup_failed = if has_live_sibling {
                // The clone directory IS `paths.worktree_dir` -- remove it
                // with the same helper `provision_isolated_clone_sync`'s own
                // internal `TimedOut`/`Failed` cleanup already uses, never
                // `remove_worktree` (a shared-checkout-shaped operation) and
                // never a `branch -D` against `clone_dir` (the branch lives
                // in the clone, not there).
                //
                // This unconditional `remove_dir_all` looks like it
                // contradicts `RemovalPolicy::IsolatedClone`'s deliberately
                // conservative "always Kept, regardless of dirtiness" tab-
                // close policy a few dozen lines away in
                // `issue_dispatch_run.rs` -- it does not. That policy exists
                // because a clean working tree does not prove committed work
                // is safe to discard; this path runs only when `spawn()`
                // itself just failed and (per `worktree_still_in_use` above)
                // no live sibling role is rooted here, so for THIS clone no
                // agent was ever handed it and there is no work of any kind
                // to protect -- the same reasoning the `else` arm's `Force`
                // policy already applies to the shared-checkout case.

                let wt = paths.worktree_dir.clone();
                let cr = creator.clone();
                let cleaned_up_by =
                    tokio::task::spawn_blocking(move || attempt_isolated_clone_cleanup(&wt, &cr))
                        .await
                        .ok()
                        .flatten();
                // issue #563: mirror the shared-checkout arm below --
                // cleanup failing here must retain the registry entry too,
                // since it's the only record the clone is still on disk.
                should_drop_registry = cleaned_up_by.is_some();
                cleaned_up_by.is_none()
            } else {
                // Match the outcome instead of discarding it (`let _ = ...`)
                // -- `RemoveOutcome` is `#[must_use]` for exactly this
                // reason (PRD 236 review). Mirrors the tab-close precedent
                // in `daemon_protocol.rs` that already matches on
                // `Kept`/`RemoveFailed`/`Removed`.
                let remove_outcome = remove_worktree(
                    &paths.worktree_dir,
                    &clone_dir,
                    RemovalPolicy::Force,
                    &creator,
                )
                .await;
                // issue #473: removal genuinely not happening (failed, or --
                // though `RemovalPolicy::Force` never produces it -- kept)
                // must not drop the registry entry, since that entry is the
                // only record that the worktree is still on disk.
                // auditor A4: don't trust git's reported exit status alone --
                // mirror the `removed && !dir.exists()` second signal
                // `attempt_worktree_cleanup`/`attempt_worktree_cleanup_async`
                // already require elsewhere in this repo
                // (`issue_dispatch_run.rs`) before treating a removal as
                // confirmed.
                let remove_failed = match remove_outcome {
                    RemoveOutcome::Removed(_) => paths.worktree_dir.exists(),
                    RemoveOutcome::Kept(_) | RemoveOutcome::RemoveFailed(_) => true,
                };
                should_drop_registry = !remove_failed;
                // Also delete the branch: `git worktree remove` never
                // deletes it. Same multi-role caveat as above — a still-live
                // sibling role may hold committed work whose only record is
                // this branch.
                let branch_delete_failed = run_status(
                    "git",
                    &[
                        "-C",
                        &clone_dir.to_string_lossy(),
                        "branch",
                        "-D",
                        &paths.branch,
                    ],
                )
                .await
                .is_err();
                remove_failed || branch_delete_failed
            };

            if cleanup_failed {
                tracing::warn!(
                    worktree = %paths.worktree_dir.display(),
                    branch = %paths.branch,
                    isolated = has_live_sibling,
                    "spawn rollback: cleanup failed — name may be wedged for future dispatches"
                );
            }

            if should_drop_registry {
                let mut wts = ctx.worktrees.lock().unwrap_or_else(|e| e.into_inner());
                wts.remove(&paths.worktree_dir);
            }

            let cleanup_note = if !cleanup_failed {
                String::new()
            } else if has_live_sibling {
                format!(
                    " (cleanup failed: run `rm -rf {}` to clear it, then try again)",
                    crate::terminal_sanitize::sanitize_path_for_terminal_display(
                        &paths.worktree_dir
                    )
                )
            } else {
                // issue #473 review round (reviewer P2-1 / auditor A2): this
                // message is the actual recovery path a human will read --
                // retaining the registry entry buys no automatic recovery,
                // since its only production consumer (tab-close) needs a
                // live agent rooted here. Name both things that can still be
                // on disk (the branch delete and the worktree removal are
                // separate calls, either of which can fail independently)
                // and give the same actionable hint the `has_live_sibling`
                // arm above already gives.
                format!(
                    " (cleanup failed: branch and/or worktree directory may still exist — \
                     check `{}`; if it's still there, run `rm -rf` on it and delete the \
                     branch manually, then try again)",
                    crate::terminal_sanitize::sanitize_path_for_terminal_display(
                        &paths.worktree_dir
                    )
                )
            };

            DispatchResult {
                worktree_dir: paths.worktree_dir,
                success: false,
                message: format!("dispatch: spawn failed: {e}{cleanup_note}"),
            }
        }
    }
}

// Issue #322: every scratch dir here goes through `crate::test_temp::tempdir()`
// rather than a bare `tempfile::tempdir()`. These tests build real git repos and
// real worktrees — the e2e-gated one below was measured holding a live 184 KiB
// `/tmp/.tmpYN3lNF` during a recorded `cargo test-e2e` — and the lib target does
// not link `tests/common/`, so nothing else moves them off the RAM-backed `/tmp`.
// `linkage-check` rule 8 covers this file, so a bare constructor cannot come back.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_dispatch_run::{new_worktree_registry, take_worktree};

    /// Build a real git repo with one commit, so the `git worktree` primitives
    /// under test operate on a genuine repo rather than a stubbed one.
    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git available");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        std::fs::create_dir_all(dir).unwrap();
        run(&["init", "-q", "."]);
        run(&["config", "user.email", "t@t.t"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        std::process::Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(repo)
            .output()
            .expect("git available")
            .status
            .success()
    }

    // --- slug + path derivation ---

    #[test]
    fn sanitize_name_neutralizes_path_traversal_and_separators() {
        // `..` and `/` must never survive into a path segment.
        assert!(!sanitize_name("../../etc/passwd").contains(".."));
        assert!(!sanitize_name("../../etc/passwd").contains('/'));
        // An all-punctuation name still yields a usable slug.
        assert_eq!(sanitize_name("///"), "dispatch");
        assert_eq!(sanitize_name(""), "dispatch");
        // Ordinary LLM-chosen slugs pass through untouched.
        assert_eq!(sanitize_name("fix-auth-bug"), "fix-auth-bug");
        assert_eq!(sanitize_name("add_rate_limiter"), "add_rate_limiter");
    }

    #[test]
    fn derive_dispatch_paths_places_worktree_as_sibling_not_nested() {
        let paths = derive_dispatch_paths(Path::new("/home/u/myrepo"), "fix-auth");
        assert_eq!(
            paths.worktree_dir,
            PathBuf::from("/home/u/myrepo-dispatch-fix-auth"),
            "the worktree must be a SIBLING of the checkout, never nested inside it"
        );
        assert_eq!(paths.branch, "agent/dispatch-fix-auth");
    }

    #[test]
    fn derive_dispatch_paths_survives_a_root_working_dir() {
        // `/` has no `file_name()`. This must not panic — it runs inside the
        // daemon's hook loop, where a panic kills the connection task.
        let paths = derive_dispatch_paths(Path::new("/"), "x");
        assert_eq!(paths.branch, "agent/dispatch-x");
        assert!(paths.worktree_dir.to_string_lossy().contains("dispatch-x"));
    }

    // --- the leftover-branch refusal (the one-shot-per-name defect) ---

    /// A dispatch name is reusable across cleanup cycles *as a diagnosable
    /// state*: `git worktree remove` PRESERVES the branch, so the second
    /// dispatch of a name must report `BranchExists` — NOT `AlreadyClaimed`,
    /// which would blame a worktree the user can see is already gone.
    #[tokio::test]
    async fn second_dispatch_of_a_name_reports_branch_exists_after_cleanup() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "fix-auth");

        // First dispatch claims the name.
        assert_eq!(
            create_worktree(
                &repo,
                &paths.worktree_dir,
                &paths.branch,
                false,
                Creator::dispatch("fix-auth")
            )
            .await,
            Ok(WorktreeCreation::Created {
                marker_warning: None
            })
        );

        // Tab close: the worktree goes away, the branch does not.
        let _ = remove_worktree(
            &paths.worktree_dir,
            &repo,
            RemovalPolicy::KeepIfDirty,
            "dispatch:test",
        )
        .await;
        assert!(!paths.worktree_dir.exists(), "worktree dir should be gone");
        assert!(
            branch_exists(&repo, &paths.branch),
            "git worktree remove must not delete the branch — the premise of this test"
        );

        // Second dispatch of the SAME name: refused, but for the real reason.
        assert_eq!(
            create_worktree(
                &repo,
                &paths.worktree_dir,
                &paths.branch,
                false,
                Creator::dispatch("fix-auth")
            )
            .await,
            Ok(WorktreeCreation::BranchExists),
            "a leftover branch must be distinguishable from a claimed worktree"
        );
    }

    /// Deleting the leftover branch makes the name usable again — the recovery
    /// path the refusal message tells the user about.
    #[tokio::test]
    async fn deleting_the_leftover_branch_frees_the_name() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "fix-auth");

        create_worktree(
            &repo,
            &paths.worktree_dir,
            &paths.branch,
            false,
            Creator::dispatch("fix-auth"),
        )
        .await
        .unwrap();
        let _ = remove_worktree(
            &paths.worktree_dir,
            &repo,
            RemovalPolicy::KeepIfDirty,
            "dispatch:test",
        )
        .await;
        std::process::Command::new("git")
            .args(["branch", "-D", &paths.branch])
            .current_dir(&repo)
            .output()
            .expect("git available");

        assert_eq!(
            create_worktree(
                &repo,
                &paths.worktree_dir,
                &paths.branch,
                false,
                Creator::dispatch("fix-auth")
            )
            .await,
            Ok(WorktreeCreation::Created {
                marker_warning: None
            }),
            "after deleting the branch the same dispatch name must work again"
        );
    }

    // --- removal policy (the PRD #120 regression) ---

    /// `KeepIfDirty` (PRD #220 dispatch): uncommitted work in the worktree wins
    /// over cleanup — the tree stays so the user can recover it.
    #[tokio::test]
    async fn keep_if_dirty_preserves_a_worktree_with_uncommitted_work() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "unit");
        create_worktree(
            &repo,
            &paths.worktree_dir,
            &paths.branch,
            false,
            Creator::dispatch("unit"),
        )
        .await
        .unwrap();
        std::fs::write(paths.worktree_dir.join("uncommitted.txt"), "work").unwrap();

        let _ = remove_worktree(
            &paths.worktree_dir,
            &repo,
            RemovalPolicy::KeepIfDirty,
            "dispatch:test",
        )
        .await;

        assert!(
            paths.worktree_dir.exists(),
            "a dirty dispatch worktree must survive tab close so work is recoverable"
        );
    }

    /// `Force`: the directory MUST go even when dirty. PRD 236 unified both
    /// dispatch producers onto `KeepIfDirty` — `#120` issue-dispatch no
    /// longer force-removes (see [`RemovalPolicy`]'s doc comment) — so this
    /// is now a direct policy check, independent of either producer, rather
    /// than the issue-dispatch-specific regression guard it used to be. The
    /// one caller left depending on `Force` is `dispatch.rs`'s own
    /// spawn-failure rollback.
    #[tokio::test]
    async fn force_removes_a_dirty_worktree_regardless_of_uncommitted_work() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let worktree_dir = repo.join(".worktrees").join("issue-7");
        create_worktree(
            &repo,
            &worktree_dir,
            "agent/issue-7",
            true,
            Creator::issue_dispatch("unit", 7),
        )
        .await
        .unwrap();
        std::fs::write(worktree_dir.join("uncommitted.txt"), "wip").unwrap();

        let _ = remove_worktree(&worktree_dir, &repo, RemovalPolicy::Force, "dispatch:test").await;

        assert!(
            !worktree_dir.exists(),
            "RemovalPolicy::Force must remove a dirty worktree unconditionally -- the policy \
             `dispatch.rs`'s spawn-failure rollback depends on"
        );
    }

    // --- the policy survives the registry round-trip the close handler uses ---

    /// The close handler in `daemon_protocol.rs` sees only a path, so the policy
    /// has to come back out of the registry intact — otherwise both producers
    /// silently share whichever policy is hardcoded there.
    #[test]
    fn registry_round_trip_preserves_each_producers_policy() {
        let reg = new_worktree_registry();
        let clone = PathBuf::from("/ws/clone");
        let issue_wt = PathBuf::from("/ws/clone/.worktrees/issue-7");
        let dispatch_wt = PathBuf::from("/ws/clone-dispatch-fix-auth");

        record_worktree(&reg, &issue_wt, &clone, RemovalPolicy::Force);
        record_worktree(&reg, &dispatch_wt, &clone, RemovalPolicy::KeepIfDirty);

        assert_eq!(
            take_worktree(&reg, &issue_wt).map(|e| e.policy),
            Some(RemovalPolicy::Force)
        );
        assert_eq!(
            take_worktree(&reg, &dispatch_wt).map(|e| e.policy),
            Some(RemovalPolicy::KeepIfDirty)
        );
    }

    // --- PRD #220: the target listing + the wire choice ---

    fn cfg(toml: &str) -> crate::project_config::ProjectConfig {
        toml::from_str(toml).expect("parse project config")
    }

    /// The listing offers `single` always, plus every ROLE-BEARING orchestration
    /// by resolved name. Schedule/authoring modes never appear — they create a
    /// future task rather than starting a line of work, so they are not targets.
    #[test]
    fn available_targets_list_single_plus_every_role_bearing_orchestration() {
        let c = cfg("[[modes]]\nname = \"dev\"\n\n\
             [[orchestrations]]\nname = \"digest\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations.roles]]\nname = \"worker\"\ncommand = \"sh\"\n\n\
             [[orchestrations]]\nname = \"review\"\n\n\
             [[orchestrations.roles]]\nname = \"lead\"\ncommand = \"cat\"\nstart = true\n");
        let found = available_orchestrations(Some(&c), Path::new("/tmp/repo"));
        assert_eq!(
            found
                .iter()
                .map(|o| (o.name.as_str(), o.roles, o.default))
                .collect::<Vec<_>>(),
            // `digest` is marked because it comes first and neither declares
            // itself; issue #704's `default_orchestration_*` tests own the rule.
            vec![("digest", 2, true), ("review", 1, false)]
        );

        let rendered = render_available_targets(&found);
        assert!(rendered.contains("--single"), "single is always offered");
        assert!(
            rendered.contains("--orchestration 'digest'"),
            "the name must be single-quoted so a name with spaces still parses:\n{rendered}"
        );
        assert!(rendered.contains("--orchestration 'review'"));
        assert!(
            !rendered.contains("schedule") && !rendered.contains("dev"),
            "modes and schedule authoring are not dispatch targets:\n{rendered}"
        );
    }

    /// An unnamed orchestration is listed under the name it will actually spawn
    /// as — the dir basename — so the name the agent passes back matches.
    #[test]
    fn available_targets_resolve_an_unnamed_orchestration_to_the_dir_basename() {
        let c = cfg("[[orchestrations]]\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n");
        let found = available_orchestrations(Some(&c), Path::new("/home/u/morning-digest"));
        assert_eq!(
            found
                .iter()
                .map(|o| (o.name.as_str(), o.roles, o.default))
                .collect::<Vec<_>>(),
            vec![("morning-digest", 1, true)]
        );
    }

    /// Issue #704: the listing says which one a dispatch that names nothing would
    /// open, AND — when the file did not say — that the answer came from file
    /// order. Without both halves the agent relaying this to the user can only
    /// report a list of equals.
    #[test]
    fn available_targets_mark_the_default_and_report_an_implicit_one() {
        let c = cfg("[[orchestrations]]\nname = \"mixed\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations]]\nname = \"gpt\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n");
        let rendered =
            render_available_targets(&available_orchestrations(Some(&c), Path::new("/tmp/repo")));
        let mixed_line = rendered
            .lines()
            .find(|l| l.contains("'mixed'"))
            .expect("mixed must be listed");
        assert!(
            mixed_line.contains("[default]"),
            "the default must be marked on its own line:\n{rendered}"
        );
        assert!(
            !rendered
                .lines()
                .any(|l| l.contains("'gpt'") && l.contains("[default]")),
            "exactly one line may carry the marker:\n{rendered}"
        );

        // And the whole daemon reply, which is what the agent actually reads. It
        // loads the config off disk, so this half needs a real dir.
        let tmp = crate::test_temp::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(crate::project_config::CONFIG_FILE_NAME),
            "[[orchestrations]]\nname = \"mixed\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations]]\nname = \"gpt\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n",
        )
        .unwrap();
        let listed = list_targets_response(Some(tmp.path()));
        assert!(
            listed.rendered.contains("Note:") && listed.rendered.contains("default = true"),
            "an implicit default must be reported, not merely marked:\n{}",
            listed.rendered
        );
        assert!(
            listed
                .orchestrations
                .iter()
                .any(|o| o.name == "mixed" && o.default),
            "the marker must ride the WIRE too, so a caller acting structurally sees it: {:?}",
            listed.orchestrations
        );
    }

    /// A DECLARED default is marked but not narrated — the config already said
    /// what it meant, so a note would be noise on every listing forever.
    #[test]
    fn a_declared_default_is_marked_without_a_note() {
        let c = cfg("[[orchestrations]]\nname = \"mixed\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations]]\nname = \"gpt\"\ndefault = true\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n");
        let found = available_orchestrations(Some(&c), Path::new("/tmp/repo"));
        assert_eq!(
            found
                .iter()
                .map(|o| (o.name.as_str(), o.default))
                .collect::<Vec<_>>(),
            vec![("mixed", false), ("gpt", true)],
            "the marker follows the declaration, not the file order"
        );
    }

    /// Two orchestrations sharing a name must not both be marked `[default]` —
    /// duplicate names are only a validation warning, so the listing has to
    /// resolve identity by position rather than by label.
    #[test]
    fn a_duplicated_orchestration_name_marks_exactly_one_default() {
        let c = cfg("[[orchestrations]]\nname = \"twin\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations]]\nname = \"twin\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"sh\"\nstart = true\n");
        let found = available_orchestrations(Some(&c), Path::new("/tmp/repo"));
        assert_eq!(
            found.iter().filter(|o| o.default).count(),
            1,
            "exactly one entry may carry the marker: {found:?}"
        );
        assert!(
            found[0].default && !found[1].default,
            "and it is the first: {found:?}"
        );
    }

    /// No config at all: only `single`, and the text says so rather than leaving
    /// the agent to infer it from an empty list.
    #[test]
    fn available_targets_without_config_offer_single_only() {
        let found = available_orchestrations(None, Path::new("/tmp/repo"));
        assert!(found.is_empty());
        let rendered = render_available_targets(&found);
        assert!(rendered.contains("--single"));
        assert!(
            rendered.contains("No orchestrations are defined"),
            "the empty case must state the situation:\n{rendered}"
        );
    }

    /// A single-agent dispatch must run an AGENT, never `$SHELL`.
    ///
    /// `SpawnRequest.command: None` means `$SHELL` in the spawn path, so the
    /// original `None` started a shell and typed the `--task` prompt into a bash
    /// prompt. Reported from real use once `--single` made that branch reachable in
    /// a repo that defines `[[orchestrations]]`; it was already reachable in any
    /// repo without them.
    #[test]
    fn single_agent_dispatch_resolves_an_agent_command_never_a_shell() {
        // Configured command wins, whitespace-trimmed.
        assert_eq!(resolve_single_agent_command(Some("opencode")), "opencode");
        assert_eq!(resolve_single_agent_command(Some("  claude  ")), "claude");

        // Unset / blank falls back to a real agent, NOT an empty string (which the
        // spawn path would read as `$SHELL`).
        for blank in [None, Some(""), Some("   ")] {
            let resolved = resolve_single_agent_command(blank);
            assert!(
                !resolved.trim().is_empty(),
                "a blank default_command must still resolve to an agent, got {resolved:?}"
            );
            assert_eq!(
                resolved,
                crate::agent_registry::CLAUDE_CODE
                    .default_command
                    .unwrap_or("claude"),
                "the fallback must match what the new-pane form uses for a blank Command"
            );
        }
    }

    /// The listing must distinguish "unknown pane", "broken config" and "genuinely
    /// none". Collapsing any of them into the empty listing makes the agent report a
    /// claim about a repo nobody looked at — the same dishonesty as reading a parse
    /// error as "no orchestrations".
    #[test]
    fn list_targets_distinguishes_unknown_pane_broken_config_and_genuinely_none() {
        // Unknown pane: explicit, and NOT phrased as "no orchestrations".
        let unknown = list_targets_response(None);
        assert!(
            unknown.error.is_some(),
            "unknown cwd must be an error state"
        );
        assert!(unknown.orchestrations.is_empty());
        assert!(
            !unknown
                .rendered
                .contains("No orchestrations are defined here"),
            "must not claim the repo has none:\n{}",
            unknown.rendered
        );

        // Genuinely none: no config file at all.
        let tmp = crate::test_temp::tempdir().unwrap();
        let none = list_targets_response(Some(tmp.path()));
        assert!(none.error.is_none(), "an absent config is not an error");
        assert!(none.rendered.contains("No orchestrations are defined here"));

        // Broken config: named, and flagged as an error.
        let bad = crate::test_temp::tempdir().unwrap();
        std::fs::write(
            bad.path().join(".dot-agent-deck.toml"),
            "[[orchestrations]]\nname = \"unterminated\n",
        )
        .unwrap();
        let broken = list_targets_response(Some(bad.path()));
        assert!(
            broken.error.is_some(),
            "an unparseable config must not read as 'no orchestrations':\n{}",
            broken.rendered
        );
        assert!(broken.rendered.contains(".dot-agent-deck.toml"));

        // Present and parseable: listed structurally as well as rendered.
        let good = crate::test_temp::tempdir().unwrap();
        std::fs::write(
            good.path().join(".dot-agent-deck.toml"),
            "[[orchestrations]]\nname = \"digest\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations.roles]]\nname = \"worker\"\ncommand = \"sh\"\n",
        )
        .unwrap();
        let ok = list_targets_response(Some(good.path()));
        assert!(ok.error.is_none());
        assert_eq!(ok.orchestrations.len(), 1);
        assert_eq!(ok.orchestrations[0].name, "digest");
        assert_eq!(ok.orchestrations[0].roles, 2);
    }

    /// An ORCHESTRATION dispatch must start the team WITH its delegation protocol.
    ///
    /// This is the defect reported from real use: the orchestration came up, its
    /// orchestrator received the task, and every worker sat idle — because the daemon
    /// spawn path never composed the orchestrator context that the interactive
    /// `Ctrl+n` path writes, so the orchestrator was never told it was one or how to
    /// `delegate`. Asserted on the CONTEXT FILE in the dispatched worktree, which is
    /// the artefact that was missing entirely.
    ///
    /// Roles run `cat` (alive on stdin, no LLM tokens), mirroring the `orch-deck`
    /// fixture.
    // Gated to the e2e tier: this spawns REAL PTYs and awaits the prompt-delivery
    // readiness gate, so it costs ~30s — too slow for the per-task fast gate, and
    // not a unit test by any honest reading.
    #[cfg(feature = "e2e")]
    #[tokio::test]
    async fn an_orchestration_dispatch_writes_the_delegation_protocol_and_the_task() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        std::fs::write(
            repo.join(".dot-agent-deck.toml"),
            "[[orchestrations]]\nname = \"demo-orch\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations.roles]]\nname = \"worker\"\ncommand = \"cat\"\ndescription = \"Does the work\"\n",
        )
        .unwrap();
        // The config must be COMMITTED: the shape is resolved from the caller's repo,
        // but the worktree the roles run in is a HEAD checkout.
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git available");
        };
        run(&["add", "-A"]);
        run(&["commit", "-qm", "add orchestration"]);

        // issue #490's clone gate now runs unconditionally near the top of
        // `handle_dispatch`, before any worktree provisioning -- without a
        // reachable daemon it fails closed and returns before the orchestrator
        // context/delegation-protocol behavior this test pins is ever reached.
        // Stub a daemon reporting no live sibling so the gate takes its
        // `Ok(false)` branch and falls through to the original path.
        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: None,
            // These unit tests assert on the worktree + spawn shape, not on
            // delegate routing (`orchestration/dispatch/001` owns that).
            state: None,
        };

        let result = handle_dispatch(
            &ctx,
            "team-unit",
            "Verify PR #232 and report back.",
            Some(&crate::event::DispatchShape::Orchestration { name: None }),
        )
        .await;

        let worktree = result.worktree_dir.clone();
        // Reclaim the sibling worktree regardless of the assertions below.
        struct Guard(std::path::PathBuf);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Guard(worktree.clone());

        assert!(
            result.success,
            "the orchestration dispatch should succeed, got: {}",
            result.message
        );
        assert!(
            result.message.contains("orchestration"),
            "the reported shape must say orchestration, got: {}",
            result.message
        );

        let context = worktree.join(".dot-agent-deck/orchestrator-context.md");
        let content = std::fs::read_to_string(&context).unwrap_or_else(|e| {
            panic!(
                "the dispatched orchestration must get an orchestrator-context.md at {} \
                 (its absence is exactly why workers sat idle): {e}",
                context.display()
            )
        });
        assert!(
            content.contains("Delegation protocol"),
            "the orchestrator must be told HOW to delegate:\n{content}"
        );
        assert!(
            content.contains("worker") && content.contains("Does the work"),
            "the orchestrator must be told WHICH agents exist:\n{content}"
        );
        assert!(
            content.contains("## Your task") && content.contains("Verify PR #232"),
            "the caller's task must ride inside the context file:\n{content}"
        );
    }

    /// Scenario: fork issue #595 fix round 2 (reviewer F3), extended in fix
    /// round 3 (reviewer N4). `ctx.working_dir` is the calling pane's own
    /// registered cwd, which — after the #595 fix to `src/ui.rs` — can
    /// legitimately be a NESTED subdirectory of its repo's toplevel (any
    /// pane already running inside a resolved isolated clone at a nested
    /// prefix). Left unresolved, `derive_dispatch_paths` placed the
    /// dispatched worktree as a sibling of that nested directory — a full
    /// clone of the whole repo materialized INSIDE the calling pane's own
    /// working tree, the same defect class F1 fixed in `src/ui.rs`'s
    /// `Action::SpawnPane`, one level down. Dispatches from a nested
    /// working dir and asserts both halves of F3: the resulting worktree
    /// is a sibling of the repo TOPLEVEL, not nested under it, AND the
    /// dispatched agent's own cwd reproduces the calling pane's nested
    /// position inside that worktree rather than landing at its root —
    /// the real `dispatch_working_dir` value, recorded in the registry
    /// only when the dispatched process genuinely starts, which needs a
    /// command that actually execs (`cat`, alive on stdin) rather than the
    /// round-2 test's nonexistent-binary stand-in. `cat` as the stand-in
    /// agent, matching `dispatch_shares_the_checkout_when_no_live_sibling_exists`
    /// above (a single-agent dispatch, no `#[cfg(unix)]` needed there).
    #[tokio::test]
    async fn dispatch_from_a_nested_working_dir_places_the_worktree_outside_the_source_repo() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let nested = repo.join("baseline").join("intent");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("marker.txt"), "hi\n").unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git available");
        };
        run(&["add", "-A"]);
        run(&["commit", "-qm", "seed nested project"]);

        // Same gate-stub as every other `handle_dispatch` test above: no live
        // sibling, so the has_live_sibling gate takes its `Ok(false)` branch.
        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: nested.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "nested-unit-595", "task", None).await;
        let worktree = result.worktree_dir.clone();
        struct Guard(std::path::PathBuf, Arc<AgentPtyRegistry>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.1.shutdown_all();
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Guard(worktree.clone(), ctx.registry.clone());

        assert!(
            result.success,
            "setup: the dispatch itself must succeed for this test to exercise anything -- \
             got: {}",
            result.message
        );
        assert!(
            !worktree.starts_with(&repo),
            "reviewer F3: dispatching from a nested working_dir must place the worktree as a \
             sibling of the repo TOPLEVEL, never nested inside the source repo's own working \
             tree -- got {worktree:?} under {repo:?}"
        );

        // Reviewer N4: F3's second half — the dispatched agent's OWN cwd
        // must reproduce the calling pane's nested position inside the new
        // worktree (`<worktree>/baseline/intent`), not the worktree root.
        // This was implemented (`dispatch_working_dir`, above) but
        // previously asserted nowhere — the placement check above pins
        // only where the WORKTREE landed, not where the agent inside it
        // was actually started.
        let live = ctx.registry.agent_records();
        assert_eq!(
            live.len(),
            1,
            "expected exactly one dispatched agent record; got {:?}",
            live.iter()
                .map(|r| (r.display_name.clone(), r.cwd.clone()))
                .collect::<Vec<_>>()
        );
        let expected_cwd = worktree.join("baseline").join("intent");
        assert_eq!(
            live[0].cwd.as_deref(),
            Some(expected_cwd.to_string_lossy().as_ref()),
            "the dispatched agent's cwd must be the calling pane's own nested position inside \
             the new worktree, not the worktree root"
        );
    }

    /// Scenario: fork issue #595 fix round 4 (reviewer N7 / auditor S1),
    /// the `src/dispatch.rs` mirror of `workspace_036` in `src/ui.rs` --
    /// see that test's comment for the full reasoning. `ctx.working_dir`
    /// reaches an in-repo root symlink (planted inside the repo, pointing
    /// back at its own root -- the shape round 3 already covers) through a
    /// SYMLINKED ANCESTOR above the repo, so `relative_subpath` still comes
    /// out empty. Round 3's containment guard falls back to the derived
    /// worktree dir's own raw spelling when `.canonicalize()` fails (since
    /// it doesn't exist on disk yet), which shares no textual prefix with
    /// the canonicalized toplevel in this shape either, so the guard stays
    /// silent and the worktree lands inside the source repo's own working
    /// tree. Dispatches from that picked path and asserts the resulting
    /// worktree is a sibling of the repo TOPLEVEL instead.
    #[tokio::test]
    async fn dispatch_from_a_symlinked_ancestor_closes_the_containment_guard_gap() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let real_root = tmp.path().join("real");
        let repo = real_root.join("repo");
        init_repo(&repo);
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let rootlink = sub.join("rootlink");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&repo, &rootlink).expect("symlink rootlink -> repo");
        #[cfg(not(unix))]
        std::os::windows::fs::symlink_dir(&repo, &rootlink).expect("symlink rootlink -> repo");

        let ancestor_link = tmp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_root, &ancestor_link).expect("symlink link -> real_root");
        #[cfg(not(unix))]
        std::os::windows::fs::symlink_dir(&real_root, &ancestor_link)
            .expect("symlink link -> real_root");

        let picked = ancestor_link.join("repo").join("sub").join("rootlink");

        // Same gate-stub as every other `handle_dispatch` test above: no live
        // sibling, so the has_live_sibling gate takes its `Ok(false)` branch.
        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: picked.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "symlinked-ancestor-595", "task", None).await;
        let worktree = result.worktree_dir.clone();
        struct Guard(std::path::PathBuf, Arc<AgentPtyRegistry>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.1.shutdown_all();
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Guard(worktree.clone(), ctx.registry.clone());

        assert!(
            result.success,
            "setup: the dispatch itself must succeed for this test to exercise anything -- \
             got: {}",
            result.message
        );

        let canonical_repo = repo.canonicalize().expect("canonicalize repo");
        let canonical_worktree = worktree.canonicalize().unwrap_or_else(|_| worktree.clone());
        assert!(
            !canonical_worktree.starts_with(&canonical_repo),
            "reviewer N7 / auditor S1: dispatching from a picked path reached through a \
             symlinked ancestor must place the worktree as a sibling of the repo TOPLEVEL, \
             never nested inside (or reproducing) the source repo's own working tree -- got \
             {canonical_worktree:?} under {canonical_repo:?}"
        );
    }

    /// Issues #575 and #600 — the partial-orchestration dispatch, at the altitude
    /// the user meets it: one role's command is wrong, the dispatch reports
    /// failure, and the roles that DID start are left running as orphans in a
    /// directory the rollback then deletes underneath them.
    ///
    /// Roles 0 and 1 run `cat` (alive on stdin, no LLM tokens); role 2 is an
    /// unresolvable absolute path, which is the cheapest way to make exactly one
    /// later role fail. Three roles rather than two so the failure is genuinely
    /// "a later role", with more than one survivor behind it.
    ///
    /// Pre-fix RED on the orphan assertions: `spawn` `?`s out of its role loop, so
    /// the two `cat` children stay live in the registry with no `SpawnHandle` for
    /// the caller to close them with (#600), while `handle_dispatch`'s rollback
    /// force-removes the worktree those children are rooted in (#575).
    // `cat` as the stand-in role, and POSIX spawn/termination semantics: the
    // fast tier runs on Windows CI too, where a bare `cat` fails to exec and
    // would turn "a LATER role failed" into "the first role failed" — the test
    // would still pass, for none of the reasons it exists.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_partial_orchestration_dispatch_leaves_no_orphans_and_no_deleted_cwd() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        std::fs::write(
            repo.join(".dot-agent-deck.toml"),
            "[[orchestrations]]\nname = \"partial-orch\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations.roles]]\nname = \"worker-one\"\ncommand = \"cat\"\n\n\
             [[orchestrations.roles]]\nname = \"worker-two\"\n\
             command = \"/nonexistent/dot-agent-deck-575\"\n",
        )
        .unwrap();
        // The shape is resolved from the CALLER's repo, but the roles run in a HEAD
        // checkout, so the config has to be committed.
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git available");
        };
        run(&["add", "-A"]);
        run(&["commit", "-qm", "add orchestration"]);

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let state: crate::state::SharedState =
            Arc::new(tokio::sync::RwLock::new(crate::state::AppState::default()));
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: None,
            state: Some(state.clone()),
        };

        let result = handle_dispatch(
            &ctx,
            "partial-unit",
            "Verify the partial-spawn rollback.",
            Some(&crate::event::DispatchShape::Orchestration { name: None }),
        )
        .await;

        // Reclaim the sibling worktree regardless of the assertions below.
        struct Guard(std::path::PathBuf, Arc<AgentPtyRegistry>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.1.shutdown_all();
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Guard(result.worktree_dir.clone(), ctx.registry.clone());

        assert!(
            !result.success,
            "precondition: role 2 cannot be exec'd, so the dispatch must fail: {}",
            result.message
        );

        // #600: nothing the failed dispatch started may outlive it. The caller got
        // no `SpawnHandle`, so anything still live here is unreachable by any
        // close path the user has.
        let live = ctx.registry.agent_records();
        assert!(
            live.is_empty(),
            "a failed dispatch must leave no live agents behind; found {} orphan(s): {:?}",
            live.len(),
            live.iter()
                .map(|r| (r.display_name.clone(), r.cwd.clone()))
                .collect::<Vec<_>>()
        );

        // …and no routing state for panes that no longer exist.
        let guard = state.read().await;
        assert!(
            guard.pane_role_map.is_empty(),
            "the rolled-back roles must leave no role-map entries: {:?}",
            guard.pane_role_map
        );
        assert!(
            guard.orchestrator_pane_ids.is_empty(),
            "…nor an orchestrator marker: {:?}",
            guard.orchestrator_pane_ids
        );
        drop(guard);

        // #575: the rollback may only reclaim the tree once nothing is rooted in
        // it — which, after the teardown above, is the case, so the slot is freed
        // exactly as it was for a spawn that never started an agent at all.
        assert!(
            !crate::issue_dispatch_run::worktree_still_in_use(
                &ctx.registry.agent_records(),
                &result.worktree_dir
            ),
            "no agent may still be rooted in the dispatched worktree"
        );
        assert!(
            !result.worktree_dir.exists(),
            "with nothing live in it, the worktree must still be reclaimed"
        );
        assert!(
            !branch_exists(&repo, "agent/dispatch-partial-unit"),
            "…and its branch deleted, so the name is not wedged"
        );
    }

    /// A shape the repo cannot satisfy must be refused BEFORE any git work, so a
    /// typo leaves no worktree or branch behind and is not reported as a spawn
    /// failure.
    #[tokio::test]
    async fn an_unknown_orchestration_name_is_refused_without_creating_a_worktree() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: None,
            // These unit tests assert on the worktree + spawn shape, not on
            // delegate routing (`orchestration/dispatch/001` owns that).
            state: None,
        };
        let result = handle_dispatch(
            &ctx,
            "typo-unit",
            "task",
            Some(&crate::event::DispatchShape::Orchestration {
                name: Some("revew".into()),
            }),
        )
        .await;

        assert!(!result.success);
        assert!(
            result.message.contains("revew"),
            "the message must name the requested target: {}",
            result.message
        );
        assert!(
            !result.message.contains("spawn failed"),
            "a validation error must not masquerade as a spawn failure: {}",
            result.message
        );
        assert!(
            !result.worktree_dir.exists(),
            "no worktree may be created for a shape that was refused"
        );
        assert!(
            !branch_exists(&repo, "agent/dispatch-typo-unit"),
            "no branch may be left behind either"
        );
    }

    // --- issue #469: ownership/liveness gate on the spawn-rollback path ---

    /// Scenario: issue #469. `handle_dispatch`'s spawn-rollback arm
    /// force-removes the worktree it just created whenever `spawn()` fails —
    /// but for a multi-role orchestration, an earlier role can already be a
    /// live PTY child rooted in that same worktree by the time a later
    /// role's spawn fails, and today's unconditional force-removal yanks the
    /// worktree out from under it. This fakes that state directly: a live
    /// sibling agent is registered with a `cwd` matching the worktree
    /// `handle_dispatch` is about to create, and the dispatch's own agent
    /// command is pointed at a binary that cannot exist, so `spawn()` fails
    /// deterministically and fast. With a live sibling still rooted there,
    /// the worktree, its branch, and the registry entry must all survive.
    #[tokio::test]
    async fn spawn_rollback_skips_cleanup_when_a_live_sibling_still_roots_the_worktree() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "guard-trip-unit");

        // issue #490's clone gate now runs unconditionally near the top of
        // `handle_dispatch`, before any worktree provisioning -- without a
        // reachable daemon it fails closed and returns before the #469
        // rollback logic this test pins is ever reached. Stub a daemon
        // reporting no live sibling so the gate takes its `Ok(false)` branch
        // and falls through to the original `create_worktree`/`spawn()` path.
        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );

        // The worktree doesn't exist yet -- `handle_dispatch` creates it --
        // but a real PTY-backed sibling needs a real cwd to spawn into, so
        // create the plain (non-worktree) directory ahead of it. `git
        // worktree add` succeeds into a pre-existing EMPTY directory, so
        // this does not trip the `AlreadyClaimed` path.
        std::fs::create_dir_all(&paths.worktree_dir).unwrap();
        let registry = Arc::new(AgentPtyRegistry::new());
        // The real defect is a multi-role orchestration: `worktree_of_record`
        // resolves a role pane's worktree via `TabMembership::Orchestration`'s
        // `orchestration_cwd`, not via the record's own `cwd` (reviewer F3) --
        // so the sibling must carry that membership shape, not a bare `cwd`,
        // to exercise the same resolution arm the real code path uses.
        let sibling_id = registry
            .spawn_agent(crate::agent_pty::SpawnOptions {
                cwd: Some(&paths.worktree_dir.to_string_lossy()),
                tab_membership: Some(crate::agent_pty::TabMembership::Orchestration {
                    name: "guard-trip-unit".to_string(),
                    role_index: 0,
                    role_name: "coder".to_string(),
                    is_start_role: false,
                    orchestration_cwd: Some(paths.worktree_dir.to_string_lossy().into_owned()),
                    display_title: None,
                    orchestration_id: None,
                }),
                ..crate::agent_pty::SpawnOptions::default()
            })
            .expect("spawn a live sibling agent rooted in the about-to-be-created worktree");

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: registry.clone(),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("/definitely-not-a-real-binary-xyz-469".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "guard-trip-unit", "task", None).await;

        // Close the sibling shell before any assertion can panic (reviewer
        // F5a): a failing `assert!` below must not leak a real `setsid`'d
        // process past a `let _ =` that never runs.
        let _ = registry.close_agent(&sibling_id);

        assert!(
            !result.success,
            "the spawn itself must still report failure"
        );
        assert!(
            paths.worktree_dir.exists(),
            "a live sibling still rooted in the worktree must keep it from being \
             force-removed (issue #469): {}",
            result.message
        );
        assert!(
            branch_exists(&repo, &paths.branch),
            "the branch must survive too -- a still-live sibling may hold committed work \
             whose only record is this branch"
        );
        assert!(
            ctx.worktrees
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&paths.worktree_dir),
            "the registry entry must survive so a later close can still find and clean it up"
        );
        assert!(
            result.message.contains("cleanup skipped"),
            "the skip message must name the cause so an operator doesn't reach for a \
             manual force-removal: {}",
            result.message
        );

        let _ = std::fs::remove_dir_all(&paths.worktree_dir);
    }

    /// Scenario: issue #469 regression guard. Same forced-failure setup as
    /// `spawn_rollback_skips_cleanup_when_a_live_sibling_still_roots_the_worktree`,
    /// but with NO live sibling registered: the worktree, its branch, and the
    /// registry entry must all be gone, exactly as
    /// `force_removes_a_dirty_worktree_regardless_of_uncommitted_work`
    /// already proves for `remove_worktree` directly -- this proves the SAME
    /// thing reached through `handle_dispatch`'s actual failure arm, which
    /// nothing exercised end-to-end before this issue.
    #[tokio::test]
    async fn spawn_rollback_force_removes_when_nothing_else_roots_the_worktree() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "guard-notrip-unit");

        // See the sibling `..._skips_cleanup...` test above: without a
        // reachable daemon, issue #490's gate fails closed before the #469
        // force-removal arm this test pins is ever reached, and the
        // assertions below would pass vacuously (nothing was ever created)
        // rather than because removal actually ran. Stub a daemon reporting
        // no live sibling so the gate falls through to the original path.
        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("/definitely-not-a-real-binary-xyz-469".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "guard-notrip-unit", "task", None).await;

        assert!(!result.success);
        assert!(
            result.message.contains("spawn failed"),
            "the rollback arm must actually have been reached -- every other assertion \
             here is a negative that a fail-closed early return (creating nothing) would \
             also satisfy, so this is the one assertion that distinguishes a genuine \
             force-removal from the gate refusing before it ever got there: {}",
            result.message
        );
        assert!(
            !paths.worktree_dir.exists(),
            "with nothing else rooted there, the worktree must still be force-removed \
             exactly as before: {}",
            result.message
        );
        assert!(
            !branch_exists(&repo, &paths.branch),
            "the branch must still be deleted too"
        );
        assert!(
            !ctx.worktrees
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&paths.worktree_dir),
            "the registry entry must still be dropped"
        );
    }

    /// Scenario: issue #473 regression guard. Forces the shared-checkout
    /// rollback arm's `git worktree remove --force` to genuinely fail (the
    /// worktree is locked the instant `git worktree add` creates it, via a
    /// `PATH`-shimmed `git`), then asserts the CORRECT post-rollback
    /// behavior: the registry entry for the worktree must still be present,
    /// since removal did not actually succeed. `dispatch.rs` now matches on
    /// `remove_worktree`'s `RemoveOutcome` instead of discarding it
    /// (`let _ = remove_worktree(...)`) and only drops the registry entry
    /// when removal actually succeeded, restoring the guarantee
    /// `RemoveOutcome::RemoveFailed` exists to provide (see its own doc
    /// comment, PRD 236 review).
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_rollback_retains_registry_entry_when_force_removal_fails() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "lockfail-unit");

        // Same daemon-gate bypass as the sibling test above -- without it,
        // issue #490's live-sibling gate fails closed before the rollback
        // arm this test targets is ever reached.
        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );

        // Force the rollback's eventual `git worktree remove --force` to
        // genuinely fail: a `PATH`-shimmed `git` locks the worktree the
        // moment `git worktree add` creates it -- synchronously, inside the
        // same subprocess call `create_worktree` awaits, so there is no
        // timing race against `handle_dispatch`'s later spawn-then-rollback
        // steps. `git worktree remove --force` refuses a locked worktree
        // even with a single `--force` (`remove_worktree_argv` only ever
        // pushes one) -- verified directly against git 2.55.0, matching
        // `RemoveOutcome::RemoveFailed`'s own doc comment on how PRD 236
        // originally reproduced this.
        let _git_stub = with_git_worktree_add_locking_the_new_worktree(tmp.path());

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("/definitely-not-a-real-binary-xyz-473".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "lockfail-unit", "task", None).await;

        assert!(!result.success);
        assert!(
            result.message.contains("spawn failed"),
            "the rollback arm must actually have been reached -- every other assertion \
             here is a negative that a fail-closed early return (creating nothing) would \
             also satisfy, so this is the one assertion that distinguishes a genuine \
             rollback attempt from the gate refusing before it ever got there: {}",
            result.message
        );
        assert!(
            paths.worktree_dir.exists(),
            "removal was forced to fail (the worktree is locked) -- the directory must \
             still be on disk: {}",
            result.message
        );
        assert!(
            ctx.worktrees
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&paths.worktree_dir),
            "issue #473: the registry entry for {} must still be present -- removal \
             genuinely failed (locked worktree), so dropping the entry anyway loses the \
             only record that this worktree is still on disk: {}",
            paths.worktree_dir.display(),
            result.message
        );
        assert!(
            result.message.contains("cleanup failed"),
            "reviewer P2-3: every other assertion here is a positive that the earlier \
             `worktree_still_in_use` early-return would also satisfy, so this is the one \
             assertion that proves the rollback arm was genuinely reached (not the early \
             return) AND pins the second half of the behavior change -- that \
             `cleanup_failed` is actually set to true here, not just that the registry \
             retains the entry: {}",
            result.message
        );

        // Best-effort cleanup: `tmp`'s own Drop removes everything under it
        // regardless, but unlock first so a leftover admin lock file cannot
        // confuse anything that inspects `repo/.git/worktrees/` before then.
        let _ = std::process::Command::new("git")
            .args([
                "-C",
                &repo.to_string_lossy(),
                "worktree",
                "unlock",
                "--",
                &paths.worktree_dir.to_string_lossy(),
            ])
            .output();
        let _ = std::fs::remove_dir_all(&paths.worktree_dir);
    }

    // --- issue #490 (PRD fork#325 M3, Model B): the live-sibling clone gate ---
    //
    // Model A's equivalent (`src/ui.rs`'s `Action::SpawnPane` handler) consults
    // `root_checkout_has_live_sibling` -- a daemon `ListAgents` round trip -- and
    // branches: no live sibling => ordinary shared-checkout worktree; a live
    // sibling already sharing the target's `--git-common-dir` => an isolated
    // clone instead (`provision_isolated_clone_sync`); the daemon query itself
    // failing or answering untrustworthily => fail CLOSED, refuse to provision
    // at all. `handle_dispatch` now has the equivalent check (this PR), so
    // the three cases below each take their own distinct branch: an ordinary
    // shared-checkout sibling, an isolated fresh clone, or a fail-closed
    // refusal, matching what a live sibling or a failing daemon implies.

    /// RAII guard, restoring `DOT_AGENT_DECK_ATTACH_SOCKET` and
    /// `DOT_AGENT_DECK_SESSION_START_WAIT_MS` to their previous values on drop
    /// -- held under `crate::config::STATE_DIR_ENV_LOCK` for its whole
    /// lifetime, same env-var-mutation lock every other env-mutating test in
    /// this codebase uses.
    struct CraftedAttachDaemonGuard {
        _env_lock: std::sync::MutexGuard<'static, ()>,
        prev_attach: Option<String>,
        prev_session_start_wait: Option<String>,
    }

    impl Drop for CraftedAttachDaemonGuard {
        fn drop(&mut self) {
            // SAFETY: `_env_lock` is held for this guard's entire lifetime.
            unsafe {
                match self.prev_attach.take() {
                    Some(v) => std::env::set_var("DOT_AGENT_DECK_ATTACH_SOCKET", v),
                    None => std::env::remove_var("DOT_AGENT_DECK_ATTACH_SOCKET"),
                }
                match self.prev_session_start_wait.take() {
                    Some(v) => std::env::set_var("DOT_AGENT_DECK_SESSION_START_WAIT_MS", v),
                    None => std::env::remove_var("DOT_AGENT_DECK_SESSION_START_WAIT_MS"),
                }
            }
        }
    }

    /// Stand up a stub attach daemon that answers every `ListAgents` request
    /// with a CALLER-SUPPLIED `AttachResponse`, and point
    /// `DOT_AGENT_DECK_ATTACH_SOCKET` at it -- the same hand-rolled
    /// frame-exchange pattern `src/ui.rs`'s `with_crafted_response_daemon`
    /// uses (copied rather than reused: that helper is private to `ui.rs`'s
    /// own test module). Also pins
    /// `DOT_AGENT_DECK_SESSION_START_WAIT_MS` to its documented floor (100ms)
    /// so a real `cat`-backed dispatch through this daemon does not pay the
    /// production 30s no-hook fallback -- the same override
    /// `tests/delegate_prompt_injection.rs` already relies on to keep a real
    /// `cat` spawn in the fast tier.
    fn with_crafted_attach_daemon(
        unique_dir: &Path,
        response: crate::daemon_protocol::AttachResponse,
    ) -> CraftedAttachDaemonGuard {
        let env_lock = crate::config::STATE_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_attach = std::env::var("DOT_AGENT_DECK_ATTACH_SOCKET").ok();
        let prev_session_start_wait = std::env::var("DOT_AGENT_DECK_SESSION_START_WAIT_MS").ok();

        #[cfg(unix)]
        let socket_addr = unique_dir.join("attach.sock");
        #[cfg(windows)]
        let socket_addr = std::path::PathBuf::from(format!(
            r"\\.\pipe\dot-agent-deck-test-{}",
            unique_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread_socket_addr = socket_addr.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!(
                        "build stub crafted-response daemon runtime: {e}"
                    )));
                    return;
                }
            };
            rt.block_on(async move {
                let listener =
                    match crate::daemon_protocol::bind_attach_listener(&thread_socket_addr) {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = ready_tx
                                .send(Err(format!("bind stub crafted-response listener: {e}")));
                            return;
                        }
                    };
                let _ = ready_tx.send(Ok(()));
                let payload =
                    serde_json::to_vec(&response).expect("serialize crafted AttachResponse");
                loop {
                    let Ok(mut stream) = listener.accept().await else {
                        return;
                    };
                    let _ = crate::daemon_protocol::read_frame(&mut stream).await;
                    let _ = crate::daemon_protocol::write_frame(
                        &mut stream,
                        crate::daemon_protocol::KIND_RESP,
                        &payload,
                    )
                    .await;
                }
            });
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("stub crafted-response daemon must report readiness within 10s")
            .expect("stub crafted-response daemon must bind successfully");

        // SAFETY: `env_lock` held above; restored by `CraftedAttachDaemonGuard::drop`.
        unsafe {
            std::env::set_var("DOT_AGENT_DECK_ATTACH_SOCKET", &socket_addr);
            std::env::set_var("DOT_AGENT_DECK_SESSION_START_WAIT_MS", "100");
        }

        CraftedAttachDaemonGuard {
            _env_lock: env_lock,
            prev_attach,
            prev_session_start_wait,
        }
    }

    /// An `AgentRecord` naming a live orchestration whose tab is rooted at
    /// `orchestration_cwd` -- the shape `root_checkout_has_live_sibling`
    /// reads out of `ListAgents`. Every other field is the harmless default a
    /// real record would carry when unset.
    fn live_orchestration_record(orchestration_cwd: &Path) -> crate::agent_pty::AgentRecord {
        crate::agent_pty::AgentRecord {
            id: "live-sibling-agent".to_string(),
            pane_id_env: None,
            display_name: None,
            cwd: None,
            tab_membership: Some(crate::agent_pty::TabMembership::Orchestration {
                name: "live-sibling".to_string(),
                role_index: 0,
                role_name: "orchestrator".to_string(),
                is_start_role: true,
                orchestration_cwd: Some(orchestration_cwd.to_string_lossy().into_owned()),
                display_title: None,
                orchestration_id: None,
            }),
            agent_type: None,
            rows: 0,
            cols: 0,
            live: None,
            spawned_at_ms: None,
            daemon_boot_id: None,
            registration_generation: None,
            outstanding_delegation: None,
            silence_watch: None,
            delegation_commission: None,
        }
    }

    /// Locate the real `git` binary via `command -v`, so a `PATH`-shimmed
    /// fake `git` (below) can still delegate every call it doesn't care
    /// about to the genuine implementation, regardless of where CI's `git`
    /// actually lives.
    #[cfg(unix)]
    fn real_git_path() -> String {
        let out = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate the real git binary via `command -v`");
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(!path.is_empty(), "no real git binary found on PATH");
        path
    }

    /// RAII guard restoring `PATH` to its prior value on drop. Shared by
    /// both `PATH`-shimmed-`git` helpers below.
    struct FakeGitOnPathGuard {
        prev_path: String,
    }

    impl Drop for FakeGitOnPathGuard {
        fn drop(&mut self) {
            // SAFETY: `PATH` is process-global, but per CLAUDE.md rule 5's
            // fork addendum every test run happens in CI via `cargo
            // nextest`, which runs each test in its OWN process (the same
            // justification `git_common_dir_async_is_bounded_by_an_external_timeout`
            // already relies on for its own `PATH` mutation) -- so no
            // sibling test observes this. Restored unconditionally here.
            unsafe {
                std::env::set_var("PATH", &self.prev_path);
            }
        }
    }

    /// Prepend a fake `git` to `PATH` that fails ONLY `git remote set-url`
    /// invocations -- `point_isolated_clone_origin`'s exact command -- and
    /// passes every other invocation straight through to the real `git`.
    /// Lets a test force `IsolatedCloneOutcome::Created { origin_warning:
    /// Some(_), .. }` deterministically: `point_isolated_clone_origin`'s
    /// failure branch otherwise requires a filesystem-permission race
    /// against `provision_isolated_clone_sync`'s own single synchronous
    /// call, which this sidesteps entirely.
    ///
    /// Final cleanup round (nit 7): this stub matches `$1`/`$2` positionally
    /// -- the same fragility `e663edf2` had to fix elsewhere in this round.
    /// Safe today only because `point_isolated_clone_origin`'s real call
    /// site is a raw `std::process::Command`, not the shared hardened core
    /// (`spawn_git_status_child` et al.), which prepends `-c core.fsmonitor=`
    /// and would shift `remote`/`set-url` off positions `$1`/`$2`. If that
    /// call site is ever routed through the shared core, this stub silently
    /// stops matching and needs updating alongside it.
    #[cfg(unix)]
    fn with_git_remote_set_url_failing(scratch: &Path) -> FakeGitOnPathGuard {
        use std::os::unix::fs::PermissionsExt;

        let real_git = real_git_path();
        let bindir = scratch.join("git-remote-set-url-stub-bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let stub = bindir.join("git");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = remote ] && [ \"$2\" = set-url ]; then\n\
                 \x20\x20echo 'stub: simulated git remote set-url failure' >&2\n\
                 \x20\x20exit 1\n\
                 fi\n\
                 exec {real_git} \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prev_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: see `FakeGitOnPathGuard::drop`.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prev_path}", bindir.display()));
        }
        FakeGitOnPathGuard { prev_path }
    }

    /// Prepend a fake `git` to `PATH` that fails ONLY `git clone`
    /// invocations, writing stderr containing a raw terminal-hostile control
    /// byte (`ESC`, as part of an ANSI color escape) before exiting
    /// non-zero -- simulating a hostile or corrupted repository/remote
    /// emitting escape sequences into git's own captured error output --
    /// and passes every other invocation through to the real `git`. Creates
    /// the destination directory (with a `.git` marker) before failing so
    /// `handle_isolated_clone_add_error` takes its ordinary
    /// leaves-a-half-created-directory `Failed` path rather than the
    /// `!clone_dir.exists()` early `Err` branch.
    ///
    /// PRD fork#544 review-findings fix round 3: detects `clone` as ANY
    /// argument, not `$1` positionally -- `spawn_git_status_child`'s new
    /// `-c core.fsmonitor=` hardening now prepends two global-option args
    /// ahead of the subcommand on every call through that shared core
    /// (`provision_isolated_clone_sync`'s own `git clone` included), so
    /// `$1` is `-c`, not `clone`, on the invocation this test actually
    /// drives. A positional check silently fell through to `exec
    /// {real_git}`, which then genuinely cloned `repo` (a valid
    /// repository) instead of simulating a failure -- this stub's job is
    /// to recognize an invocation AS a clone regardless of what global
    /// options precede the subcommand, the same way real `git` itself
    /// does.
    #[cfg(unix)]
    fn with_git_clone_failing_with_hostile_stderr(scratch: &Path) -> FakeGitOnPathGuard {
        use std::os::unix::fs::PermissionsExt;

        let real_git = real_git_path();
        let bindir = scratch.join("git-clone-fail-stub-bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let stub = bindir.join("git");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 is_clone=0\n\
                 for a in \"$@\"; do\n\
                 \x20\x20if [ \"$a\" = clone ]; then is_clone=1; fi\n\
                 done\n\
                 if [ \"$is_clone\" = 1 ]; then\n\
                 \x20\x20dest=\"\"\n\
                 \x20\x20for a in \"$@\"; do dest=\"$a\"; done\n\
                 \x20\x20mkdir -p \"$dest/.git\"\n\
                 \x20\x20printf 'fatal: simulated clone failure \\033[31mhostile\\033[0m\\n' >&2\n\
                 \x20\x20exit 128\n\
                 fi\n\
                 exec {real_git} \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prev_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: see `FakeGitOnPathGuard::drop`.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prev_path}", bindir.display()));
        }
        FakeGitOnPathGuard { prev_path }
    }

    /// Prepend a fake `git` to `PATH` that lets `git clone` genuinely
    /// succeed, then plants a chmod-500 subdirectory inside the freshly
    /// cloned `.git/` containing one file -- an entry `attempt_isolated_clone_cleanup`'s
    /// later `remove_dir_all` cannot unlink, forcing that call to genuinely
    /// fail (issue #563) without disturbing provisioning itself. Every
    /// OTHER invocation (the branch probes, the checkout, the origin
    /// fixup) passes straight through to the real `git`, and none of them
    /// ever touches this planted subdirectory, so provisioning completes
    /// exactly as it would without this stub -- only the LATER cleanup
    /// attempt, which has no git subprocess of its own to shim (unlike the
    /// shared-checkout arm's `git worktree remove`), fails.
    ///
    /// Word-scanned rather than positional, mirroring
    /// `with_git_clone_failing_with_hostile_stderr`'s own reasoning: matches
    /// wherever the literal word `clone` appears anywhere in argv (the
    /// `-c core.fsmonitor=` hardening `spawn_git_status_child` prepends to
    /// every call through `provision_isolated_clone_sync`'s shared core
    /// shifts `clone` off any fixed positional index), and reads the clone
    /// destination off the LAST argument, matching `git clone`'s own
    /// invocation shape (`clone --origin origin -- <source> <dest>`).
    #[cfg(unix)]
    fn with_git_clone_leaving_an_unremovable_entry(scratch: &Path) -> FakeGitOnPathGuard {
        use std::os::unix::fs::PermissionsExt;

        let real_git = real_git_path();
        let bindir = scratch.join("git-clone-unremovable-stub-bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let stub = bindir.join("git");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 is_clone=0\n\
                 for a in \"$@\"; do\n\
                 \x20\x20if [ \"$a\" = clone ]; then is_clone=1; fi\n\
                 done\n\
                 if [ \"$is_clone\" = 1 ]; then\n\
                 \x20\x20{real_git} \"$@\"\n\
                 \x20\x20status=$?\n\
                 \x20\x20if [ \"$status\" -eq 0 ]; then\n\
                 \x20\x20\x20\x20dest=\"\"\n\
                 \x20\x20\x20\x20for a in \"$@\"; do dest=\"$a\"; done\n\
                 \x20\x20\x20\x20mkdir -p \"$dest/.git/dad-563-unremovable\"\n\
                 \x20\x20\x20\x20: > \"$dest/.git/dad-563-unremovable/blocker\"\n\
                 \x20\x20\x20\x20chmod 500 \"$dest/.git/dad-563-unremovable\"\n\
                 \x20\x20fi\n\
                 \x20\x20exit \"$status\"\n\
                 fi\n\
                 exec {real_git} \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prev_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: see `FakeGitOnPathGuard::drop`.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prev_path}", bindir.display()));
        }
        FakeGitOnPathGuard { prev_path }
    }

    /// Prepend a fake `git` to `PATH` that LOCKS a worktree the instant `git
    /// worktree add` creates it -- synchronously, inside the same subprocess
    /// call `create_worktree`'s `run_status_killable_args` awaits, so there
    /// is no timing race against anything `handle_dispatch` does afterward
    /// (recording the worktree, attempting the spawn, or -- issue #473's
    /// target -- the rollback's own `git worktree remove --force`). Every
    /// other invocation, including that eventual removal attempt, passes
    /// straight through to the real `git`.
    ///
    /// Word-scanned rather than positional, mirroring
    /// `with_git_clone_failing_with_hostile_stderr`'s own reasoning: matches
    /// wherever `worktree` is immediately followed by `add`, and reads
    /// `clone_dir`/`worktree_dir` off the values immediately following
    /// `-C`/`add` respectively -- so the `-c core.fsmonitor=` hardening
    /// `spawn_git_status_child` prepends to every call through this shared
    /// core cannot shift the match off a fixed `$N` the way it did to an
    /// earlier, positional version of that sibling stub.
    #[cfg(unix)]
    fn with_git_worktree_add_locking_the_new_worktree(scratch: &Path) -> FakeGitOnPathGuard {
        use std::os::unix::fs::PermissionsExt;

        let real_git = real_git_path();
        let bindir = scratch.join("git-worktree-add-lock-stub-bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let stub = bindir.join("git");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 is_add=0\n\
                 clone_dir=\"\"\n\
                 worktree_dir=\"\"\n\
                 prev=\"\"\n\
                 for a in \"$@\"; do\n\
                 \x20\x20if [ \"$prev\" = \"-C\" ]; then\n\
                 \x20\x20\x20\x20clone_dir=\"$a\"\n\
                 \x20\x20fi\n\
                 \x20\x20if [ \"$prev\" = \"worktree\" ] && [ \"$a\" = \"add\" ]; then\n\
                 \x20\x20\x20\x20is_add=1\n\
                 \x20\x20fi\n\
                 \x20\x20if [ \"$prev\" = \"add\" ]; then\n\
                 \x20\x20\x20\x20worktree_dir=\"$a\"\n\
                 \x20\x20fi\n\
                 \x20\x20prev=\"$a\"\n\
                 done\n\
                 if [ \"$is_add\" = \"1\" ] && [ -n \"$worktree_dir\" ]; then\n\
                 \x20\x20{real_git} \"$@\"\n\
                 \x20\x20status=$?\n\
                 \x20\x20if [ \"$status\" -eq 0 ]; then\n\
                 \x20\x20\x20\x20{real_git} -C \"$clone_dir\" worktree lock -- \"$worktree_dir\" 1>&2\n\
                 \x20\x20fi\n\
                 \x20\x20exit \"$status\"\n\
                 fi\n\
                 exec {real_git} \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prev_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: see `FakeGitOnPathGuard::drop`.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prev_path}", bindir.display()));
        }
        FakeGitOnPathGuard { prev_path }
    }

    /// Prepend a fake `git` to `PATH` that fails ONLY `git branch -D`
    /// invocations -- the shared-checkout rollback arm's separate
    /// branch-deletion call -- and passes every other invocation, including
    /// its own `git worktree remove --force`, straight through to the real
    /// `git`. Lets a test force the worktree removal to genuinely succeed
    /// while the branch delete alone fails (issue #473 sibling gap, auditor
    /// A3).
    ///
    /// Word-scanned rather than positional (matches `branch` immediately
    /// followed by `-D` anywhere in argv), the same reasoning
    /// `with_git_worktree_add_locking_the_new_worktree` documents: the
    /// rollback's `branch -D` call goes through `run_status`, a raw
    /// `tokio::process::Command` with no global-option prefix today, but a
    /// positional match would silently stop matching if that ever changed.
    #[cfg(unix)]
    fn with_git_branch_delete_failing(scratch: &Path) -> FakeGitOnPathGuard {
        use std::os::unix::fs::PermissionsExt;

        let real_git = real_git_path();
        let bindir = scratch.join("git-branch-delete-fail-stub-bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let stub = bindir.join("git");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 prev=\"\"\n\
                 is_branch_delete=0\n\
                 for a in \"$@\"; do\n\
                 \x20\x20if [ \"$prev\" = \"branch\" ] && [ \"$a\" = \"-D\" ]; then\n\
                 \x20\x20\x20\x20is_branch_delete=1\n\
                 \x20\x20fi\n\
                 \x20\x20prev=\"$a\"\n\
                 done\n\
                 if [ \"$is_branch_delete\" = \"1\" ]; then\n\
                 \x20\x20echo 'stub: simulated git branch -D failure' >&2\n\
                 \x20\x20exit 1\n\
                 fi\n\
                 exec {real_git} \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prev_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: see `FakeGitOnPathGuard::drop`.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prev_path}", bindir.display()));
        }
        FakeGitOnPathGuard { prev_path }
    }

    /// Scenario: issue #473 sibling gap (auditor A3). Forces the
    /// shared-checkout rollback arm's worktree removal to succeed for real
    /// while the SEPARATE `git branch -D` call fails, via
    /// `with_git_branch_delete_failing`. Guards against
    /// `should_drop_registry = !cleanup_failed` -- a plausible-looking but
    /// wrong simplification the auditor reproduced directly: it would
    /// compile, pass every other rollback test, and silently reintroduce
    /// phantom registry retention for a worktree that is actually gone. The
    /// worktree here IS gone (only the branch delete failed), so the
    /// registry entry -- the only record of a still-on-disk worktree -- must
    /// still be dropped, even though `cleanup_failed` is separately true.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_rollback_drops_registry_entry_when_only_branch_delete_fails() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "branchfail-unit");

        // Same daemon-gate bypass every sibling rollback test in this file
        // uses -- without it, issue #490's live-sibling gate fails closed
        // before the rollback arm this test targets is ever reached.
        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );
        let _git_stub = with_git_branch_delete_failing(tmp.path());

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("/definitely-not-a-real-binary-xyz-473b".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "branchfail-unit", "task", None).await;

        assert!(!result.success);
        assert!(
            result.message.contains("spawn failed"),
            "the rollback arm must actually have been reached: {}",
            result.message
        );
        assert!(
            !paths.worktree_dir.exists(),
            "worktree removal itself must have genuinely succeeded (only the branch \
             delete was stubbed to fail): {}",
            result.message
        );
        assert!(
            branch_exists(&repo, &paths.branch),
            "the branch delete must have genuinely failed, or this test proves nothing \
             about the sibling gap it targets"
        );
        assert!(
            !ctx.worktrees
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&paths.worktree_dir),
            "issue #473 sibling gap: the worktree itself is gone, so the registry entry \
             must still be dropped even though the branch delete failed -- dropping the \
             registry must track whether the WORKTREE was removed, not whether cleanup as \
             a whole (including the branch delete) succeeded: {}",
            result.message
        );
        assert!(
            result.message.contains("cleanup failed"),
            "cleanup_failed must still be true, separately from should_drop_registry, \
             since the branch delete genuinely failed: {}",
            result.message
        );
    }

    /// Scenario: issue #490, case 1 -- regression guard. No live orchestration
    /// shares the target root checkout (the stub daemon answers `ListAgents`
    /// with an empty `agent_records`), so `handle_dispatch` must provision the
    /// worktree exactly as it does today: an ordinary `git worktree add`
    /// sibling of `ctx.working_dir`, sharing its git common dir. Uses a real
    /// (fast, `cat`-backed) spawn so the resulting worktree survives on disk
    /// to inspect, rather than a deliberately-failed one that would trigger
    /// `handle_dispatch`'s own rollback and remove it.
    #[tokio::test]
    async fn dispatch_shares_the_checkout_when_no_live_sibling_exists() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![]),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "gate-none-unit", "task", None).await;

        assert!(
            result.success,
            "with no live sibling, dispatch must still succeed: {}",
            result.message
        );
        assert!(
            result.worktree_dir.exists(),
            "with no live sibling, the worktree must be provisioned: {}",
            result.message
        );
        let repo_common = crate::issue_dispatch_run::git_common_dir(&repo)
            .expect("resolve the shared checkout's own common dir");
        let worktree_common = crate::issue_dispatch_run::git_common_dir(&result.worktree_dir)
            .expect("resolve the new worktree's common dir");
        assert_eq!(
            worktree_common, repo_common,
            "with no live sibling, the worktree must be an ordinary `git worktree add` \
             sibling sharing the checkout's own common dir -- an unaffected regression \
             guard, not new behavior"
        );
    }

    /// Scenario: issue #490, case 2 -- the actual gate. A live orchestration
    /// already has its OWN sibling worktree open against the same root
    /// checkout (mirroring the real #325 incident shape: `orchestration_cwd`
    /// is the live sibling's WORKTREE path, not `ctx.working_dir` itself, so
    /// only a `--git-common-dir` compare -- not raw path equality -- can see
    /// the collision). `handle_dispatch` must NOT create a second plain
    /// sibling of the shared checkout; it must isolate this dispatch into its
    /// own fresh clone instead, mirroring Model A's `Ok(true)` branch
    /// (`provision_isolated_clone_sync`), which this PR implements: the two
    /// common dirs below must compare DIFFERENT, proving the isolated-clone
    /// branch actually ran rather than the ordinary shared-sibling path.
    #[tokio::test]
    async fn dispatch_isolates_into_a_fresh_clone_when_a_live_sibling_shares_the_checkout() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        // The already-live sibling: a distinct `git worktree add` off `repo`,
        // exactly the shape an earlier orchestration's own dispatch/SpawnPane
        // would have produced.
        let live_sibling_dir = tmp.path().join("repo-existing-live-orchestration");
        create_worktree(
            &repo,
            &live_sibling_dir,
            "agent/existing-live-orchestration",
            false,
            Creator::dispatch("existing-live-orchestration"),
        )
        .await
        .expect("provision the pre-existing live sibling worktree");

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![live_orchestration_record(
                &live_sibling_dir,
            )]),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "gate-live-unit", "task", None).await;

        assert!(
            result.success,
            "`default_command` is a real binary (`cat`), so this dispatch must actually \
             succeed -- without this, the test also passes in the scenario where the \
             clone is provisioned, spawn then fails, and a broken rollback leaves the \
             clone directory behind uncleaned, which is exactly the bug class this PR's \
             own review found elsewhere: {}",
            result.message
        );

        let repo_common = crate::issue_dispatch_run::git_common_dir(&repo)
            .expect("resolve the shared checkout's own common dir");
        let worktree_common = crate::issue_dispatch_run::git_common_dir(&result.worktree_dir)
            .expect(
                "resolve the new dispatch's own common dir -- it must exist as SOME kind of \
                 git repository regardless of which provisioning mechanism ran",
            );
        assert_ne!(
            worktree_common,
            repo_common,
            "a live sibling already sharing the target root checkout must make this \
             dispatch isolate into its OWN fresh clone -- a distinct git common dir/object \
             store -- instead of sharing {}'s via a `git worktree add` sibling, matching \
             Model A's Ok(true) branch: {}",
            repo.display(),
            result.message
        );

        // Fix round 2 (reviewer/auditor, tester round): `record_worktree`'s
        // call site branches the registered policy on `has_live_sibling` --
        // this must land as `RemovalPolicy::IsolatedClone`, never the
        // shared-checkout arm's `KeepIfDirty`, or tab close would run `git
        // worktree remove` against a directory that isn't a linked worktree
        // of anything.
        let registered_policy = ctx
            .worktrees
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&result.worktree_dir)
            .map(|entry| entry.policy);
        assert_eq!(
            registered_policy,
            Some(RemovalPolicy::IsolatedClone),
            "an isolated-clone dispatch must be registered under RemovalPolicy::IsolatedClone"
        );
    }

    /// Scenario: issue #490, case 3a -- fail closed on a well-formed daemon
    /// ERROR response (`ok: false`). Mirrors
    /// `root_checkout_has_live_sibling_fails_closed_on_daemon_error_response`
    /// in `src/ui.rs`: this is the shape `serve_attach` emits for a malformed
    /// request today, and a wedged/half-upgraded daemon that still accepts
    /// connections could emit for any other reason. `handle_dispatch` must
    /// refuse to provision at all -- no worktree, no branch -- matching Model
    /// A's `Err(reason)` branch, rather than falling back to the ordinary
    /// shared-sibling path, which this PR implements.
    #[tokio::test]
    async fn dispatch_fails_closed_on_daemon_error_response() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::err("simulated daemon-side failure"),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };
        let paths = derive_dispatch_paths(&repo, "gate-err-unit");

        let result = handle_dispatch(&ctx, "gate-err-unit", "task", None).await;

        assert!(
            !result.success,
            "an `ok: false` daemon response must fail dispatch CLOSED, not silently fall \
             back to the ordinary shared-sibling path: {}",
            result.message
        );
        assert!(
            !paths.worktree_dir.exists(),
            "a fail-closed refusal must not leave a worktree behind: {}",
            result.message
        );
        assert!(
            !branch_exists(&repo, &paths.branch),
            "a fail-closed refusal must not leave a branch behind either: {}",
            result.message
        );
    }

    /// Scenario: issue #490, case 3b -- fail closed on the OTHER untrustworthy
    /// shape: `ok: true` but `agent_records: None`, the documented OLDER-daemon
    /// shape (`agent_records`'s own doc comment: "Older daemons omit this
    /// field"). Mirrors
    /// `root_checkout_has_live_sibling_fails_closed_on_legacy_agents_only_response`
    /// in `src/ui.rs`: a legacy `agents` list carries only ids, no
    /// `tab_membership`, so it cannot answer whether a live sibling shares
    /// this root checkout -- `handle_dispatch` must refuse rather than assume
    /// "no live sibling", the same fail-closed shape the sibling
    /// `_daemon_error_` test above pins.
    #[tokio::test]
    async fn dispatch_fails_closed_on_legacy_agents_only_response() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agents(vec!["agent-1".to_string()]),
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };
        let paths = derive_dispatch_paths(&repo, "gate-legacy-unit");

        let result = handle_dispatch(&ctx, "gate-legacy-unit", "task", None).await;

        assert!(
            !result.success,
            "an older-daemon-shaped response (agent_records: None) must fail dispatch \
             CLOSED, not silently fall back to the ordinary shared-sibling path: {}",
            result.message
        );
        assert!(
            !paths.worktree_dir.exists(),
            "a fail-closed refusal must not leave a worktree behind: {}",
            result.message
        );
        assert!(
            !branch_exists(&repo, &paths.branch),
            "a fail-closed refusal must not leave a branch behind either: {}",
            result.message
        );
    }

    // --- issue #490 fix round 2 (tester round): coverage for the isolated-
    // clone rollback/cleanup logic reviewer+auditor found undertested across
    // two rounds of fixes (P1-4: the changelog's `No-Test:` claim was false
    // for all of it). Each test below is the isolated-clone-arm twin of an
    // already-pinned shared-checkout-arm test, forcing the isolated branch
    // via the same `with_crafted_attach_daemon` live-sibling gate the
    // `dispatch_isolates_into_a_fresh_clone_when_a_live_sibling_shares_the_checkout`
    // test above already uses.

    /// Scenario: issue #490 fix round 2, item 1. Same forced-spawn-failure
    /// setup as `spawn_rollback_force_removes_when_nothing_else_roots_the_worktree`
    /// (#469), but with the live-sibling GATE also tripped, so
    /// `handle_dispatch` provisions an ISOLATED CLONE rather than an
    /// ordinary shared-checkout worktree before spawn fails. With nothing
    /// rooted in the freshly-cloned directory, the rollback's isolated-clone
    /// branch (`attempt_isolated_clone_cleanup` via `spawn_blocking`) must
    /// actually remove it, not just report success without cleaning up. Also
    /// (auditor F2, PRD fork#325 M3 final round -- "the two things this
    /// three-round review was actually about") pre-creates a branch of the
    /// SAME NAME as the dispatch's own `paths.branch` directly in the ROOT
    /// checkout (`repo`), representing unrelated committed work that
    /// happened to reuse the name, and asserts it still exists after the
    /// rollback: the original round-1 BLOCKER (auditor A1) was exactly this
    /// isolated-clone rollback path running `git -C <root checkout> branch
    /// -D <branch>` and force-deleting it. Proving the clone itself is
    /// cleaned up correctly (the assertions below already did) is not the
    /// same claim as proving the root checkout's branches were never
    /// touched -- a partial regression that reintroduced the stray `branch
    /// -D` alongside otherwise-correct clone cleanup would pass every
    /// assertion this test previously had.
    #[tokio::test]
    async fn spawn_rollback_force_removes_the_isolated_clone_when_nothing_else_roots_it() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        // Trips the has_live_sibling GATE: an already-live orchestration
        // sharing `repo`'s git-common-dir via its own sibling worktree.
        let live_sibling_dir = tmp.path().join("repo-existing-live-orchestration");
        create_worktree(
            &repo,
            &live_sibling_dir,
            "agent/existing-live-orchestration",
            false,
            Creator::dispatch("existing-live-orchestration"),
        )
        .await
        .expect("provision the pre-existing live sibling worktree");

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![live_orchestration_record(
                &live_sibling_dir,
            )]),
        );

        // Auditor F2: unrelated pre-existing committed work in the ROOT
        // checkout, under the exact branch name this dispatch will pick.
        let paths = derive_dispatch_paths(&repo, "isolated-notrip-unit");
        std::process::Command::new("git")
            .args(["branch", &paths.branch])
            .current_dir(&repo)
            .output()
            .expect("git available");
        assert!(
            branch_exists(&repo, &paths.branch),
            "sanity: the unrelated root-checkout branch must actually have been created"
        );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("/definitely-not-a-real-binary-xyz-490".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "isolated-notrip-unit", "task", None).await;

        assert!(!result.success);
        assert!(
            result.message.contains("spawn failed"),
            "the rollback arm must actually have been reached: {}",
            result.message
        );
        assert!(
            !result.message.contains("cleanup skipped"),
            "nothing is rooted in the fresh clone, so cleanup must not be skipped: {}",
            result.message
        );
        assert!(
            !result.worktree_dir.exists(),
            "with nothing else rooted there, the isolated clone must actually be removed \
             via attempt_isolated_clone_cleanup, exactly as force-removal already works for \
             the shared-checkout arm: {}",
            result.message
        );
        assert!(
            !ctx.worktrees
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&result.worktree_dir),
            "the registry entry must still be dropped"
        );
        assert!(
            branch_exists(&repo, &paths.branch),
            "auditor F2 (round-1 BLOCKER A1): the isolated-clone rollback path must NEVER run \
             `branch -D` against the root checkout -- this branch is unrelated pre-existing \
             work that merely shares a name with the dispatch, and must survive untouched: {}",
            result.message
        );
    }

    /// Scenario: issue #490 fix round 2, item 2. Same as
    /// `spawn_rollback_force_removes_the_isolated_clone_when_nothing_else_roots_it`,
    /// but with a live sibling role ALSO registered as rooted INSIDE the
    /// about-to-be-cleaned-up clone -- the #469 multi-role liveness guard
    /// (`worktree_still_in_use`) -- which must protect the isolated clone
    /// exactly as it already protects an ordinary shared-checkout worktree.
    /// The sibling's real PTY is spawned into `tmp.path()` (which already
    /// exists) rather than the clone target itself: unlike the
    /// shared-checkout arm's `git worktree add` (which tolerates a
    /// pre-existing EMPTY directory), `provision_isolated_clone_sync` reports
    /// `AlreadyClaimed` for ANY pre-existing path, so pre-creating the clone
    /// target the way the #469 test pre-creates its worktree dir would
    /// short-circuit the whole scenario before spawn is ever reached. Only
    /// `tab_membership.orchestration_cwd` -- the field `worktree_still_in_use`
    /// actually reads (see `worktree_of_record`) -- needs to name the
    /// not-yet-created clone target. Also (auditor F1, PRD fork#325 M3 final
    /// round) gives `repo` an `origin` and installs
    /// `with_git_remote_set_url_failing`, the same stub
    /// `dispatch_surfaces_the_origin_warning_in_the_success_message` uses, so
    /// the `clone_origin_warning` fold into THIS arm's "cleanup skipped"
    /// message (round 2, reviewer P1-2 / auditor R3) is actually exercised
    /// end to end rather than merely reachable in principle -- without this,
    /// a regression dropping that fold again would still pass every
    /// assertion this test previously had.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_rollback_skips_cleanup_when_a_live_sibling_still_roots_the_isolated_clone() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        // Gives the source an `origin` so `provision_isolated_clone_sync`
        // takes the "point at source's own origin" branch rather than the
        // "no origin" removal branch -- see
        // `dispatch_surfaces_the_origin_warning_in_the_success_message`.
        let out = std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://example.invalid/repo.git",
            ])
            .current_dir(&repo)
            .output()
            .expect("git available");
        assert!(out.status.success(), "git remote add failed: {out:?}");
        let _git_stub = with_git_remote_set_url_failing(tmp.path());

        let live_sibling_dir = tmp.path().join("repo-existing-live-orchestration");
        create_worktree(
            &repo,
            &live_sibling_dir,
            "agent/existing-live-orchestration",
            false,
            Creator::dispatch("existing-live-orchestration"),
        )
        .await
        .expect("provision the pre-existing live sibling worktree");

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![live_orchestration_record(
                &live_sibling_dir,
            )]),
        );

        let paths = derive_dispatch_paths(&repo, "isolated-trip-unit");
        let registry = Arc::new(AgentPtyRegistry::new());
        let sibling_id = registry
            .spawn_agent(crate::agent_pty::SpawnOptions {
                cwd: Some(&tmp.path().to_string_lossy()),
                tab_membership: Some(crate::agent_pty::TabMembership::Orchestration {
                    name: "isolated-trip-unit".to_string(),
                    role_index: 0,
                    role_name: "coder".to_string(),
                    is_start_role: false,
                    orchestration_cwd: Some(paths.worktree_dir.to_string_lossy().into_owned()),
                    display_title: None,
                    orchestration_id: None,
                }),
                ..crate::agent_pty::SpawnOptions::default()
            })
            .expect(
                "spawn a live sibling agent claiming to be rooted in the about-to-be-created \
                 isolated clone",
            );

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: registry.clone(),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("/definitely-not-a-real-binary-xyz-490".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "isolated-trip-unit", "task", None).await;

        // Close the sibling shell before any assertion can panic (mirroring
        // the #469 test's own F5a precaution).
        let _ = registry.close_agent(&sibling_id);

        assert!(
            !result.success,
            "the spawn itself must still report failure"
        );
        assert_eq!(
            result.worktree_dir, paths.worktree_dir,
            "sanity: the dispatch must have picked the same path this test pre-derived"
        );
        assert!(
            paths.worktree_dir.exists(),
            "a live sibling still rooted in the isolated clone must keep it from being \
             force-removed (issue #469, isolated-clone arm): {}",
            result.message
        );
        assert!(
            branch_exists(&repo, &paths.branch)
                || crate::issue_dispatch_run::git_common_dir(&paths.worktree_dir).is_ok(),
            "the clone itself (which carries its own local branch) must survive too"
        );
        assert!(
            ctx.worktrees
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&paths.worktree_dir),
            "the registry entry must survive so a later close can still find and clean it up"
        );
        assert!(
            result.message.contains("cleanup skipped"),
            "the skip message must name the cause so an operator doesn't reach for a manual \
             force-removal: {}",
            result.message
        );
        assert!(
            result
                .message
                .contains("could not be pointed at the real remote"),
            "auditor F1: the clone_origin_warning fold into the skip-cleanup arm (round 2, \
             reviewer P1-2 / auditor R3) must actually surface here -- live sibling roles \
             remaining inside the clone are exactly the ones CLAUDE.md rule 1 tells to `git \
             push origin` from inside it: {}",
            result.message
        );
        assert!(
            result
                .message
                .contains("stub: simulated git remote set-url failure"),
            "the underlying git error text must flow through on this arm too, not just a \
             generic notice: {}",
            result.message
        );

        let _ = std::fs::remove_dir_all(&paths.worktree_dir);
    }

    /// Scenario: issue #563. The isolated-clone twin of
    /// `spawn_rollback_retains_registry_entry_when_force_removal_fails`
    /// (#473): trips the `has_live_sibling` gate exactly as
    /// `spawn_rollback_force_removes_the_isolated_clone_when_nothing_else_roots_it`
    /// does, so `handle_dispatch` provisions a fresh isolated clone before
    /// spawn fails, then forces the rollback's `attempt_isolated_clone_cleanup`
    /// to genuinely fail via `with_git_clone_leaving_an_unremovable_entry` --
    /// a chmod-500 subdirectory planted inside the freshly cloned `.git/`
    /// right after the real clone succeeds, never touched by any later
    /// provisioning step, so only the LATER cleanup attempt (not
    /// provisioning itself) fails.
    ///
    /// Asserts the CORRECT post-rollback behavior, mirroring #473's
    /// shared-checkout assertion: the registry entry must still be present,
    /// since the isolated clone directory is genuinely still on disk. The
    /// isolated-clone arm's `cleanup_failed = cleaned_up_by.is_none()`
    /// computation never updates `should_drop_registry` (unlike the
    /// shared-checkout `else` arm, which sets `should_drop_registry =
    /// !remove_failed`) -- it stays at its initial `true` regardless of
    /// whether cleanup actually succeeded, so today the registry entry is
    /// dropped anyway even though the directory is still there.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_rollback_retains_registry_entry_when_isolated_clone_cleanup_fails() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        // Trips the has_live_sibling GATE, same as
        // `spawn_rollback_force_removes_the_isolated_clone_when_nothing_else_roots_it`.
        let live_sibling_dir = tmp.path().join("repo-existing-live-orchestration");
        create_worktree(
            &repo,
            &live_sibling_dir,
            "agent/existing-live-orchestration",
            false,
            Creator::dispatch("existing-live-orchestration"),
        )
        .await
        .expect("provision the pre-existing live sibling worktree");

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![live_orchestration_record(
                &live_sibling_dir,
            )]),
        );

        let paths = derive_dispatch_paths(&repo, "isolated-cleanupfail-unit");
        let _git_stub = with_git_clone_leaving_an_unremovable_entry(tmp.path());

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("/definitely-not-a-real-binary-xyz-563".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "isolated-cleanupfail-unit", "task", None).await;

        // Restore the blocker directory's permissions before any assertion
        // can panic, so this test's own tempdir can still be cleaned up on
        // drop regardless of outcome.
        let _ = std::fs::set_permissions(
            paths.worktree_dir.join(".git").join("dad-563-unremovable"),
            std::fs::Permissions::from_mode(0o755),
        );

        assert!(!result.success);
        assert!(
            result.message.contains("spawn failed"),
            "the rollback arm must actually have been reached: {}",
            result.message
        );
        assert!(
            paths.worktree_dir.exists(),
            "cleanup was forced to fail (an unremovable entry planted inside `.git`) -- the \
             isolated clone directory must still be on disk: {}",
            result.message
        );
        assert!(
            ctx.worktrees
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&paths.worktree_dir),
            "issue #563: the registry entry for {} must still be present -- \
             attempt_isolated_clone_cleanup genuinely failed (the directory is still on \
             disk), so dropping the entry anyway loses the only record that this isolated \
             clone is still there: {}",
            paths.worktree_dir.display(),
            result.message
        );
        assert!(
            result.message.contains("cleanup failed"),
            "the rollback must report cleanup as having failed: {}",
            result.message
        );
    }

    /// Scenario: issue #490 fix round 2, item 3. The isolated clone's
    /// registered `RemovalPolicy::IsolatedClone` must ALWAYS report `Kept`
    /// on tab close -- even when the clone's working tree is perfectly
    /// clean -- unlike `RemovalPolicy::KeepIfDirty`
    /// (`keep_if_dirty_preserves_a_worktree_with_uncommitted_work` above),
    /// which only protects a DIRTY tree. Deleting an isolated clone destroys
    /// its own `.git`, so a clean working tree does not prove it is safe to
    /// discard the way it does for a linked worktree sharing the root's
    /// object store.
    #[tokio::test]
    async fn isolated_clone_is_always_kept_on_tab_close_even_when_clean() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let clone_dir = tmp.path().join("isolated-clone");

        // A real, genuinely-clean `git clone` -- not provisioned via
        // `provision_isolated_clone_sync`, since `remove_worktree`'s policy
        // branch doesn't care how the clone came to exist, only what policy
        // it's called with.
        let out = std::process::Command::new("git")
            .args([
                "clone",
                "-q",
                "--",
                &repo.to_string_lossy(),
                &clone_dir.to_string_lossy(),
            ])
            .output()
            .expect("git available");
        assert!(out.status.success(), "git clone failed: {out:?}");

        let outcome = remove_worktree(
            &clone_dir,
            &repo,
            RemovalPolicy::IsolatedClone,
            "dispatch:test",
        )
        .await;

        assert_eq!(
            outcome,
            crate::issue_dispatch_run::RemoveOutcome::Kept(crate::event::KeptReason::IsolatedClone),
            "an isolated clone must always be reported Kept, regardless of dirtiness"
        );
        assert!(
            clone_dir.exists(),
            "an isolated clone must never be auto-removed on tab close, even when its \
             working tree is clean -- removing it would destroy its own .git"
        );
    }

    /// Scenario: issue #490 fix round 2, item 4 (reviewer B3 / auditor B1).
    /// Forces `provision_isolated_clone_sync` to actually return
    /// `Created { origin_warning: Some(_), .. }` by making `git remote
    /// set-url` fail underneath it (a `PATH`-shimmed `git`, so nothing here
    /// depends on filesystem-permission timing), then asserts
    /// `handle_dispatch`'s SUCCESS message actually surfaces that warning
    /// text -- the field CLAUDE.md rule 1 tells the dispatched agent to `git
    /// push origin` from inside, so silently dropping this warning would let
    /// a push land back in the user's own root checkout instead of the real
    /// remote.
    #[cfg(unix)]
    #[tokio::test]
    async fn dispatch_surfaces_the_origin_warning_in_the_success_message() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        // The origin fixup takes the "point at the source's own origin"
        // branch -- whose `origin_warning` is never overwritten again below
        // it in `provision_isolated_clone_sync` -- only when the SOURCE
        // itself has an origin configured. The "no origin" branch's later
        // `remove_isolated_clone_origin_default` call (unaffected by this
        // test's stub, which only fails `remote set-url`) would otherwise
        // silently overwrite the warning this test injects.
        let out = std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://example.invalid/repo.git",
            ])
            .current_dir(&repo)
            .output()
            .expect("git available");
        assert!(out.status.success(), "git remote add failed: {out:?}");

        let live_sibling_dir = tmp.path().join("repo-existing-live-orchestration");
        create_worktree(
            &repo,
            &live_sibling_dir,
            "agent/existing-live-orchestration",
            false,
            Creator::dispatch("existing-live-orchestration"),
        )
        .await
        .expect("provision the pre-existing live sibling worktree");

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![live_orchestration_record(
                &live_sibling_dir,
            )]),
        );
        let _git_stub = with_git_remote_set_url_failing(tmp.path());

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "origin-warn-unit", "task", None).await;

        assert!(
            result.success,
            "the origin fixup failing must not fail the whole dispatch: {}",
            result.message
        );
        assert!(
            result
                .message
                .contains("could not be pointed at the real remote"),
            "the success message must surface the origin_warning, not silently drop it: {}",
            result.message
        );
        assert!(
            result
                .message
                .contains("stub: simulated git remote set-url failure"),
            "the underlying git error text must actually flow through, not just a generic \
             notice: {}",
            result.message
        );
    }

    /// Scenario: issue #490 fix round 2, item 5 (R4/P2-8). Forces the
    /// isolated arm's `git clone` step itself to fail with stderr containing
    /// a raw terminal-hostile control byte (`ESC`), then asserts
    /// `handle_dispatch`'s resulting failure message never contains that raw
    /// byte -- proving the `IsolatedCloneOutcome::Failed { error, .. }` arm
    /// actually sanitizes git's own captured stderr before writing it into
    /// the caller's pane, not just the deck-controlled path/name
    /// interpolations sitting next to it in the same message. Also (reviewer
    /// P3-B, PRD fork#325 M3 final round) proves the negative assertion
    /// below isn't vacuous by first invoking the stub directly and
    /// confirming it genuinely emits the raw ESC byte on its own, matching
    /// `terminal_sanitize.rs`'s own precedent (`escapes_word_joiner_u2060`)
    /// of confirming the hostile input was actually present before trusting
    /// its absence downstream -- without this, the stub silently failing to
    /// emit ESC at all (e.g. a shell quoting change swallowing `\033`) would
    /// make the "no raw ESC" assertion below pass for the wrong reason.
    #[cfg(unix)]
    #[tokio::test]
    async fn dispatch_sanitizes_git_stderr_in_the_isolated_clone_failed_message() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);

        let live_sibling_dir = tmp.path().join("repo-existing-live-orchestration");
        create_worktree(
            &repo,
            &live_sibling_dir,
            "agent/existing-live-orchestration",
            false,
            Creator::dispatch("existing-live-orchestration"),
        )
        .await
        .expect("provision the pre-existing live sibling worktree");

        let _daemon = with_crafted_attach_daemon(
            tmp.path(),
            crate::daemon_protocol::AttachResponse::agent_records(vec![live_orchestration_record(
                &live_sibling_dir,
            )]),
        );
        let _git_stub = with_git_clone_failing_with_hostile_stderr(tmp.path());

        // Reviewer P3-B: call the now-stubbed `git clone` directly (a
        // throwaway probe destination, distinct from the isolated-clone
        // target `handle_dispatch` will pick below) and confirm ITS raw
        // stderr genuinely contains the ESC byte, before any sanitizer has
        // had a chance to touch it.
        let probe_dest = tmp.path().join("git-stub-probe-dest");
        let probe = std::process::Command::new("git")
            .args([
                "clone",
                "--",
                &repo.to_string_lossy(),
                &probe_dest.to_string_lossy(),
            ])
            .output()
            .expect("stubbed git available on PATH");
        let probe_stderr = String::from_utf8_lossy(&probe.stderr);
        assert!(
            probe_stderr.contains('\u{1b}'),
            "sanity: the stub itself must genuinely emit a raw ESC byte, or the \"no raw ESC\" \
             assertion below would be vacuous: {probe_stderr:?}"
        );
        let _ = std::fs::remove_dir_all(&probe_dest);

        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let ctx = DispatchContext {
            working_dir: repo.clone(),
            registry: Arc::new(AgentPtyRegistry::new()),
            event_tx,
            worktrees: new_worktree_registry(),
            default_command: Some("cat".to_string()),
            state: None,
        };

        let result = handle_dispatch(&ctx, "hostile-clone-fail-unit", "task", None).await;

        assert!(!result.success);
        assert!(
            result.message.contains("isolated clone failed"),
            "sanity: the isolated clone's own git-clone-failure arm must have been reached: {}",
            result.message
        );
        assert!(
            !result.message.contains('\u{1b}'),
            "a raw ESC byte from git's own stderr must never reach the caller's pane \
             unsanitized: {:?}",
            result.message
        );
        assert!(
            result.message.contains("simulated clone failure"),
            "the underlying git error text must still be readable, just escaped, not \
             discarded entirely: {}",
            result.message
        );
    }

    /// The wire choice maps onto the spawn override, and ABSENT stays absent —
    /// that is what preserves the pre-selector behaviour for an older CLI.
    #[test]
    fn wire_shape_maps_onto_the_spawn_override() {
        use crate::event::DispatchShape;
        assert_eq!(shape_override_of(None), None);
        assert_eq!(
            shape_override_of(Some(&DispatchShape::SingleAgent)),
            Some(SpawnShapeOverride::SingleAgent)
        );
        assert_eq!(
            shape_override_of(Some(&DispatchShape::Orchestration { name: None })),
            Some(SpawnShapeOverride::Orchestration(None))
        );
        assert_eq!(
            shape_override_of(Some(&DispatchShape::Orchestration {
                name: Some("review".into())
            })),
            Some(SpawnShapeOverride::Orchestration(Some("review".into())))
        );
    }

    /// The `shape` field is additive: a payload written by a CLI that predates it
    /// still deserializes, and lands as `None` (= config-derived), so an older
    /// client keeps working against a newer daemon.
    #[test]
    fn dispatch_signal_without_shape_still_deserializes_as_config_derived() {
        let legacy = r#"{"message_type":"dispatch","pane_id":"p1","name":"unit",
                         "task":"do it","timestamp":"2026-08-08T00:00:00Z"}"#;
        let msg: crate::event::DaemonMessage =
            serde_json::from_str(legacy).expect("a pre-selector dispatch payload must still parse");
        match msg {
            crate::event::DaemonMessage::Dispatch(sig) => {
                assert_eq!(sig.name, "unit");
                assert!(
                    sig.shape.is_none(),
                    "an omitted shape must mean config-derived, not a parse failure"
                );
            }
            other => panic!("expected a dispatch message, got {other:?}"),
        }
    }
}
