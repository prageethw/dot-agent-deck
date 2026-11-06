//! The daemon is the authority on which orchestration run titles are taken —
//! issue #555.
//!
//! The `Ctrl+n` form refuses a title a live orchestration already holds, but it
//! decides from ONE `ListAgents` snapshot taken when the form opens
//! (`transition_after_dir_pick`). Two attached clients whose forms were open at
//! the same time each see a snapshot without the other's orchestration, both are
//! told the title is free, and both start — two tabs with indistinguishable
//! labels, and a `delegate`/`work-done` routing story told in names the user can
//! no longer tell apart. A snapshot is not an authority, so the check now lives
//! in the daemon's `StartAgent` handler, before the registry insert.
//!
//! These drive the REAL handler over the attach socket, exactly as a TUI or the
//! desktop does, against an in-process daemon. No LLM and no TUI: what is under
//! test is the accept/refuse decision and what it leaves behind; the TUI half of
//! the refusal (the form coming back with the collision warning) is
//! `orchestration/identity/009`.

#![cfg(unix)]

use dot_agent_deck::agent_pty::{DOT_AGENT_DECK_PANE_ID, SpawnOptions, TabMembership};
use dot_agent_deck::daemon_client::{ClientError, DaemonClient, StartAgentOptions};
use dot_agent_deck::daemon_protocol::START_ERR_ORCHESTRATION_TITLE_IN_USE;
use spec::spec;

mod common;

const ORCHESTRATION: &str = "review";

/// One role of one orchestration TAB. `orchestration_id` is the per-tab token
/// the TUI mints once per tab (PRD #140) — two tabs of the same orchestration in
/// the same directory differ only by it.
struct Role<'a> {
    command: &'a str,
    orchestration_id: &'a str,
    role_index: usize,
    role_name: &'a str,
    is_start_role: bool,
    display_title: Option<&'a str>,
    orchestration_cwd: &'a str,
}

/// Start one role through the daemon and answer the pane id the daemon minted
/// for it. Fork #365 (PRD #365 M2): the daemon, not the caller, is authoritative
/// for `pane_id`, so none is proposed; a caller that needs to name the pane
/// afterwards (its liveness, its registry record) uses the reply's.
async fn start_role(client: &DaemonClient, role: Role<'_>) -> Result<String, ClientError> {
    start_role_from(client, role, None).await
}

/// [`start_role`] as the fork's TUI starts a role of an ISOLATED CLONE: the
/// role's `orchestration_cwd` names the clone its panes run in, and
/// `isolated_clone_origin` the project directory the clone was made from. With
/// `None` it is exactly [`start_role`] — what a client that runs in the picked
/// directory itself (the desktop app, an older or upstream TUI) sends.
async fn start_role_from(
    client: &DaemonClient,
    role: Role<'_>,
    isolated_clone_origin: Option<&str>,
) -> Result<String, ClientError> {
    let (_agent_id, pane_id) = client
        .start_agent_with_pane_id(StartAgentOptions {
            command: Some(role.command.to_string()),
            cwd: Some(role.orchestration_cwd.to_string()),
            display_name: Some(role.role_name.to_string()),
            tab_membership: Some(TabMembership::Orchestration {
                name: ORCHESTRATION.to_string(),
                role_index: role.role_index,
                role_name: role.role_name.to_string(),
                is_start_role: role.is_start_role,
                orchestration_cwd: Some(role.orchestration_cwd.to_string()),
                display_title: role.display_title.map(str::to_string),
                orchestration_id: Some(role.orchestration_id.to_string()),
            }),
            isolated_clone_origin: isolated_clone_origin.map(str::to_string),
            ..StartAgentOptions::default()
        })
        .await?;
    Ok(pane_id.expect("the daemon must mint the started agent's pane_id (PRD #365 M2)"))
}

fn assert_title_refusal(result: Result<String, ClientError>, title: &str) {
    match result {
        Err(ClientError::Server(message)) => {
            assert!(
                message.starts_with(START_ERR_ORCHESTRATION_TITLE_IN_USE),
                "the refusal must carry the stable `{START_ERR_ORCHESTRATION_TITLE_IN_USE}` prefix \
                 a client keys its form-reopen on, got: {message}"
            );
            assert!(
                message.contains(title),
                "the refusal must name the title it refused, got: {message}"
            );
        }
        other => panic!(
            "a second orchestration tab under the title `{title}` in the same directory must be \
             REFUSED by the daemon — two tabs with indistinguishable labels is issue #555 — got: \
             {other:?}"
        ),
    }
}

/// Scenario: two clients each open an orchestration tab of the same config in
/// the same directory under the same run title — the race the form's one-shot
/// snapshot cannot see. The first tab's roles all start; the second tab's start
/// role is refused with a specific title-in-use error and nothing is spawned for
/// it, while the same second tab under a different title starts normally. Then
/// eight clients start eight tabs under one fresh title at the same instant, and
/// exactly one of them wins.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/identity/007")]
async fn identity_007_a_concurrent_duplicate_run_title_is_refused_by_the_daemon() {
    let daemon = common::spawn_inprocess_daemon().await;
    let dir = common::race_safe_tempdir();
    let cwd = dir.path().to_string_lossy().into_owned();
    let client = DaemonClient::new(daemon.attach_path.clone());
    let title = "proj-orchestrator-1";

    // Client A: its tab's start role claims the title.
    start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "tab-a",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some(title),
            orchestration_cwd: &cwd,
        },
    )
    .await
    .expect("the first tab under a free title must start");

    // Client B, whose form snapshot predates A: same title, same directory,
    // different tab. This is the start the daemon must refuse.
    let agents_before_refusal = daemon.registry.agent_records().len();
    let refused = start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "tab-b",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some(title),
            orchestration_cwd: &cwd,
        },
    )
    .await;
    assert_title_refusal(refused, title);
    // Counted rather than looked up by name: the daemon mints the pane id, so a
    // name the test chose could never match a pane that did start.
    assert_eq!(
        daemon.registry.agent_records().len(),
        agents_before_refusal,
        "a refused start must spawn nothing — the refusal is before the registry insert; \
         records = {:?}",
        daemon.registry.agent_records()
    );

    // The load-bearing half of the design: an orchestration tab is N separate
    // `StartAgent` calls, one per role, and role 0 has just registered the
    // title. Scoping by the per-tab token is what stops the tab colliding with
    // itself — without it every tab would be half-built.
    start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "tab-a",
            role_index: 1,
            role_name: "coder",
            is_start_role: false,
            display_title: Some(title),
            orchestration_cwd: &cwd,
        },
    )
    .await
    .expect("the title-holding tab's own later roles must never collide with it");

    // Control: the refused client picks another name, and it starts — so the
    // refusal above is about the TITLE, not about a second tab of the same
    // orchestration in the same directory (which PRD #140 supports).
    start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "tab-b",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some("proj-orchestrator-2"),
            orchestration_cwd: &cwd,
        },
    )
    .await
    .expect("the same second tab under a free title must start");

    // The genuinely concurrent case: eight clients start eight tabs under one
    // fresh title at the same instant, so no start has a live pane to be seen by
    // when the others are checked. Exactly one may win. A daemon that counted
    // only live panes as holders — not a start still in flight — admits several.
    let racing_title = "proj-orchestrator-3";
    let tab_ids: Vec<String> = (0..8).map(|n| format!("race-tab-{n}")).collect();
    let starts = tab_ids.iter().map(|tab_id| {
        let client = DaemonClient::new(daemon.attach_path.clone());
        let cwd = cwd.clone();
        async move {
            start_role(
                &client,
                Role {
                    command: "cat",
                    orchestration_id: tab_id,
                    role_index: 0,
                    role_name: "orchestrator",
                    is_start_role: true,
                    display_title: Some(racing_title),
                    orchestration_cwd: &cwd,
                },
            )
            .await
        }
    });
    let results = futures_util::future::join_all(starts).await;
    let started = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        started, 1,
        "exactly one of eight concurrent starts under one title may win; results = {results:?}"
    );
    for result in results.into_iter().filter(|r| r.is_err()) {
        assert_title_refusal(result, racing_title);
    }
}

/// Scenario: the edges of what counts as "taken". A tab that typed no name runs
/// under its canonical orchestration name, and a second tab typing that name is
/// refused; the same title in a different directory is a different key and
/// starts, while a symlinked alias of the holder's directory does not; a title
/// carrying control bytes claims the canonical name the tab will actually show;
/// and once every pane of the title-holding tab has exited, the title can be
/// claimed again — even with an unrelated agent reusing the holder's pane id —
/// rather than being unclaimable for the rest of the daemon's life. The same
/// goes for a run the daemon spawns itself, as a scheduled fire does: a client
/// typing its title is refused while it runs, and admitted once it has exited.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/identity/008")]
async fn identity_008_a_title_is_keyed_by_resolved_title_and_cwd_and_freed_on_exit() {
    let daemon = common::spawn_inprocess_daemon().await;
    let dir = common::race_safe_tempdir();
    let cwd = dir.path().to_string_lossy().into_owned();
    let other_dir = common::race_safe_tempdir();
    let other_cwd = other_dir.path().to_string_lossy().into_owned();
    let client = DaemonClient::new(daemon.attach_path.clone());

    // No typed name: the tab's title is the canonical orchestration name, the
    // same `unwrap_or(name)` fallback the TUI's tab label and the form's
    // snapshot apply.
    let release = dir.path().join("release-holder");
    let holder_command = format!(
        "sh -c 'while [ ! -e {} ]; do sleep 0.05; done'",
        release.display()
    );
    let holder_pane = start_role(
        &client,
        Role {
            command: &holder_command,
            orchestration_id: "unnamed-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: None,
            orchestration_cwd: &cwd,
        },
    )
    .await
    .expect("an unnamed orchestration must start");

    let refused = start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "typed-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some(ORCHESTRATION),
            orchestration_cwd: &cwd,
        },
    )
    .await;
    assert_title_refusal(refused, ORCHESTRATION);

    // Keyed on the orchestration cwd as well as the title: the same resolved
    // title in another project is not a collision.
    start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "elsewhere-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: None,
            orchestration_cwd: &other_cwd,
        },
    )
    .await
    .expect("the same title in a different directory is a different key");

    // ...but an ALIAS of the holder's directory is the same directory: a symlink
    // to it names the same project, the same `.dot-agent-deck/` files and, to
    // the user, the same tab label.
    let alias = other_dir.path().join("alias-of-holder-dir");
    std::os::unix::fs::symlink(dir.path(), &alias).expect("symlink the holder's directory");
    let refused = start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "alias-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: None,
            orchestration_cwd: &alias.to_string_lossy(),
        },
    )
    .await;
    assert_title_refusal(refused, ORCHESTRATION);

    // A title carrying control bytes is not what the tab shows — the registry
    // drops it and the tab falls back to its canonical name — so the canonical
    // name is what it claims, and it collides with the untitled tab already
    // running in `other_dir`.
    let refused = start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "control-byte-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some("smuggled\u{1b}[31mtitle"),
            orchestration_cwd: &other_cwd,
        },
    )
    .await;
    assert_title_refusal(refused, ORCHESTRATION);

    // The holder's only pane exits. Its role-map entry stays — nothing but a
    // pane CLOSE unregisters it — so a check that did not consult liveness
    // would leave the title unclaimable forever.
    tokio::fs::write(&release, b"")
        .await
        .expect("release the holder's stand-in");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while daemon.registry.has_live_pane(&holder_pane) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "precondition: the holder's stand-in never exited"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // A pane id is a reusable slot: an unrelated, non-orchestration agent now
    // runs on the holder's old pane id, while the holder's stale role-map entry
    // still maps that id to its orchestration. That is not the holder coming
    // back, and must not hold its title.
    //
    // Spawned into the registry directly, the way the daemon's own respawn
    // reuses a pane: fork #365 (PRD #365 M2) makes `StartAgent` mint every pane
    // id and refuse a `desktop-` one an exited agent still holds, so no start
    // over the attach socket can put a second agent on an existing pane id.
    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), holder_pane.clone())],
            ..SpawnOptions::default()
        })
        .expect("a plain pane may reuse an exited pane's id");

    start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "typed-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some(ORCHESTRATION),
            orchestration_cwd: &cwd,
        },
    )
    .await
    .expect("a title whose every pane has exited must be claimable again");

    // Issue #1339: the same holds for a run the daemon spawned ITSELF (a
    // scheduled fire, `dispatch`, issue dispatch). It is admitted through the
    // same check and holds its title while live — a client typing it is
    // refused — and its admission claim is released once its roles are
    // registered, so the title is free again once those roles exit rather
    // than held by a claim nothing will ever end.
    let spawned_dir = common::race_safe_tempdir();
    let spawned_cwd = spawned_dir.path().to_string_lossy().into_owned();
    let spawned_release = spawned_dir.path().join("release-roles");
    let role_command = format!(
        "sh -c 'while [ ! -e {} ]; do sleep 0.05; done'",
        spawned_release.display()
    );
    tokio::fs::write(
        spawned_dir.path().join(".dot-agent-deck.toml"),
        format!(
            "[[orchestrations]]\nname = \"{ORCHESTRATION}\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"{role_command}\"\nstart = true\n\n\
             [[orchestrations.roles]]\nname = \"coder\"\ncommand = \"{role_command}\"\n"
        ),
    )
    .await
    .expect("write the spawned run's config");
    let spawned = dot_agent_deck::spawn::spawn(
        dot_agent_deck::spawn::SpawnRequest {
            task_name: "scheduled-review".to_string(),
            working_dir: spawned_cwd.clone(),
            command: None,
            prompt: "review".to_string(),
            resolved_target: None,
            compose_orchestrator_context: None,
            seed_reason: None,
            owner: None,
        },
        &daemon.registry,
        &QuietNotifier,
        Some(&daemon.event_tx),
        true,
        Some(&daemon.state),
    )
    .await
    .expect("the daemon's own spawn primitive must start the run");
    let spawned_title = format!(
        "{ORCHESTRATION} · {}",
        spawned_dir
            .path()
            .file_name()
            .expect("a tempdir has a basename")
            .to_string_lossy()
    );
    let refused = start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "after-spawn-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some(&spawned_title),
            orchestration_cwd: &spawned_cwd,
        },
    )
    .await;
    assert_title_refusal(refused, &spawned_title);

    tokio::fs::write(&spawned_release, b"")
        .await
        .expect("release the spawned run's stand-ins");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while spawned
        .agents
        .iter()
        .any(|a| daemon.registry.has_live_pane(&a.pane_id))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "precondition: the spawned run's stand-ins never exited"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    start_role(
        &client,
        Role {
            command: "cat",
            orchestration_id: "after-spawn-tab",
            role_index: 0,
            role_name: "orchestrator",
            is_start_role: true,
            display_title: Some(&spawned_title),
            orchestration_cwd: &spawned_cwd,
        },
    )
    .await
    .expect(
        "a daemon-spawned run's title must be free once its roles have exited — still refused \
         here means the spawn's own admission claim was never released",
    );
}

/// The one role start of a tab that the run-title check reads, as a client
/// sends it: its per-tab token, the title it asks for and the directory it names
/// as its `orchestration_cwd`.
fn title_role<'a>(tab: &'a str, title: &'a str, orchestration_cwd: &'a str) -> Role<'a> {
    Role {
        command: "cat",
        orchestration_id: tab,
        role_index: 0,
        role_name: "orchestrator",
        is_start_role: true,
        display_title: Some(title),
        orchestration_cwd,
    }
}

/// Scenario: the fork's TUI runs every orchestration in an isolated clone and names the clone as its `orchestration_cwd`, sending the project directory only as `isolated_clone_origin`, while the desktop app names the project directory itself. Two raw `StartAgent` requests for one run title, one of each kind, are therefore one project's two tabs: in either order the later is refused with the title-in-use error and spawns nothing, and so is a second cloned start of the same project. Controls: the same title from a cloned start of another project is admitted, and a start that names the clone's own directory but no origin keeps keying on that directory, as before.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/identity/043")]
async fn identity_043_a_cloned_start_and_a_source_directory_start_share_one_run_title() {
    let daemon = common::spawn_inprocess_daemon().await;
    let client = DaemonClient::new(daemon.attach_path.clone());
    let fresh_dir = || {
        let dir = common::race_safe_tempdir();
        let path = dir.path().to_string_lossy().into_owned();
        (dir, path)
    };
    // The project, and the sibling clones the fork's TUI would make of it. The
    // guards stay bound so no directory is removed while a pane runs in it.
    let (_source_guard, source) = fresh_dir();
    let (_clone_guards, clones): (Vec<_>, Vec<_>) = (0..5).map(|_| fresh_dir()).unzip();
    // A second project, with its own clone.
    let (_other_source_guard, other_source) = fresh_dir();
    let (_other_clone_guard, other_clone) = fresh_dir();

    // A source-directory client first (the desktop app), then the fork's TUI.
    let title = "proj-orchestrator-1";
    start_role(&client, title_role("desktop-1", title, &source))
        .await
        .expect("a free title is admitted");
    let agents = daemon.registry.agent_records().len();
    let refused = start_role_from(
        &client,
        title_role("deck-1", title, &clones[0]),
        Some(&source),
    )
    .await;
    assert_title_refusal(refused, title);
    assert_eq!(
        daemon.registry.agent_records().len(),
        agents,
        "a cloned start refused for the title a source-directory start holds must spawn nothing"
    );

    // The fork's TUI first, then a source-directory client.
    let title = "proj-orchestrator-2";
    start_role_from(
        &client,
        title_role("deck-2", title, &clones[1]),
        Some(&source),
    )
    .await
    .expect("a free title is admitted");
    let agents = daemon.registry.agent_records().len();
    let refused = start_role(&client, title_role("desktop-2", title, &source)).await;
    assert_title_refusal(refused, title);
    assert_eq!(
        daemon.registry.agent_records().len(),
        agents,
        "a source-directory start refused for the title a cloned start holds must spawn nothing"
    );

    // Two cloned starts of one project: each has a clone of its own, and the
    // project is what they share.
    let title = "proj-orchestrator-3";
    start_role_from(
        &client,
        title_role("deck-3a", title, &clones[2]),
        Some(&source),
    )
    .await
    .expect("a free title is admitted");
    let refused = start_role_from(
        &client,
        title_role("deck-3b", title, &clones[3]),
        Some(&source),
    )
    .await;
    assert_title_refusal(refused, title);

    // Controls. The same title from a cloned start of ANOTHER project is another
    // key, and a start that names the first clone's own directory but no origin
    // is keyed by that directory, as it was before the origin was read.
    let title = "proj-orchestrator-4";
    start_role_from(
        &client,
        title_role("deck-4", title, &clones[4]),
        Some(&source),
    )
    .await
    .expect("a free title is admitted");
    start_role_from(
        &client,
        title_role("other-deck-4", title, &other_clone),
        Some(&other_source),
    )
    .await
    .expect("the same title from a cloned start of another project is another key");
    start_role(&client, title_role("no-origin-4", title, &clones[4]))
        .await
        .expect("a start that names no origin keeps keying on its own orchestration cwd");
}

/// The spawn primitive reports failures through a notifier; nothing here is
/// expected to fail, and the `expect` on its result says why if it does.
struct QuietNotifier;
impl dot_agent_deck::scheduler::Notifier for QuietNotifier {
    fn notify(&self, event: dot_agent_deck::scheduler::NotifyEvent) {
        eprintln!("[spawn notifier] {event:?}");
    }
}
