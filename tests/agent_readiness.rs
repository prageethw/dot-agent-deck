//! Fast-tier, pure-data coverage for issue #243's per-event readiness fact: the
//! `session_start_origin` marker a wrapper stamps on the `SessionStart` it emits.
//!
//! There are three values and three predicates over them, and the behavioural
//! tests that exercise them each reach exactly one value, so the table itself is
//! pinned here, where an accidental widening is visible instead of silently
//! repricing a seam that reads it.
//!
//! Upstream sync 2026-11-05: this file used to also pin the per-agent half of #243
//! (`AgentSpec::pre_prompt_readiness`, `agent_readiness_001` and `_002`). Upstream
//! deleted `PrePromptReadiness` and the delegate / scheduler readiness gates that
//! read it with the typed prompt delivery (PRD #1666 S12), so those two tests
//! were retired with the API they pinned. `agent_readiness_003` stays: the origin
//! predicates it pins still exist on both sides.

use std::collections::HashMap;

use dot_agent_deck::event::{
    AgentEvent, AgentType, EventType, SESSION_START_ORIGIN_METADATA_KEY,
    WRAPPER_FORK_SESSION_START_ORIGIN, WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN,
    WRAPPER_INTERFACE_SETTLED_SESSION_START_ORIGIN,
};
use spec::spec;

/// A `SessionStart` carrying `origin` under the origin metadata key, or none at
/// all when `origin` is `None` — the shape every non-wrapper producer posts.
fn session_start_with_origin(origin: Option<&str>) -> AgentEvent {
    let mut metadata = HashMap::new();
    if let Some(origin) = origin {
        metadata.insert(
            SESSION_START_ORIGIN_METADATA_KEY.to_string(),
            origin.to_string(),
        );
    }
    AgentEvent {
        session_id: "session-origin-table".to_string(),
        agent_type: AgentType::Codex,
        event_type: EventType::SessionStart,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: chrono::Utc::now(),
        user_prompt: None,
        metadata,
        pane_id: Some("pane-origin-table".to_string()),
        agent_id: Some("agent-origin-table".to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
        background: None,
        background_revision: None,
    }
}

/// Scenario: Build a `SessionStart` for each of the wrapper's three
/// `session_start_origin` values and one with no origin key at all, then ask all
/// four of `AgentEvent`'s origin predicates about each. The full truth table must
/// hold — in particular the settled marker must NOT answer the strong
/// raw-input-mode question, because that is the single bit deciding which of the
/// wrapper's two facts may RELEASE the readiness gate, and which post-readiness
/// buffer is then owed.
#[spec("agent/readiness/003")]
#[test]
fn agent_readiness_003_session_start_origin_predicates_price_the_two_facts_apart() {
    // (origin, fork?, interface-READY?, interface-SETTLED?)
    // The remaining two predicates are derived below rather than tabulated, so
    // a table typo cannot make them agree with themselves.
    let rows: [(Option<&str>, bool, bool, bool); 4] = [
        // No marker at all: every hook-emitting agent's own `SessionStart`, and
        // the shape a pre-#243 wrapper still posts. None of the wrapper
        // questions may answer `true` for it.
        (None, false, false, false),
        // Boot provenance: the wrapper saying "I forked a child", which says
        // nothing about the child's interface.
        (Some(WRAPPER_FORK_SESSION_START_ORIGIN), true, false, false),
        // Fact 1, the OBSERVATION: the child cleared `ICANON`/`ECHO`. The one
        // value a Wrapper-strategy agent's gate may be released on before its
        // upgrade window expires, and the one priced at
        // `WRAPPER_INTERFACE_READINESS_BUFFER` rather than the ordinary buffer.
        // It used to be the one value that SKIPPED the buffer; measurement
        // retracted that (a TUI takes raw mode at INIT, before it will accept a
        // submit), and the predicates below are unchanged by the retraction.
        (
            Some(WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN),
            false,
            true,
            false,
        ),
        // Fact 2, the GUESS: the child wrote something and then went quiet for
        // the settle window. A stalled launcher settles exactly like a REPL at
        // its prompt, so this must stay OUT of the strong predicate.
        (
            Some(WRAPPER_INTERFACE_SETTLED_SESSION_START_ORIGIN),
            false,
            false,
            true,
        ),
    ];

    for (origin, fork, interface_ready, interface_settled) in rows {
        let event = session_start_with_origin(origin);
        assert_eq!(
            event.is_wrapper_fork_session_start(),
            fork,
            "origin {origin:?}: is_wrapper_fork_session_start"
        );
        assert_eq!(
            event.is_wrapper_interface_ready_session_start(),
            interface_ready,
            "origin {origin:?}: is_wrapper_interface_ready_session_start — this is the bit the \
             delegate's buffer skip reads (`src/state.rs`, guard 1), so widening it reprices the \
             seam"
        );
        assert_eq!(
            event.is_wrapper_interface_settled_session_start(),
            interface_settled,
            "origin {origin:?}: is_wrapper_interface_settled_session_start"
        );
        // The two composites are definitionally the unions, and stating them
        // that way pins the RELATIONSHIP rather than four independent booleans:
        // whichever value is added next, the composites must still be the unions
        // of the narrow answers, and `is_wrapper_session_start` must still be
        // the one that also admits the fork marker.
        assert_eq!(
            event.is_wrapper_interface_session_start(),
            interface_ready || interface_settled,
            "origin {origin:?}: the EITHER-fact predicate must be exactly the union of the two \
             narrow ones — it is what releases the readiness GATE, which both facts may do"
        );
        assert_eq!(
            event.is_wrapper_session_start(),
            fork || interface_ready || interface_settled,
            "origin {origin:?}: the any-wrapper predicate must admit the fork marker as well as \
             both interface facts"
        );
    }

    // An unrecognised value must read as "not a wrapper marker" rather than as
    // any of the three: a forward-compatible producer inventing a fourth origin
    // must not inherit the privilege the strong one carries.
    let unknown = session_start_with_origin(Some("wrapper_interface_ready_v2"));
    assert!(
        !unknown.is_wrapper_session_start(),
        "an unrecognised origin value must not satisfy ANY wrapper predicate; prefix matching \
         here would hand a future or hostile value the buffer skip"
    );
}
