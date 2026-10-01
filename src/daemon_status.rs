//! `dot-agent-deck daemon status [--json]`.
//!
//! Read-only diagnostic snapshot of the daemon's managed agents. A pure CLI
//! consumer of the existing `AttachRequest::ListAgents`: no new attach request
//! variant, and therefore no `PROTOCOL_VERSION` bump — the command adds nothing
//! to the TUI↔daemon wire, so an older daemon serves a newer CLI's status query
//! unchanged. (Issue #459: that rationale used to be a citation of a design note
//! under `.dot-agent-deck/`, which `.gitignore` excludes, so nobody reading the
//! merged source could follow it. The reasoning is inlined here instead.) This
//! module only reshapes `ListAgents`' `Vec<AgentRecord>` into the CLI's own
//! documented fields — it touches no daemon-side locking, since the existing
//! `ListAgents` handler (`daemon_protocol.rs`) already bounds itself to a
//! short `AppState` read lock released before any I/O await.
//!
//! Deliberately excluded from both the human table and the JSON document:
//!
//! * `last_user_prompt` / `first_prompts`. A status query is a diagnostic, run
//!   by anyone who can reach the socket and routinely pasted into a bug report
//!   or a terminal someone else is watching; prompt text is the user's private
//!   content and has no diagnostic value that the status/tool columns do not
//!   already carry. Pinned by `daemon/status/004`. The same reasoning is why
//!   [`StatusTool`] carries the tool NAME without its `detail` (issue #455).
//! * `hook_session_id` / `last_activity`. Wanted, but neither field exists on
//!   `SessionSnapshot` today; adding them means updating every
//!   `SessionSnapshot` struct literal across the crate. Left as a deliberate
//!   follow-up rather than folded in here.

use std::time::Duration;

use serde::Serialize;

use crate::agent_pty::{AgentRecord, OutstandingDelegationEntry, TabMembership};
use crate::state::{
    ActiveTool, SessionStatus, observes_own_delegations, observing_orchestrator_panes,
};

/// Version of the `--json` document shape. Bump on a field removal or a
/// meaning change; additive fields don't need a bump — consumers should
/// tolerate unknown keys.
///
/// `2` (issue #455): `active_tool.detail` was REMOVED. Version 1 serialized the
/// internal [`ActiveTool`] whole, so the JSON document carried the tool's
/// `detail` — the first line of the command being run, the file path being
/// read/written, the search pattern, the subagent description (see
/// `crate::hook`) — while the human table had always printed the tool NAME
/// only. Both modes now honour the same privacy contract, so a consumer that
/// read `active_tool.detail` gets nothing rather than something subtly
/// different: a field removal, which this constant exists to announce.
pub const SCHEMA_VERSION: u32 = 2;

/// Deadline for the whole connect+request round trip against the attach
/// socket. This is one-shot, local Unix-socket IPC, so 3s comfortably covers a
/// live daemon under load without leaving a caller stuck waiting on a wedged
/// one.
///
/// Expiry ABANDONS the query — it must never retry in a loop, and must never
/// cause lazy daemon startup. Both halves are load-bearing for a diagnostic
/// (issue #459 — inlined here rather than left as a citation of a note under
/// the gitignored `.dot-agent-deck/`): the command's whole job is to report the
/// daemon's condition without perturbing it, so a retry loop against a wedged
/// daemon would add load to the exact failure being diagnosed, and spawning a
/// daemon to answer "is a daemon running?" would make the question
/// unanswerable — the honest answer for an unreachable daemon is the
/// `unavailable` failure exit, not a freshly-minted empty one.
pub const STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// The public projection of a live [`ActiveTool`]: the tool's NAME, and
/// nothing else.
///
/// Issue #455: this type exists so the privacy contract is enforced by the
/// type system rather than by remembering. `StatusAgent` used to carry
/// `Option<ActiveTool>` — the internal state type — so every field ever added
/// to `ActiveTool` rode into the `--json` document for free, which is exactly
/// how `detail` (a command line, a file path, a search pattern — see
/// `crate::hook`) leaked out of a command documented as never printing prompt
/// text. Adding a field to `ActiveTool` can no longer widen this document; it
/// takes a deliberate edit here, and that edit owes a [`SCHEMA_VERSION`] bump.
///
/// Kept as an object with a `name` key rather than flattened to a bare string
/// so the change is a pure field REMOVAL: a v1 consumer reading
/// `active_tool.name` still reads it under v2, and only `active_tool.detail`
/// disappears.
#[derive(Debug, Clone, Serialize)]
pub struct StatusTool {
    pub name: String,
}

/// One row of the status output — the CLI's own documented shape, not a raw
/// re-export of `AgentRecord`/`SessionSnapshot`.
#[derive(Debug, Clone, Serialize)]
pub struct StatusAgent {
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SessionStatus>,
    pub shell_synthetic_working: bool,
    /// Issue #714: whether a monitored external wait (`worker-agent-deck
    /// wait start`/`wait done`) is the reason this pane currently reads
    /// `Working` — either because the wait promoted it from idle, or
    /// because it is holding open a `Working` that an agent's own real
    /// completion would otherwise have reverted. Drives the `Observing`
    /// label on both the CLI and TUI surfaces (issue #784). Only ever `true`
    /// alongside `status: Working`.
    pub wait_observing: bool,
    /// Issue #803: whether this pane presents as `Observing` because it is
    /// `Idle` while a delegation IT ISSUED is still outstanding on a worker
    /// pane. `status` keeps reporting the real `Idle`. Never `true` for a
    /// pane that itself owes a `work-done` (that pane is a delegated
    /// worker), nor alongside any status but `Idle`. Always serialized;
    /// additive — see [`SCHEMA_VERSION`]'s doc.
    pub observing_delegations: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tool: Option<StatusTool>,
    /// Issue #586 M1/M2: PRD #126's idle-worker watch, if currently armed for
    /// this pane. Purely additive — see [`SCHEMA_VERSION`]'s doc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outstanding_delegation: Option<crate::agent_pty::WatchSnapshot>,
    /// Issue #586 M1/M2: PRD #249's delegate silent-worker watch, if
    /// currently armed for this pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_watch: Option<crate::agent_pty::WatchSnapshot>,
    /// Issue #586 M1/M2: issue #448's commission ledger entry for this pane,
    /// if any delegation is still unanswered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegation_commission: Option<crate::agent_pty::CommissionSnapshot>,
}

/// Top-level `--json` document: a [`SCHEMA_VERSION`] and the `agents` array,
/// each entry a [`StatusAgent`].
///
/// Issue #459 follow-through: this used to defer to "the design rationale",
/// which was a note under the gitignored `.dot-agent-deck/` — no reader of the
/// merged source could follow it. The shape is stated here instead, and what is
/// deliberately absent from it (`last_user_prompt` / `first_prompts`,
/// `hook_session_id` / `last_activity`) is stated in the module doc comment
/// above, with the reason for each.
#[derive(Debug, Clone, Serialize)]
pub struct StatusDocument {
    pub schema_version: u32,
    pub agents: Vec<StatusAgent>,
}

impl StatusDocument {
    pub fn new(agents: Vec<StatusAgent>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            agents,
        }
    }
}

/// `TabMembership` -> a short human role label. `None` (no membership, i.e.
/// a dashboard pane) stays `None` — the caller decides how to render that.
fn role_of(tab_membership: &Option<TabMembership>) -> Option<String> {
    match tab_membership {
        None => None,
        Some(TabMembership::Mode { name }) => Some(format!("mode:{name}")),
        Some(TabMembership::Orchestration {
            role_name,
            is_start_role,
            ..
        }) => {
            let role = if role_name.is_empty() {
                "role"
            } else {
                role_name.as_str()
            };
            if *is_start_role {
                Some(format!("{role} (orchestrator)"))
            } else {
                Some(role.to_string())
            }
        }
    }
}

/// Reduce the daemon's `ListAgents` reply to the CLI's own status shape.
/// Pure — no I/O — so it's unit-testable independent of a live daemon.
#[cfg(test)]
pub fn build_status_agents(records: Vec<AgentRecord>) -> Vec<StatusAgent> {
    build_status_agents_with_delegations(records, &[])
}

/// Reduce a reply to status rows using both the per-record delegations and issue
/// #817's daemon-wide delegation list from the same reply. A delegation onto a worker pane with no live agent has no
/// record to carry `outstanding_delegation`, so `delegations` supplies it;
/// entries duplicating a record's own join are harmless (same pairs). An
/// orchestrator pane absent from `records` simply gets no row to mark.
pub fn build_status_agents_with_delegations(
    records: Vec<AgentRecord>,
    delegations: &[OutstandingDelegationEntry],
) -> Vec<StatusAgent> {
    // Issue #803: derived from this one reply, so it can never outlive the
    // records it was read from.
    let observing_panes = observing_orchestrator_panes(
        records
            .iter()
            .filter_map(|record| {
                Some((
                    record.pane_id_env.as_deref()?,
                    record.outstanding_delegation.as_ref()?,
                ))
            })
            .chain(
                delegations
                    .iter()
                    .map(|d| (d.worker_pane_id.as_str(), &d.watch)),
            ),
    );
    records
        .into_iter()
        .map(|record| {
            let live = record.live;
            let observing_delegations = live.as_ref().is_some_and(|s| {
                observes_own_delegations(&s.status, record.pane_id_env.as_deref(), &observing_panes)
            });
            StatusAgent {
                agent_id: record.id,
                pane_id: record.pane_id_env,
                label: record.display_name,
                cwd: record.cwd,
                role: role_of(&record.tab_membership),
                status: live.as_ref().map(|s| s.status.clone()),
                shell_synthetic_working: live
                    .as_ref()
                    .map(|s| s.shell_synthetic_working)
                    .unwrap_or(false),
                wait_observing: live
                    .as_ref()
                    .map(|s| {
                        s.status == SessionStatus::Working
                            && (s.wait_synthetic_working || s.wait_deferred_revert)
                    })
                    .unwrap_or(false),
                observing_delegations,
                // Issue #455: project down to the NAME here, at the one place
                // that crosses from internal state into the CLI's document —
                // `detail` never leaves this function.
                active_tool: live
                    .and_then(|s| s.active_tool)
                    .map(|tool: ActiveTool| StatusTool { name: tool.name }),
                outstanding_delegation: record.outstanding_delegation,
                silence_watch: record.silence_watch,
                delegation_commission: record.delegation_commission,
            }
        })
        .collect()
}

const DASH: &str = "-";

fn cell(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or(DASH)
}

/// Render the concise, one-row-per-agent human table. Never includes prompt
/// text or scrollback (see the module docs for why) — only the diagnostic
/// fields on [`StatusAgent`] are candidates for a column here, and the TOOL
/// column deliberately shows the tool's name without its `detail`.
pub fn format_human(agents: &[StatusAgent]) -> String {
    if agents.is_empty() {
        return "no managed agents\n".to_string();
    }
    let mut out = String::new();
    out.push_str("PANE\tAGENT\tROLE\tSTATUS\tTOOL\tLABEL\tCWD\n");
    for a in agents {
        let status = a
            .status
            .as_ref()
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| DASH.to_string());
        // Issue #714 / #784: a `Working` currently held up by a monitored
        // external wait (`worker-agent-deck wait start`) rather than real
        // agent activity — either because the wait promoted the pane from
        // idle, or because it is holding open a `Working` an agent's own
        // completion would otherwise have reverted — renders as the bare
        // word `"Observing"` (issue #784 dropped the earlier `"Working
        // (observing)"` suffix composition). See `wait_observing`'s doc
        // comment above. This replaces the whole status word, so it must run
        // before the shell-busy marker below, which attaches to whatever
        // word is current.
        //
        // Issue #803: an `Idle` orchestrator with a delegation it issued
        // still outstanding (`observing_delegations`) reads the same bare
        // word. Each flag is gated on the one status it qualifies, so every
        // other status keeps its own word whatever the flags say.
        let wait_observing = a.wait_observing && a.status == Some(SessionStatus::Working);
        let observing_delegations =
            a.observing_delegations && a.status == Some(SessionStatus::Idle);
        let status = if wait_observing || observing_delegations {
            "Observing".to_string()
        } else {
            status
        };
        // Fork issue #21 provenance marker: flag a `Working` that this
        // mechanism currently holds shell responsible for keeping up.
        // Originally that meant "synthesized from shell activity rather
        // than agent-emitted" — PRD #499 round 6 widened it: the marker can
        // now also be set by a hand-off (`MonitoredWaitDone`'s Direction A)
        // onto a `Working` a real agent event emitted, once a monitored
        // wait's revert becomes shell's obligation to pay. See
        // `shell_synthetic_working`'s doc comment in `src/state.rs`. Runs
        // after the observing check above so the marker attaches directly to
        // whichever word is current (`"Working*"` or `"Observing*"`).
        let status = if a.shell_synthetic_working {
            format!("{status}*")
        } else {
            status
        };
        let tool = a
            .active_tool
            .as_ref()
            .map(|t| t.name.as_str())
            .unwrap_or(DASH);
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            cell(&a.pane_id),
            a.agent_id,
            cell(&a.role),
            status,
            tool,
            cell(&a.label),
            cell(&a.cwd),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SessionSnapshot;

    fn record(id: &str, pane: &str, live: Option<SessionSnapshot>) -> AgentRecord {
        AgentRecord {
            id: id.to_string(),
            pane_id_env: Some(pane.to_string()),
            display_name: None,
            cwd: Some("/tmp/x".to_string()),
            tab_membership: None,
            agent_type: None,
            rows: 0,
            cols: 0,
            live,
            spawned_at_ms: None,
            daemon_boot_id: None,
            registration_generation: None,
            cli_name: None,
            crashed: None,
            outstanding_delegation: None,
            silence_watch: None,
            delegation_commission: None,
        }
    }

    fn snapshot(status: SessionStatus) -> SessionSnapshot {
        SessionSnapshot {
            status,
            agent_type: None,
            active_tool: None,
            tool_count: 0,
            first_prompts: Vec::new(),
            last_user_prompt: None,
            live_target: None,
            last_activity_ms: None,
            shell_synthetic_working: false,
            monitored_wait_active: false,
            wait_synthetic_working: false,
            shell_descendant_busy: false,
            wait_deferred_revert: false,
            model: None,
            agent_report_activity_seen: false,
        }
    }

    /// Scenario: build status rows from a driven agent (has a live
    /// `Thinking` snapshot) and an untouched control agent (no live
    /// snapshot). Confirm the rendered human table names both pane ids and
    /// that stripping each row's own pane id still leaves them different —
    /// this is the pure-function core of `daemon/status/001`.
    #[test]
    fn build_status_agents_distinguishes_driven_from_control() {
        let records = vec![
            record("agent-1", "driven", Some(snapshot(SessionStatus::Thinking))),
            record("agent-2", "control", None),
        ];
        let agents = build_status_agents(records);
        let table = format_human(&agents);
        assert!(table.contains("driven"));
        assert!(table.contains("control"));

        let driven_line = table.lines().find(|l| l.contains("driven")).unwrap();
        let control_line = table.lines().find(|l| l.contains("control")).unwrap();
        let driven_norm = driven_line
            .replace("driven", "<pane>")
            .replace("agent-1", "<agent>");
        let control_norm = control_line
            .replace("control", "<pane>")
            .replace("agent-2", "<agent>");
        assert_ne!(driven_norm, control_norm);
    }

    /// Issue #586 M1/M2: `build_status_agents` must carry the three new
    /// delegation-watch fields straight through from `AgentRecord` onto
    /// `StatusAgent` when the registry has them populated, and leave them
    /// absent (not `null`) on the wire when it doesn't.
    #[test]
    fn build_status_agents_carries_delegation_watch_fields() {
        let armed = crate::agent_pty::AgentRecord {
            outstanding_delegation: Some(crate::agent_pty::WatchSnapshot {
                armed_secs_ago: 12,
                orchestrator_pane_id: "orch".to_string(),
            }),
            silence_watch: Some(crate::agent_pty::WatchSnapshot {
                armed_secs_ago: 3,
                orchestrator_pane_id: "orch".to_string(),
            }),
            delegation_commission: Some(crate::agent_pty::CommissionSnapshot {
                outstanding: 2,
                oldest_armed_secs_ago: 45,
                orchestrator_pane_id: "orch".to_string(),
            }),
            ..record("agent-1", "armed", None)
        };
        let unarmed = record("agent-2", "unarmed", None);

        let agents = build_status_agents(vec![armed, unarmed]);
        let armed_status = agents
            .iter()
            .find(|a| a.pane_id.as_deref() == Some("armed"))
            .unwrap();
        assert_eq!(
            armed_status
                .outstanding_delegation
                .as_ref()
                .unwrap()
                .armed_secs_ago,
            12
        );
        assert_eq!(
            armed_status.silence_watch.as_ref().unwrap().armed_secs_ago,
            3
        );
        assert_eq!(
            armed_status
                .delegation_commission
                .as_ref()
                .unwrap()
                .outstanding,
            2
        );

        let unarmed_status = agents
            .iter()
            .find(|a| a.pane_id.as_deref() == Some("unarmed"))
            .unwrap();
        assert!(unarmed_status.outstanding_delegation.is_none());
        assert!(unarmed_status.silence_watch.is_none());
        assert!(unarmed_status.delegation_commission.is_none());

        let json = serde_json::to_string(&StatusDocument::new(agents)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let unarmed_json = v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["pane_id"] == "unarmed")
            .unwrap();
        for key in [
            "outstanding_delegation",
            "silence_watch",
            "delegation_commission",
        ] {
            assert!(
                unarmed_json.as_object().unwrap().get(key).is_none(),
                "{key} must be absent, not null, when the watch isn't armed"
            );
        }
    }

    /// Scenario: a status row built from a live snapshot carrying a seeded
    /// `last_user_prompt` must never leak that prompt text into either the
    /// human table or the JSON document — the pure-function core of
    /// `daemon/status/004`.
    #[test]
    fn format_human_and_json_never_include_prompt_text() {
        let mut snap = snapshot(SessionStatus::Working);
        snap.last_user_prompt = Some("SENTINEL-PROMPT-TEXT".to_string());
        snap.first_prompts = vec!["SENTINEL-PROMPT-TEXT".to_string()];
        let agents = build_status_agents(vec![record("agent-1", "leak", Some(snap))]);

        let table = format_human(&agents);
        assert!(!table.contains("SENTINEL-PROMPT-TEXT"));

        let json = serde_json::to_string(&StatusDocument::new(agents)).unwrap();
        assert!(!json.contains("SENTINEL-PROMPT-TEXT"));
    }

    /// Scenario: a status row built from a live snapshot whose `active_tool`
    /// carries a `detail` must publish the tool's NAME and drop the detail —
    /// in the human table (which always did) and in the JSON document (which
    /// serialized `ActiveTool` whole until issue #455). The pure-function core
    /// of `daemon/status/004`'s `--json` half.
    #[test]
    fn active_tool_publishes_the_name_and_never_the_detail() {
        let mut snap = snapshot(SessionStatus::Working);
        snap.active_tool = Some(ActiveTool {
            name: "Read".to_string(),
            detail: Some("SENTINEL-TOOL-DETAIL".to_string()),
        });
        let agents = build_status_agents(vec![record("agent-1", "tool-pane", Some(snap))]);

        let table = format_human(&agents);
        assert!(
            table.contains("Read"),
            "table must name the tool: {table:?}"
        );
        assert!(!table.contains("SENTINEL-TOOL-DETAIL"));

        let json = serde_json::to_string(&StatusDocument::new(agents)).unwrap();
        assert!(
            !json.contains("SENTINEL-TOOL-DETAIL"),
            "json leaked: {json}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let tool = &parsed["agents"][0]["active_tool"];
        assert_eq!(tool["name"], "Read");
        assert!(
            tool.get("detail").is_none(),
            "`active_tool.detail` must be gone from the document, not merely \
             empty; got {tool:?}"
        );
    }

    #[test]
    fn json_document_carries_schema_version_and_pane_id() {
        let agents = build_status_agents(vec![record("agent-1", "json-pane", None)]);
        let json = serde_json::to_string(&StatusDocument::new(agents)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Version 2 (issue #455): `active_tool.detail` was removed from the
        // document, and `SCHEMA_VERSION`'s contract is to announce exactly
        // that.
        assert_eq!(parsed["schema_version"], 2);
        assert!(json.contains("json-pane"));
    }

    /// Scenario: issue #714 — a `Working` row held up purely by a monitored
    /// wait (`wait_synthetic_working`) must render distinctly from both a
    /// plain `Working` row and a shell-synthetic one, and the two markers
    /// must compose when both apply: the bare `"Observing"` word replaces
    /// `"Working"` (issue #784), then shell's `*` marker attaches directly
    /// to it. Also pins the PRD's headline flow — `wait_deferred_revert`
    /// alone (the H1 fix's OR-broadening, `build_status_agents`'s
    /// `wait_synthetic_working || wait_deferred_revert`) must trigger the
    /// same word — and the negative case: either flag set on a row whose
    /// status is NOT `Working` must never show it (the `status == Working`
    /// gate added alongside the fix).
    #[test]
    fn format_human_marks_wait_synthetic_working_as_observing() {
        let mut wait_only = snapshot(SessionStatus::Working);
        wait_only.shell_synthetic_working = false;
        wait_only.wait_synthetic_working = true;
        let agents = build_status_agents(vec![record("agent-1", "wait-pane", Some(wait_only))]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("wait-pane"))
            .unwrap_or_else(|| panic!("no row for wait-pane in {table:?}"));
        assert!(
            line.contains("Observing") && !line.contains("Working (observing)"),
            "a wait-only synthetic Working row must render the bare \"Observing\" word; \
             got {line:?}"
        );

        let mut shell_only = snapshot(SessionStatus::Working);
        shell_only.shell_synthetic_working = true;
        shell_only.wait_synthetic_working = false;
        let agents = build_status_agents(vec![record("agent-2", "shell-pane", Some(shell_only))]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("shell-pane"))
            .unwrap_or_else(|| panic!("no row for shell-pane in {table:?}"));
        assert!(
            line.contains("Working*") && !line.contains("Observing"),
            "a shell-only synthetic Working row must keep rendering exactly \"Working*\", \
             unchanged (non-regression); got {line:?}"
        );

        let mut both = snapshot(SessionStatus::Working);
        both.shell_synthetic_working = true;
        both.wait_synthetic_working = true;
        let agents = build_status_agents(vec![record("agent-3", "both-pane", Some(both))]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("both-pane"))
            .unwrap_or_else(|| panic!("no row for both-pane in {table:?}"));
        assert!(
            line.contains("Observing*") && !line.contains("Working* (observing)"),
            "when both markers apply they must compose as \"Observing*\" — the shell-busy \
             marker attached directly to the new status word; got {line:?}"
        );

        // H1 fix: `wait_deferred_revert` alone (no `wait_synthetic_working`)
        // must trigger the marker too — this exercises `build_status_agents`'s
        // OR-broadening, which the cases above never reach.
        let mut deferred_only = snapshot(SessionStatus::Working);
        deferred_only.wait_synthetic_working = false;
        deferred_only.wait_deferred_revert = true;
        let agents = build_status_agents(vec![record(
            "agent-4",
            "deferred-pane",
            Some(deferred_only),
        )]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("deferred-pane"))
            .unwrap_or_else(|| panic!("no row for deferred-pane in {table:?}"));
        assert!(
            line.contains("Observing") && !line.contains("Working (observing)"),
            "a `wait_deferred_revert`-only row (no `wait_synthetic_working`) must still render \
             the bare \"Observing\" word — this is the H1 fix's OR-broadening; got {line:?}"
        );

        // Negative case: the flag set but the status is NOT `Working` must
        // never show the marker — the `status == Working` gate added
        // alongside the fix.
        let mut not_working = snapshot(SessionStatus::WaitingForInput);
        not_working.wait_synthetic_working = true;
        not_working.wait_deferred_revert = true;
        let agents =
            build_status_agents(vec![record("agent-5", "waiting-pane", Some(not_working))]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("waiting-pane"))
            .unwrap_or_else(|| panic!("no row for waiting-pane in {table:?}"));
        assert!(
            !line.contains("Observing") && line.contains("WaitingForInput"),
            "a non-Working row must never show \"Observing\" even with both wait flags set; \
             got {line:?}"
        );
        assert!(
            !agents[0].wait_observing,
            "the projected wire value must gate on `status == Working` too, not just the text marker"
        );
    }

    /// Scenario: issue #784. `format_human` drops the `"Working "` prefix for
    /// a wait-observing row and renders the bare word `"Observing"` instead
    /// of the earlier `"Working (observing)"` composition —
    /// `format_human_marks_wait_synthetic_working_as_observing` above now
    /// pins the same current text (updated alongside this fix); this is a
    /// separate, more focused pin sitting next to it.
    ///
    /// Composition question for the shell-busy `"*"` marker when BOTH
    /// `shell_synthetic_working` and the wait-observing gate apply: today's
    /// code (`format_human`) attaches `*` directly onto the status WORD first
    /// (`"Working"` -> `"Working*"`), then appends `" (observing)"` as a
    /// wholly separate suffix — so `*` has never been part of the
    /// `"(observing)"` composition, it is a distinct concern (shell holding
    /// responsibility for keeping `Working` alive) that happens to render
    /// adjacent to it. Since this fix replaces the whole status WORD
    /// (`"Working"` -> `"Observing"`) rather than appending a suffix to it,
    /// the marker's existing "attaches directly to the status word" placement
    /// is preserved by attaching it to the new word the same way: `"Observing*"`,
    /// not `"Observing (marker dropped)"` or `"Working*Observing"`. This
    /// keeps the two concerns visually composed exactly as before — a status
    /// word, optionally starred — with only the word itself changing.
    #[test]
    fn format_human_marks_wait_observing_as_bare_observing_word() {
        let mut wait_only = snapshot(SessionStatus::Working);
        wait_only.shell_synthetic_working = false;
        wait_only.wait_synthetic_working = true;
        let agents = build_status_agents(vec![record("agent-1", "wait-pane", Some(wait_only))]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("wait-pane"))
            .unwrap_or_else(|| panic!("no row for wait-pane in {table:?}"));
        assert!(
            line.contains("Observing"),
            "a wait-only synthetic Working row must render the bare \"Observing\" word; \
             got {line:?}"
        );
        assert!(
            !line.contains("Working (observing)"),
            "the old \"Working (observing)\" composition must be gone once issue #784 lands; \
             got {line:?}"
        );

        let mut deferred_only = snapshot(SessionStatus::Working);
        deferred_only.wait_synthetic_working = false;
        deferred_only.wait_deferred_revert = true;
        let agents = build_status_agents(vec![record(
            "agent-2",
            "deferred-pane",
            Some(deferred_only),
        )]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("deferred-pane"))
            .unwrap_or_else(|| panic!("no row for deferred-pane in {table:?}"));
        assert!(
            line.contains("Observing"),
            "a `wait_deferred_revert`-only row (no `wait_synthetic_working`) must also render \
             \"Observing\" — this is the H1 fix's OR-broadening; got {line:?}"
        );

        // Composition case: both the shell-busy marker and wait-observing
        // apply. See this test's own doc comment for the reasoning behind
        // pinning "Observing*" (marker preserved, attached to the new word)
        // rather than dropping the marker entirely.
        let mut both = snapshot(SessionStatus::Working);
        both.shell_synthetic_working = true;
        both.wait_synthetic_working = true;
        let agents = build_status_agents(vec![record("agent-3", "both-pane", Some(both))]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("both-pane"))
            .unwrap_or_else(|| panic!("no row for both-pane in {table:?}"));
        assert!(
            line.contains("Observing*"),
            "when both markers apply they must compose as \"Observing*\" — the shell-busy \
             marker preserved and attached directly to the new status word; got {line:?}"
        );
        assert!(
            !line.contains("Working* (observing)") && !line.contains("Working (observing)"),
            "the old composition must be fully gone once issue #784 lands; got {line:?}"
        );

        // Negative case: the flag set but the status is NOT `Working` must
        // never show the new "Observing" word (nor the old suffix).
        let mut not_working = snapshot(SessionStatus::WaitingForInput);
        not_working.wait_synthetic_working = true;
        not_working.wait_deferred_revert = true;
        let agents =
            build_status_agents(vec![record("agent-4", "waiting-pane", Some(not_working))]);
        let table = format_human(&agents);
        let line = table
            .lines()
            .find(|l| l.contains("waiting-pane"))
            .unwrap_or_else(|| panic!("no row for waiting-pane in {table:?}"));
        assert!(
            !line.contains("Observing") && !line.contains("(observing)"),
            "a non-Working row must never show \"Observing\" even with both wait flags set; \
             got {line:?}"
        );
        assert!(
            line.contains("WaitingForInput"),
            "a non-Working row must keep rendering its real status; got {line:?}"
        );
    }

    /// Issue #803 fixture: a worker row whose `delegate` was issued by
    /// `orchestrator_pane` and has not been answered by a `work-done` yet.
    fn worker_delegated_by(id: &str, pane: &str, orchestrator_pane: &str) -> AgentRecord {
        AgentRecord {
            outstanding_delegation: Some(crate::agent_pty::WatchSnapshot {
                armed_secs_ago: 90,
                orchestrator_pane_id: orchestrator_pane.to_string(),
            }),
            ..record(id, pane, Some(snapshot(SessionStatus::Idle)))
        }
    }

    /// The STATUS column of `pane`'s row in the human table (columns are
    /// tab-separated: `PANE AGENT ROLE STATUS TOOL LABEL CWD`).
    fn status_cell(table: &str, pane: &str) -> String {
        table
            .lines()
            .find(|l| l.split('\t').next() == Some(pane))
            .and_then(|l| l.split('\t').nth(3))
            .unwrap_or_else(|| panic!("no row for {pane} in {table:?}"))
            .to_string()
    }

    /// Scenario: issue #803. An orchestrator whose real status is `Idle` and
    /// which ISSUED a delegation that is still outstanding on a worker pane
    /// must read `Observing` in the human table, exactly the word a
    /// wait-held `Working` already uses (issue #784). A second orchestrator
    /// that issued nothing, and the delegated worker itself, keep their own
    /// status words.
    #[test]
    fn format_human_shows_observing_for_idle_orchestrator_with_outstanding_delegation() {
        let agents = build_status_agents(vec![
            record("agent-1", "orch-pane", Some(snapshot(SessionStatus::Idle))),
            worker_delegated_by("agent-2", "worker-pane", "orch-pane"),
            record(
                "agent-3",
                "bystander-pane",
                Some(snapshot(SessionStatus::Idle)),
            ),
        ]);
        let table = format_human(&agents);

        assert_eq!(
            status_cell(&table, "orch-pane"),
            "Observing",
            "an Idle orchestrator with a delegation it issued still outstanding must read \
             \"Observing\" in the `daemon status` table; table:\n{table}"
        );
        assert_eq!(
            status_cell(&table, "bystander-pane"),
            "Idle",
            "an orchestrator that issued no outstanding delegation must keep reading \"Idle\" — \
             another pane's delegations must never leak onto it; table:\n{table}"
        );
        assert_eq!(
            status_cell(&table, "worker-pane"),
            "Idle",
            "the delegated WORKER's own row is out of scope for issue #803 and must be \
             unchanged; table:\n{table}"
        );
    }

    /// The `--json` document's row for `pane`, read back the way a consumer
    /// reads it: as parsed JSON, keyed by field name.
    fn json_row(agents: Vec<StatusAgent>, pane: &str) -> serde_json::Value {
        let json = serde_json::to_string(&StatusDocument::new(agents)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["pane_id"] == pane)
            .unwrap_or_else(|| panic!("no json row for {pane} in {json}"))
            .clone()
    }

    /// Scenario: issue #803. The `--json` document for an Idle orchestrator
    /// with a delegation it issued still outstanding keeps reporting its REAL
    /// status (`Idle`), leaves `wait_observing` at its original meaning
    /// (`false`: no monitored wait is holding a `Working`), and flags the
    /// presentation through the separate `observing_delegations: true`. A
    /// pane that issued no outstanding delegation, and the delegated worker,
    /// report both flags `false`.
    #[test]
    fn json_keeps_idle_status_and_flags_observing_delegations_for_delegating_orchestrator() {
        let agents = || {
            build_status_agents(vec![
                record("agent-1", "orch-pane", Some(snapshot(SessionStatus::Idle))),
                worker_delegated_by("agent-2", "worker-pane", "orch-pane"),
                record(
                    "agent-3",
                    "bystander-pane",
                    Some(snapshot(SessionStatus::Idle)),
                ),
            ])
        };
        let idle = serde_json::to_value(SessionStatus::Idle).unwrap();

        let orchestrator = json_row(agents(), "orch-pane");
        assert_eq!(
            orchestrator["status"], idle,
            "the orchestrator's real status must stay Idle in the json document — issue #803 \
             is display-only; got {orchestrator}"
        );
        assert_eq!(
            orchestrator["observing_delegations"],
            serde_json::Value::Bool(true),
            "an Idle orchestrator with a delegation it issued still outstanding must report \
             `observing_delegations: true`; got {orchestrator}"
        );
        assert_eq!(
            orchestrator["wait_observing"],
            serde_json::Value::Bool(false),
            "`wait_observing` keeps its original meaning (a Working held by a monitored \
             wait) and must stay false for an Idle orchestrator that holds no wait; got \
             {orchestrator}"
        );

        for pane in ["bystander-pane", "worker-pane"] {
            let row = json_row(agents(), pane);
            assert_eq!(row["status"], idle);
            assert_eq!(
                row["observing_delegations"],
                serde_json::Value::Bool(false),
                "{pane} issued no outstanding delegation and must report \
                 `observing_delegations: false`; got {row}"
            );
            assert_eq!(
                row["wait_observing"],
                serde_json::Value::Bool(false),
                "{pane} holds no monitored wait and must report `wait_observing: false`; got \
                 {row}"
            );
        }
    }

    /// Scenario: issue #803. `observing_delegations` always serializes, like
    /// `wait_observing`: an ordinary row with no delegation anywhere, and a
    /// row with no live snapshot at all, both carry it as `false` rather than
    /// omitting the key.
    #[test]
    fn json_observing_delegations_is_present_and_false_on_an_ordinary_row() {
        let agents = || {
            build_status_agents(vec![
                record(
                    "agent-1",
                    "thinking-pane",
                    Some(snapshot(SessionStatus::Thinking)),
                ),
                record("agent-2", "idle-pane", Some(snapshot(SessionStatus::Idle))),
                record("agent-3", "silent-pane", None),
            ])
        };
        for pane in ["thinking-pane", "idle-pane", "silent-pane"] {
            let row = json_row(agents(), pane);
            assert_eq!(
                row.get("observing_delegations"),
                Some(&serde_json::Value::Bool(false)),
                "`observing_delegations` must be present and false on {pane}'s ordinary row; \
                 got {row}"
            );
            assert_eq!(
                row.get("wait_observing"),
                Some(&serde_json::Value::Bool(false)),
                "`wait_observing` must be present and false on {pane}'s ordinary row; got {row}"
            );
        }
    }

    /// Scenario: issue #803. A `Working` held by a monitored wait reports
    /// `wait_observing: true` and `observing_delegations: false`, and that
    /// holds even when the same pane issued a delegation that is still
    /// outstanding, because its status is not `Idle`. The human table reads
    /// `Observing` for it either way.
    #[test]
    fn json_wait_held_working_row_flags_wait_observing_and_not_observing_delegations() {
        let mut wait_held = snapshot(SessionStatus::Working);
        wait_held.wait_synthetic_working = true;

        let plain = build_status_agents(vec![record(
            "agent-1",
            "wait-pane",
            Some(wait_held.clone()),
        )]);
        let delegating = build_status_agents(vec![
            record("agent-1", "wait-pane", Some(wait_held)),
            worker_delegated_by("agent-2", "worker-pane", "wait-pane"),
        ]);

        for (agents, shape) in [
            (plain, "holds a monitored wait"),
            (
                delegating,
                "holds a monitored wait and issued an outstanding delegation",
            ),
        ] {
            assert_eq!(
                status_cell(&format_human(&agents), "wait-pane"),
                "Observing",
                "a wait-held Working row that {shape} reads \"Observing\" in the table"
            );
            let row = json_row(agents, "wait-pane");
            assert_eq!(
                row["wait_observing"],
                serde_json::Value::Bool(true),
                "a Working row that {shape} must report `wait_observing: true`; got {row}"
            );
            assert_eq!(
                row["observing_delegations"],
                serde_json::Value::Bool(false),
                "a Working row that {shape} must report `observing_delegations: false`, \
                 because its status is not Idle; got {row}"
            );
        }
    }

    /// Scenario: issue #803. An orchestrator whose status is `Unknown` (which
    /// may be a newer status this build could not decode) with a delegation
    /// it issued still outstanding keeps its own status word in the human
    /// table and reports both `observing_delegations` and `wait_observing` as
    /// `false`.
    #[test]
    fn unknown_orchestrator_with_outstanding_delegation_keeps_its_own_status() {
        let agents = || {
            build_status_agents(vec![
                record(
                    "agent-1",
                    "orch-pane",
                    Some(snapshot(SessionStatus::Unknown)),
                ),
                worker_delegated_by("agent-2", "worker-pane", "orch-pane"),
            ])
        };
        let table = format_human(&agents());
        assert_eq!(
            status_cell(&table, "orch-pane"),
            "Unknown",
            "an Unknown orchestrator must keep reading \"Unknown\" with a delegation \
             outstanding, never \"Observing\"; table:\n{table}"
        );

        let row = json_row(agents(), "orch-pane");
        assert_eq!(
            row["status"],
            serde_json::to_value(SessionStatus::Unknown).unwrap(),
            "the json document reports the real status; got {row}"
        );
        assert_eq!(
            row["observing_delegations"],
            serde_json::Value::Bool(false),
            "an Unknown orchestrator must report `observing_delegations: false`; got {row}"
        );
        assert_eq!(
            row["wait_observing"],
            serde_json::Value::Bool(false),
            "an Unknown orchestrator holds no monitored wait; got {row}"
        );
    }

    /// Scenario: issue #803. A pane that owes a `work-done` AND has itself
    /// issued a delegation that is still outstanding is reported as a
    /// delegated worker: its table row keeps the word a delegated worker's
    /// row has (`Idle`), never `Observing`, and `observing_delegations` is
    /// `false`. The orchestrator above it, which owes nothing, reads
    /// `Observing`.
    #[test]
    fn pane_that_owes_a_work_done_is_never_reported_as_observing() {
        let agents = || {
            build_status_agents(vec![
                record("agent-1", "orch-pane", Some(snapshot(SessionStatus::Idle))),
                worker_delegated_by("agent-2", "middle-pane", "orch-pane"),
                worker_delegated_by("agent-3", "leaf-pane", "middle-pane"),
            ])
        };
        let table = format_human(&agents());
        assert_eq!(
            status_cell(&table, "middle-pane"),
            "Idle",
            "a pane that owes a work-done reads as a delegated worker's row does, never \
             \"Observing\", even though it issued a delegation itself; table:\n{table}"
        );
        assert_eq!(
            status_cell(&table, "leaf-pane"),
            "Idle",
            "the delegated leaf worker's row is unchanged; table:\n{table}"
        );
        assert_eq!(
            status_cell(&table, "orch-pane"),
            "Observing",
            "the orchestrator owes nothing and issued an outstanding delegation; \
             table:\n{table}"
        );

        let middle = json_row(agents(), "middle-pane");
        assert_eq!(
            middle["observing_delegations"],
            serde_json::Value::Bool(false),
            "a pane that owes a work-done must report `observing_delegations: false`; got \
             {middle}"
        );
        assert_eq!(
            middle["wait_observing"],
            serde_json::Value::Bool(false),
            "a pane that owes a work-done holds no monitored wait; got {middle}"
        );
        let orchestrator = json_row(agents(), "orch-pane");
        assert_eq!(
            orchestrator["observing_delegations"],
            serde_json::Value::Bool(true),
            "the orchestrator above it must report `observing_delegations: true`; got \
             {orchestrator}"
        );
    }

    /// Scenario: issue #803. The override applies to `Idle` only:
    /// an orchestrator with a delegation outstanding whose real status is
    /// `WaitingForInput`, `Error`, `Thinking` or a genuine `Working` keeps
    /// its real status word and reports `wait_observing: false`. Once the
    /// worker's delegation is gone from the records the Idle orchestrator
    /// reads `Idle` again.
    #[test]
    fn delegating_orchestrator_keeps_real_status_when_not_idle() {
        for status in [
            SessionStatus::WaitingForInput,
            SessionStatus::Error,
            SessionStatus::Thinking,
            SessionStatus::Working,
        ] {
            let agents = build_status_agents(vec![
                record("agent-1", "orch-pane", Some(snapshot(status.clone()))),
                worker_delegated_by("agent-2", "worker-pane", "orch-pane"),
            ]);
            let table = format_human(&agents);
            assert_eq!(
                status_cell(&table, "orch-pane"),
                format!("{status:?}"),
                "a {status:?} orchestrator must show its real status even with a delegation \
                 outstanding; table:\n{table}"
            );
            let orchestrator = agents
                .iter()
                .find(|a| a.pane_id.as_deref() == Some("orch-pane"))
                .unwrap();
            assert!(
                !orchestrator.wait_observing,
                "a {status:?} orchestrator must not report `wait_observing` for a delegation"
            );
        }

        let agents = build_status_agents(vec![
            record("agent-1", "orch-pane", Some(snapshot(SessionStatus::Idle))),
            record(
                "agent-2",
                "worker-pane",
                Some(snapshot(SessionStatus::Idle)),
            ),
        ]);
        let table = format_human(&agents);
        assert_eq!(
            status_cell(&table, "orch-pane"),
            "Idle",
            "with no delegation outstanding the orchestrator reads plain Idle; table:\n{table}"
        );
        assert!(!agents[0].wait_observing);
    }

    /// Issue #817: a delegation from the daemon-wide list whose worker pane has
    /// no record still makes an `Idle` orchestrator read `Observing`; an
    /// orchestrator pane absent from the records is ignored, and a non-`Idle`
    /// orchestrator keeps its own status word.
    #[test]
    fn daemon_wide_delegation_without_worker_record_marks_idle_orchestrator() {
        let entry = |orch: &str| OutstandingDelegationEntry {
            worker_pane_id: "ghost-worker".into(),
            watch: crate::agent_pty::WatchSnapshot {
                armed_secs_ago: 1,
                orchestrator_pane_id: orch.into(),
            },
        };
        let agents = build_status_agents_with_delegations(
            vec![record(
                "a1",
                "orch-pane",
                Some(snapshot(SessionStatus::Idle)),
            )],
            &[entry("orch-pane")],
        );
        assert!(agents[0].observing_delegations);
        assert_eq!(
            status_cell(&format_human(&agents), "orch-pane"),
            "Observing"
        );

        let agents = build_status_agents_with_delegations(
            vec![record(
                "a1",
                "orch-pane",
                Some(snapshot(SessionStatus::Idle)),
            )],
            &[entry("missing-orch")],
        );
        assert!(!agents[0].observing_delegations);

        let agents = build_status_agents_with_delegations(
            vec![record(
                "a1",
                "orch-pane",
                Some(snapshot(SessionStatus::Working)),
            )],
            &[entry("orch-pane")],
        );
        assert_eq!(status_cell(&format_human(&agents), "orch-pane"), "Working");
    }
}
