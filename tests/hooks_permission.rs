use chrono::Utc;
use dot_agent_deck::event::{AgentEvent, AgentType, EventType};
use dot_agent_deck::state::{AppState, SessionStatus};

use spec::spec;

const PANE_ID: &str = "permission-marker-pane";
const SESSION_ID: &str = "permission-marker-session";
const AGENT_ID: &str = "permission-marker-agent";

fn event(event_type: EventType, tool_name: Option<&str>) -> AgentEvent {
    AgentEvent {
        session_id: SESSION_ID.to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type,
        tool_name: tool_name.map(str::to_string),
        tool_detail: None,
        cwd: Some("/tmp/runbox".to_string()),
        timestamp: Utc::now(),
        user_prompt: None,
        metadata: Default::default(),
        pane_id: Some(PANE_ID.to_string()),
        agent_id: Some(AGENT_ID.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    }
}

/// Scenario: A session reaches WaitingForInput on a `Bash` permission prompt, then the SAME `Bash` tool's `PreToolUse` (`ToolStart`) fires once approved. The status must move off WaitingForInput instead of sticking for the tool's whole run.
#[spec("hooks/permission/001")]
#[test]
fn hooks_permission_001_tool_start_matching_the_approved_tool_clears_waiting_for_input() {
    let mut state = AppState::default();
    state.register_pane(PANE_ID.to_string());

    state.apply_event(event(EventType::SessionStart, None));
    state.apply_event(event(EventType::PermissionRequest, Some("Bash")));
    assert_eq!(
        state.sessions[SESSION_ID].status,
        SessionStatus::WaitingForInput,
        "precondition: the permission prompt must have armed WaitingForInput"
    );

    state.apply_event(event(EventType::ToolStart, Some("Bash")));

    assert_eq!(
        state.sessions[SESSION_ID].status,
        SessionStatus::Working,
        "the approved tool's own ToolStart must clear WaitingForInput instead of leaving the badge stuck for the tool's whole run"
    );
}

/// Scenario: A session is WaitingForInput on a `Bash` permission prompt when an unrelated `Read` tool's `PreToolUse` (`ToolStart`) fires — the concurrent-subagent case. The WaitingForInput badge must NOT be cleared by a tool that was never the one awaiting approval.
#[spec("hooks/permission/002")]
#[test]
fn hooks_permission_002_tool_start_for_an_unrelated_tool_preserves_waiting_for_input() {
    let mut state = AppState::default();
    state.register_pane(PANE_ID.to_string());

    state.apply_event(event(EventType::SessionStart, None));
    state.apply_event(event(EventType::PermissionRequest, Some("Bash")));
    assert_eq!(
        state.sessions[SESSION_ID].status,
        SessionStatus::WaitingForInput,
        "precondition: the permission prompt must have armed WaitingForInput"
    );

    state.apply_event(event(EventType::ToolStart, Some("Read")));

    assert_eq!(
        state.sessions[SESSION_ID].status,
        SessionStatus::WaitingForInput,
        "an unrelated tool's ToolStart must not clear a badge that isn't waiting on it"
    );
}

/// Scenario: A session reaches WaitingForInput via a `PermissionRequest` that carries no tool name — OpenCode's real `permission.asked` payload never includes one (`src/opencode_manage.rs`'s `permissionPayload` sends only `session_id`/`event`/`prompt`/`cwd`). An unrelated tool's concurrent `ToolStart` must NOT clear the badge, same as the named case in `hooks/permission/002`.
#[spec("hooks/permission/003")]
#[test]
fn hooks_permission_003_tool_start_after_a_nameless_permission_request_preserves_waiting_for_input()
{
    let mut state = AppState::default();
    state.register_pane(PANE_ID.to_string());

    state.apply_event(event(EventType::SessionStart, None));
    state.apply_event(event(EventType::PermissionRequest, None));
    assert_eq!(
        state.sessions[SESSION_ID].status,
        SessionStatus::WaitingForInput,
        "precondition: a nameless permission prompt must still arm WaitingForInput"
    );

    state.apply_event(event(EventType::ToolStart, Some("Read")));

    assert_eq!(
        state.sessions[SESSION_ID].status,
        SessionStatus::WaitingForInput,
        "a PermissionRequest with no tool_name sets pending_permission_tool to None, \
         indistinguishable from 'no permission pending' — an unrelated ToolStart must \
         not be treated as the human's approval taking effect and clear the badge"
    );
}
