//! PRD #699 fix-round RED tests for the `TabManager`-level half of M4's
//! "grow an already-open orchestration tab" path
//! (`TabManager::add_role_to_existing_orchestration` /
//! `TabManager::orchestration_tab_index_for`), driven directly against a
//! `TabManager` with a trivial no-op `PaneController` — no daemon, no PTY,
//! no `ui.rs`/`EmbeddedPaneController` involved. This mirrors
//! `tests/pane_close.rs`'s own `DelayedCloseController` technique for
//! testing `TabManager` in isolation.
//!
//! Two orchestrator-review findings live here:
//!
//! - `pane/spawn/009` (M4 / auditor F4): growing a tab that already carries
//!   a DEAD SLOT for the role being spawned appends a second entry instead
//!   of replacing the dead one. This test compiles and runs today — it is a
//!   genuine behavioral RED against the code as it exists now.
//! - `pane/spawn/010` (B2 / auditor F1): `orchestration_tab_index_for`
//!   matches on the bare `(cwd, name)` tuple, so two orchestration tabs that
//!   share both (told apart only by their PRD #140 `Instance` orchestration
//!   id) are indistinguishable and a role meant for one gets grown into the
//!   other. Fixing this needs a per-tab `orchestration_id` the `Tab`/
//!   `TabManager` API does not carry today, so this test deliberately calls
//!   `open_orchestration_tab_with_existing_role_panes` and
//!   `orchestration_tab_index_for` with an extra `orchestration_id`
//!   argument neither accepts yet — this file does NOT compile until the
//!   coder delegation adds it, exactly the same "RED that fails to compile"
//!   pattern already established elsewhere in this PRD (see
//!   `tests/pane_spawn.rs`'s own module doc history). The additive shape
//!   chosen here (`Option<&str>`, trailing parameter) is a proposal, not a
//!   mandate — coordinate with whatever shape the fix actually takes.

#![cfg(unix)]

use std::sync::Arc;

use dot_agent_deck::pane::{PaneController, PaneDirection, PaneError, PaneInfo, RenameOutcome};
use dot_agent_deck::project_config::{OrchestrationConfig, OrchestrationRoleConfig};
use dot_agent_deck::tab::{OrchestrationRoleStatus, Tab, TabManager};
use spec::spec;

/// A `PaneController` that does nothing and never fails — `TabManager`'s own
/// tab-shape bookkeeping (the surface under test here) never calls into the
/// controller for the operations these tests exercise, so every method is
/// an unconditional success stub.
#[derive(Default)]
struct NoopPaneController;

impl PaneController for NoopPaneController {
    fn focus_pane(&self, _pane_id: &str) -> Result<(), PaneError> {
        Ok(())
    }

    fn close_pane(&self, _pane_id: &str) -> Result<(), PaneError> {
        Ok(())
    }

    fn list_panes(&self) -> Result<Vec<PaneInfo>, PaneError> {
        Ok(Vec::new())
    }

    fn resize_pane(
        &self,
        _pane_id: &str,
        _direction: PaneDirection,
        _amount: u16,
    ) -> Result<(), PaneError> {
        Ok(())
    }

    fn rename_pane(&self, _pane_id: &str, name: &str) -> Result<RenameOutcome, PaneError> {
        Ok(RenameOutcome::applied(name))
    }

    fn toggle_layout(&self) -> Result<(), PaneError> {
        Ok(())
    }

    fn write_to_pane(&self, _pane_id: &str, _text: &str) -> Result<(), PaneError> {
        Ok(())
    }

    fn name(&self) -> &str {
        "noop"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn role(name: &str, start: bool) -> OrchestrationRoleConfig {
    OrchestrationRoleConfig {
        agent: None,
        name: name.to_string(),
        command: "cat".to_string(),
        start,
        description: None,
        prompt_template: None,
        clear: false,
    }
}

fn two_role_config(orchestration_name: &str) -> OrchestrationConfig {
    OrchestrationConfig {
        default: false,
        name: orchestration_name.to_string(),
        roles: vec![role("orchestrator", true), role("reviewer", false)],
    }
}

fn new_tab_manager() -> TabManager {
    TabManager::new(Arc::new(NoopPaneController))
}

/// Scenario: PRD #699 M4 (`findings-699-reviewer.md` "M4") / auditor F4.
/// Open an orchestration tab whose `reviewer` role has no live pane (a dead
/// slot — the shape produced when a role's pane closed while the TUI was
/// detached and gets reattached with no daemon registration to back it).
/// Grow that tab with a genuinely NEW `reviewer` pane, exactly the call
/// `pane spawn`'s daemon broadcast handler makes today. The tab must end up
/// with exactly ONE `reviewer` role entry — the dead slot's pane id
/// replaced and its status flipped from `Failed` to `Working` — not a
/// second `reviewer` entry appended alongside the dead one. RED today:
/// `add_role_to_existing_orchestration` always appends, so `config.roles`
/// ends up naming `reviewer` twice and `role_pane_ids` grows to 3 entries
/// for a 2-role config.
#[spec("pane/spawn/009")]
#[test]
fn pane_spawn_009_growing_a_dead_slot_replaces_it_instead_of_appending_a_duplicate() {
    let mut tabs = new_tab_manager();
    let config = two_role_config("dead-slot-orch");

    let (tab_index, _) = tabs
        .open_orchestration_tab_with_existing_role_panes(
            &config,
            "/work/dead-slot-cwd",
            vec![Some("orch-pane".to_string()), None],
            None,
            None,
        )
        .expect("open a tab with reviewer as a dead slot");

    let (grown_role_index, _was_new) = tabs
        .add_role_to_existing_orchestration(
            tab_index,
            role("reviewer", false),
            "fresh-reviewer-pane".to_string(),
        )
        .expect("grow the tab with a freshly-spawned reviewer");

    let Tab::Orchestration {
        role_pane_ids,
        role_statuses,
        config: grown_config,
        ..
    } = &tabs.tabs()[tab_index]
    else {
        panic!("expected an Orchestration tab at index {tab_index}");
    };

    let reviewer_entries = grown_config
        .roles
        .iter()
        .filter(|r| r.name == "reviewer")
        .count();
    assert_eq!(
        reviewer_entries,
        1,
        "growing a tab that already has a dead slot for `reviewer` must REPLACE that slot, \
         not append a second `reviewer` role config entry; roles = {:?}",
        grown_config
            .roles
            .iter()
            .map(|r| &r.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        grown_config.roles.len(),
        2,
        "the role COUNT must stay at 2 (orchestrator + reviewer), not grow to 3; roles = {:?}",
        grown_config
            .roles
            .iter()
            .map(|r| &r.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        role_pane_ids.len(),
        2,
        "role_pane_ids must stay at 2 entries, not grow to 3; role_pane_ids = {role_pane_ids:?}"
    );
    assert_eq!(
        role_pane_ids[1], "fresh-reviewer-pane",
        "the dead slot's pane id must be replaced by the newly-spawned pane, not left as the \
         empty dead-slot sentinel with a second entry appended; role_pane_ids = {role_pane_ids:?}"
    );
    assert_eq!(
        role_statuses[1],
        OrchestrationRoleStatus::Working,
        "the replaced slot's status must flip from the dead-slot Failed marker to Working; \
         role_statuses = {role_statuses:?}"
    );
    assert_eq!(
        grown_role_index, 1,
        "the returned role_index must name the REPLACED slot's position (1), not a freshly \
         appended index (2)"
    );
}

/// Scenario: PRD #699 B2 (`findings-699-reviewer.md` "B2") / auditor F1 —
/// rated the most significant finding in the audit. Two orchestration tabs
/// share the exact same orchestration `name` and `cwd` (a `Ctrl+N`-opened
/// second tab of the same config in the same directory, or the batch
/// producer `surface_spawned_orchestration` firing into a directory that
/// already has a tab open), told apart only by their PRD #140 `Instance`
/// orchestration id. `pane spawn`'s tab-growth path must resolve the tab
/// belonging to the CALLING orchestration instance specifically —
/// `orchestration_tab_index_for`'s bare `(cwd, name)` match cannot tell
/// them apart, so a role meant for instance B is grown into instance A's
/// tab instead. Build both tabs, resolve the lookup for instance B's id,
/// grow whichever tab that resolves to, then assert instance A's tab is
/// completely untouched (role count, pane ids) while instance B's tab is
/// the one that grew. Deliberately calls
/// `open_orchestration_tab_with_existing_role_panes` and
/// `orchestration_tab_index_for` with an extra `orchestration_id: Option<&str>`
/// argument neither accepts today — this file does not compile until the
/// coder delegation threads a per-tab orchestration id through both, which
/// is the fix this test exists to pin. RED (as a compile failure) today.
#[spec("pane/spawn/010")]
#[test]
fn pane_spawn_010_two_same_name_cwd_instances_do_not_cross_wire_on_growth() {
    let mut tabs = new_tab_manager();
    let cwd = "/work/shared-cwd";
    let name = "cross-wire-orch";
    let config = two_role_config(name);

    let (tab_a, _) = tabs
        .open_orchestration_tab_with_existing_role_panes(
            &config,
            cwd,
            vec![
                Some("a-orchestrator".to_string()),
                Some("a-reviewer".to_string()),
            ],
            None,
            Some("cross-wire-instance-a"),
        )
        .expect("open instance A's tab");
    let (tab_b, _) = tabs
        .open_orchestration_tab_with_existing_role_panes(
            &config,
            cwd,
            vec![
                Some("b-orchestrator".to_string()),
                Some("b-reviewer".to_string()),
            ],
            None,
            Some("cross-wire-instance-b"),
        )
        .expect("open instance B's tab");
    assert_ne!(tab_a, tab_b, "the two instances must be two distinct tabs");

    // The resolution instance B's own `pane spawn` growth path performs:
    // find the tab belonging to ITS orchestration_id, at this shared (cwd, name).
    let resolved_for_b = tabs
        .orchestration_tab_index_for(cwd, name, Some("cross-wire-instance-b"))
        .expect(
            "orchestration_tab_index_for must resolve SOME tab for instance B's own id, not \
             nothing",
        );
    assert_eq!(
        resolved_for_b, tab_b,
        "orchestration_tab_index_for must resolve the tab whose orchestration_id matches the \
         CALLER's (instance B), not just whichever same-(cwd,name) tab happens to be first \
         (instance A); got tab {resolved_for_b}, A = {tab_a}, B = {tab_b}"
    );

    let (grown_role_index, _was_new) = tabs
        .add_role_to_existing_orchestration(
            resolved_for_b,
            role("coder", false),
            "b-coder".to_string(),
        )
        .expect("grow instance B's tab with a new coder role");
    assert_eq!(grown_role_index, 2);

    let Tab::Orchestration {
        role_pane_ids: a_pane_ids,
        config: a_config,
        ..
    } = &tabs.tabs()[tab_a]
    else {
        panic!("expected instance A's tab to still be an Orchestration tab");
    };
    assert_eq!(
        a_pane_ids.len(),
        2,
        "instance A's tab must be COMPLETELY untouched by a role spawned into instance B; \
         role_pane_ids = {a_pane_ids:?}"
    );
    assert_eq!(
        a_config.roles.len(),
        2,
        "instance A's role config must be untouched; roles = {:?}",
        a_config.roles.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
    assert!(
        !a_pane_ids.contains(&"b-coder".to_string()),
        "instance B's new pane must never appear on instance A's tab; role_pane_ids = {a_pane_ids:?}"
    );

    let Tab::Orchestration {
        role_pane_ids: b_pane_ids,
        config: b_config,
        ..
    } = &tabs.tabs()[tab_b]
    else {
        panic!("expected instance B's tab to still be an Orchestration tab");
    };
    assert_eq!(
        b_pane_ids.len(),
        3,
        "instance B's tab must be the one that grew; role_pane_ids = {b_pane_ids:?}"
    );
    assert!(
        b_pane_ids.contains(&"b-coder".to_string()),
        "instance B's tab must carry the newly-spawned coder pane; role_pane_ids = {b_pane_ids:?}"
    );
    assert_eq!(b_config.roles.len(), 3);

    // A caller carrying NO orchestration_id (a pre-PRD#140 legacy surface)
    // must not accidentally match either tokened tab — the fix's stated
    // fallback behavior for the token-less case.
    let resolved_for_legacy = tabs.orchestration_tab_index_for(cwd, name, None);
    assert!(
        resolved_for_legacy.is_none(),
        "a token-less lookup must not match either tokened instance's tab; got \
         {resolved_for_legacy:?} (A = {tab_a}, B = {tab_b})"
    );
}
