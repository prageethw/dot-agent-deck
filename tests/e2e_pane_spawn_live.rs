#![cfg(feature = "e2e")]

//! PRD #699 M4: PTY-attached e2e coverage for what M3's daemon-hosted `pane
//! spawn <role>` verb does to an ALREADY-ATTACHED TUI.
//!
//! M3 (merged, GREEN — `tests/pane_spawn.rs`) added the daemon verb itself and
//! proved it end-to-end at the handler level: a role declared in
//! `.dot-agent-deck.toml` but never spawned into a running orchestration
//! instance can be spawned on demand, and afterward is reachable via
//! `delegate_targets`. What M3 deliberately left unhandled — see
//! `handle_spawn_role_with_state`'s own doc comment in `src/state.rs`
//! ("a live TUI merging this into an already-open orchestration tab is
//! PRD #699 M4's job, not this one's") — is whether an ALREADY-ATTACHED TUI
//! correctly absorbs the resulting `BroadcastMsg::OrchestrationSurface`
//! broadcast into the orchestration tab it already has open, rather than
//! building a second, confusing tab for the same orchestration. That is
//! fundamentally a rendered-UI behavior no handler-level test can exercise —
//! exactly why this file exists as a genuine L2, PTY-attached test (modeled
//! on `tests/e2e_scheduler_live_surface.rs`, the only other file that drives
//! `surface_one_orchestration` end-to-end via a real attached `TuiDeck`),
//! unlike M1-M3's handler-level suites.
//!
//! The bug, precisely: `surface_one_orchestration`'s idempotency guard
//! (`src/ui.rs`) only checks whether the pane ids IN THIS SURFACE already
//! belong to some tab. A `pane spawn` broadcast carries only the ONE
//! newly-spawned role's (novel) pane id, so the guard is always `false` for
//! it, and the function falls straight through to
//! `open_orchestration_tab_with_existing_role_panes`, which unconditionally
//! pushes a brand-new `Tab::Orchestration` — producing a SECOND tab for the
//! same orchestration instead of the new role's card joining the tab already
//! open for it.
//!
//! `pane/spawn/005` reuses the `pane/spawn` catalog area M3 established
//! (`tests/pane_spawn.rs`, ids `001`-`004`, all L1/handler-level) rather than
//! opening a new one — this is the SAME `pane spawn` verb's TUI-visible
//! consequence, not a different feature. `005`, not `003`: the task that
//! commissioned this test named `pane/spawn/003`, expecting only two prior
//! ids in this area, but `tests/pane_spawn.rs` already claims `001`-`004`
//! (all four of M3's own scenarios, all L1) — reusing any of them would be a
//! duplicate `#[spec(...)]` id, which `cargo xtask linkage-check`'s
//! `duplicate_catalog_id` test rejects outright. `005` is the next free slot
//! and the first PTY/L2 entry in this area.

mod common;

use std::time::Duration;

use common::{TuiDeck, commit_fixture, open_orchestration, role_pane_border_title};
use dot_agent_deck::agent_pty::TabMembership;
use spec::spec;

/// Scenario: Open a real Orchestration tab (`pane-spawn-live` fixture: one
/// orchestration, roles `orchestrator` [start] + `coder`, both spawned the
/// instant the tab opens) and confirm both role cards are visible together.
/// Then mutate the RUNNING orchestration's own `.dot-agent-deck.toml` (read
/// back from the daemon's registry, at the isolated-clone cwd its role panes
/// actually run in — never the launch directory) to add a THIRD role,
/// `reviewer`, that was never part of the config this tab was opened from —
/// mirroring the "operator edited the config mid-session" scenario
/// `tests/pane_spawn.rs` (M3) already exercises at the handler level. Invoke
/// the REAL `dot-agent-deck pane spawn reviewer` CLI subcommand exactly as a
/// real orchestrator agent would from inside its own pane (a subprocess
/// carrying only `DOT_AGENT_DECK_PANE_ID` + the hook socket path — the daemon
/// resolves the caller from those alone, never from this subprocess's own
/// filesystem cwd). Assert reviewer's card joins the SAME orchestration tab
/// that is still active (this test never switches tabs) and that the tab bar
/// still shows exactly one Dashboard tab + one orchestration tab — not two
/// orchestration tabs for the same orchestration.
#[spec("pane/spawn/005")]
#[test]
fn spawn_005_pane_spawn_joins_the_already_open_orchestration_tab() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("pane-spawn-live");
    let work = deck.workdir().to_path_buf();
    // Isolated-clone provisioning needs a ref to branch from — an unborn HEAD
    // (the harness's own bare `git init`) does not provide one.
    commit_fixture(&work);
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode, orchestrator focused

    // Precondition: both configured roles spawned into ONE tab at open time.
    assert!(
        deck.wait_for_grid_string_within("coder", Duration::from_secs(10)),
        "the coder role card must be visible on the freshly-opened orchestration \
         tab before this test touches anything else.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Precondition: exactly Dashboard + this ONE orchestration tab. Tab
    // labels are joined by a bare `│` divider with no other occurrence on the
    // one-row tab-bar strip (`render_tab_strip`, `src/ui.rs`), so counting
    // dividers on that row is an exact tab count, independent of what either
    // tab happens to be titled.
    let tab_bar_row_before = deck
        .snapshot_grid()
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let tabs_before = tab_bar_row_before.matches('│').count() + 1;
    assert_eq!(
        tabs_before, 2,
        "precondition: exactly one Dashboard tab + one orchestration tab must be \
         open before the spawn.\nTab bar row: {tab_bar_row_before:?}"
    );

    // Read back the orchestrator's real (daemon-minted) pane id and the
    // isolated-clone cwd its role panes actually run in -- both come from the
    // daemon's own registry, never reconstructed by hand, so they are exactly
    // what `handle_spawn_role_with_state` and `surface_one_orchestration`
    // will themselves resolve.
    let records = common::agent_records_on(deck.attach_socket_path());
    let orchestrator_record = records
        .iter()
        .find(|r| {
            matches!(
                &r.tab_membership,
                Some(TabMembership::Orchestration { role_name, is_start_role: true, .. })
                    if role_name == "orchestrator"
            )
        })
        .expect("the orchestrator role must be registered with its tab membership");
    let orchestrator_pane_id = orchestrator_record
        .pane_id_env
        .clone()
        .expect("the orchestrator role must carry a DOT_AGENT_DECK_PANE_ID");
    let orchestration_cwd = orchestrator_record
        .cwd
        .clone()
        .expect("the orchestrator role must carry its (isolated-clone) cwd");

    // Mirror the real-world "operator added a role mid-session" scenario
    // `tests/pane_spawn.rs` (M3) exercises at the handler level: mutate the
    // RUNNING orchestration's own config file -- in its isolated-clone cwd,
    // NOT the launch directory `work` -- to add the `reviewer` role that was
    // never part of the config this tab was opened from.
    let running_config_path = std::path::Path::new(&orchestration_cwd).join(".dot-agent-deck.toml");
    std::fs::write(
        &running_config_path,
        "[[orchestrations]]\n\
         name = \"spawn-orch\"\n\
         \n\
         [[orchestrations.roles]]\n\
         name = \"orchestrator\"\n\
         command = \"cat\"\n\
         start = true\n\
         \n\
         [[orchestrations.roles]]\n\
         name = \"coder\"\n\
         command = \"cat\"\n\
         \n\
         [[orchestrations.roles]]\n\
         name = \"reviewer\"\n\
         command = \"cat\"\n",
    )
    .expect("add the reviewer role to the running orchestration's own config");

    // Invoke the REAL `pane spawn reviewer` CLI subcommand exactly as a real
    // orchestrator agent would from inside its own pane (`src/main.rs`'s
    // `PaneCmd::Spawn` arm): a plain subprocess carrying only
    // `DOT_AGENT_DECK_PANE_ID` and the hook socket path over the same
    // unversioned `DaemonMessage` channel `delegate`/`pane restart` already
    // use. The daemon resolves the caller from those two alone -- never from
    // this subprocess's own filesystem cwd, which stays the test's tempdir.
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let output = std::process::Command::new(bin)
        .args(["pane", "spawn", "reviewer"])
        .env("DOT_AGENT_DECK_PANE_ID", &orchestrator_pane_id)
        .env("DOT_AGENT_DECK_SOCKET", deck.hook_socket_path())
        .output()
        .expect("run `dot-agent-deck pane spawn reviewer`");
    assert!(
        output.status.success(),
        "`pane spawn reviewer` must succeed at the daemon level (M3's already-GREEN \
         verb, not this test's own pin) -- stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Give the already-attached TUI's event-subscriber a chance to drain the
    // queued `OrchestrationSurface` broadcast and (attempt to) merge it.
    deck.wait_until_quiescent();

    // The pin, part 1: reviewer's card must join the SAME orchestration tab
    // that is still active -- this test never sends a tab-switch chord, so if
    // reviewer is visible at all it can only be on the tab already open.
    assert!(
        deck.wait_for_grid_string_within("reviewer", Duration::from_secs(10)),
        "spawning `reviewer` into an orchestration that already has an ATTACHED \
         tab open (orchestrator + coder) must make reviewer's card join that SAME \
         tab -- it never appeared on screen at all, meaning it was built into a \
         tab this test never switched to.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // The pin, part 2: still only ONE orchestration tab (+ Dashboard) exists
    // -- the spawn must not have built a SECOND tab for the same
    // orchestration alongside the one already open.
    let grid_after = deck.snapshot_grid();
    let tab_bar_row_after = grid_after.lines().next().unwrap_or_default();
    let tabs_after = tab_bar_row_after.matches('│').count() + 1;
    assert_eq!(
        tabs_after, 2,
        "spawning a configured-but-unspawned role into an ALREADY-OPEN \
         orchestration tab must not create a second tab for the same \
         orchestration -- expected the tab bar to still show exactly 2 tabs \
         (Dashboard + the one orchestration tab), got {tabs_after} from row \
         {tab_bar_row_after:?}.\nFull grid:\n{grid_after}"
    );

    // And reviewer's card specifically lives inside the role-pane box
    // structure of that tab (its own bordered box titled "reviewer"), not
    // merely as stray text somewhere on screen.
    assert_eq!(
        role_pane_border_title(&grid_after, "reviewer").as_deref(),
        Some("reviewer"),
        "reviewer's role pane must render as its own bordered box titled \
         'reviewer' inside the currently-visible orchestration tab.\nGrid:\n{grid_after}"
    );
}
