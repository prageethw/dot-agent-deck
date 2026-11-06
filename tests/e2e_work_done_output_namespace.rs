#![cfg(all(feature = "e2e", unix))]

//! upstream #331 / fork #76 — what happens to a worker's own file when it is the
//! `--task-file` of a delegated task's `work-done`, and sits where the daemon
//! files its own report (`.dot-agent-deck/work-done-<role>.md`).
//!
//! These tests used to pin a CLIENT-SIDE refusal: the CLI refused any
//! `--task-file` that resolved into that namespace. That guard is retired
//! (upstream-sync round 36, CLAUDE.md rule 24 case 3, on an independent audit);
//! the daemon's own mechanism protects the file, and does it better, because it
//! decides by the CONTENT of the destination, read on the same handle it then
//! writes, never by how the `--task-file` path was spelled. A task-scoped report
//! writes the role file only when it is absent, empty, or one the deck itself
//! wrote (this process's recorded fingerprint, else the deck's frame on both
//! ends). Any other file there is kept byte for byte, mode and all, and the report
//! is saved whole inside the deck's frame under a fresh, never-reused
//! `full-report-work-done-<role>-*.md` name the orchestrator is told about. A
//! symlink AT the role path is a different mechanism: the deck opens it with
//! `O_NOFOLLOW`, refuses, writes nothing, and the report is inlined in the
//! orchestrator's message with no file named.
//!
//! The old tests passed no `--task-id`. A `work-done` without one is an
//! uncorrelated note, which never writes the role file at all, so they never
//! modelled the hazard; these do (delegate a task, report it by id), and the
//! daemon side is observed end to end: the real CLI, the real daemon, the
//! orchestrator's inbox.
//!
//! Lane 1 (`cargo test-e2e`), not the fast tier the old tests were in: the task
//! id comes from a real `delegate`, and the inbox fixture that makes one from the
//! test process (`tests/support/inbox_route.rs`) needs the `e2e`-only
//! unbound-components seam. What the fast tier keeps of the same guarantees is
//! the daemon-side pins in `src/state.rs`
//! (`a_task_file_that_is_a_link_to_the_role_file_leaves_both_alone`,
//! `a_symlink_at_the_summary_path_is_refused_whatever_it_points_at`,
//! `a_report_whose_role_path_is_a_symlink_is_delivered_inline`,
//! `a_symlinked_context_directory_is_never_written_through_by_a_report`) and
//! `work_done_013` in `tests/work_done_reporting.rs`.

mod common;
#[path = "support/inbox_doorbell.rs"]
mod inbox;
#[path = "support/inbox_route.rs"]
mod route;

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use inbox::Endpoint;
use spec::spec;

/// The frame the deck puts round a saved report. Spelled out here, not imported,
/// so a silent rewording of the daemon's template fails these tests instead of
/// being followed by them.
const FRAME_OPEN: &str = "[UNTRUSTED-WORKER-REPORT:\n";
const FRAME_CLOSE: &str = "\n:END-UNTRUSTED-WORKER-REPORT]\n";

/// How the daemon names a report it saves beside a role file it kept.
const COPY_PREFIX: &str = "full-report-work-done-coder-";

/// Reports. Each starts with a marker line (the text before its first colon) that
/// is unique to it and survives JSON escaping, so a test can find it in what the
/// orchestrator's inbox hands over.
const WORKER_REPORT: &str =
    "NS-WORKER-REPORT-4e2a: the worker's own review\n- finding one\n- finding two";
const OWN_REPORT: &str = "NS-OWN-REPORT-17b0: a report the role path is linked to\n- one finding";
const DECOY_REPORT: &str = "NS-DECOY-REPORT-c3d1: an unrelated file that is merely named like one";
const OUTSIDE_REPORT_1: &str = "NS-OUTSIDE-REPORT-1-91c7: a report in a plain file";
const OUTSIDE_REPORT_2: &str = "NS-OUTSIDE-REPORT-2-5be8: the next report from the same role";

/// The marker line of one of the reports above.
fn mark(report: &str) -> &str {
    report.split(':').next().expect("a report has a marker")
}

/// What one `work-done --task-id` did, as far as the deck's files and the
/// orchestrator's inbox show it.
struct Reported {
    /// The `full-report-work-done-coder-*.md` files this report added.
    new_copies: Vec<PathBuf>,
    /// Everything the orchestrator's inbox handed over until the report arrived.
    delivered: String,
}

/// A real daemon holding one orchestration, its `orchestrator` and `coder` panes
/// running the routing stand-in, in a project directory the daemon files its
/// reports under.
struct Scene {
    // Panes first: dropping one stops its agent through the daemon, which
    // therefore has to outlive them.
    orch: inbox::Pane,
    worker: inbox::Pane,
    daemon: common::DaemonProc,
}

impl Scene {
    fn new() -> Self {
        let daemon = inbox::daemon_unbound(&[]);
        let (orch, worker) = route::team(&daemon, false);
        Self {
            orch,
            worker,
            daemon,
        }
    }

    /// The directory the worker was started in: where the daemon files a report,
    /// whatever directory the CLI is run from.
    fn project(&self) -> &Path {
        self.daemon.cwd()
    }

    fn context(&self) -> PathBuf {
        self.project().join(".dot-agent-deck")
    }

    /// Where the deck files a commissioned report of the `coder` role.
    fn role_file(&self) -> PathBuf {
        self.context().join("work-done-coder.md")
    }

    /// The orchestrator delegates a task to the worker, which fetches it. Answers
    /// the task id the worker's `work-done --task-id` names.
    fn assign(&self, label: &str) -> String {
        let task = format!("NS-TASK-{label}: send your report with work-done");
        let id = route::delegate(&self.daemon, &self.orch, &task, false);
        route::fetched(&self.daemon, &self.worker, &id, &task, "the orchestrator");
        id
    }

    /// The reports the deck has saved beside a role file it kept, by name.
    fn copies(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(self.context())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(COPY_PREFIX) && name.ends_with(".md"))
            })
            .collect();
        found.sort();
        found
    }

    /// The worker runs the real `work-done --task-id <task_id> --task-file
    /// <task_file>` from `cwd`, as its agent would (its identity and capability in
    /// the environment). It must be accepted; answers what it left behind.
    fn submit(&self, task_id: &str, cwd: &Path, task_file: &str, report: &str) -> Reported {
        let before = self.copies();
        let out = route::cli_output(
            &self.daemon,
            &self.worker,
            cwd,
            &["work-done", "--task-id", task_id, "--task-file", task_file],
        );
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        );
        assert!(
            out.status.success() && stdout.contains(task_id),
            "`work-done --task-id {task_id} --task-file {task_file}`, run from {}, must be \
             accepted and say which task it reported — the daemon decides what to do with a \
             file it did not write, the CLI no longer refuses one.\nstdout: {stdout:?}\n\
             stderr: {stderr:?}",
            cwd.display()
        );
        let new_copies = self
            .copies()
            .into_iter()
            .filter(|copy| !before.contains(copy))
            .collect();
        let delivered = self.delivered(mark(report));
        Reported {
            new_copies,
            delivered,
        }
    }

    /// Everything the orchestrator's inbox hands over until a message carrying
    /// `needle` has arrived. The test process stands in for the orchestrator's
    /// agent (the daemon runs with unbound components) and acknowledges what it
    /// takes.
    fn delivered(&self, needle: &str) -> String {
        let seen = RefCell::new(String::new());
        assert!(
            common::wait_until(common::load_scaled(inbox::WAIT), || {
                seen.borrow_mut()
                    .push_str(&inbox::fetch(&self.daemon, &self.orch, true).to_string());
                seen.borrow().contains(needle)
            }),
            "the orchestrator never received the report carrying {needle}; its inbox handed \
             over: {}",
            seen.borrow()
        );
        seen.into_inner()
    }

    /// Control: with the role path free, a report from a plain file outside the
    /// namespace is filed AT it by the deck, whole inside the frame, and saves no
    /// copy. The report's own file is left as it was.
    fn control_filed(&self, label: &str, cwd: &Path, file: &str, report: &str) {
        std::fs::write(cwd.join(file), report).expect("write the report's own file");
        let id = self.assign(label);
        let filed = self.submit(&id, cwd, file, report);
        assert!(
            filed.new_copies.is_empty(),
            "{label}: the role path was free, so the deck files the report there and saves no \
             copy; it saved {:?}",
            filed.new_copies
        );
        let text = std::fs::read_to_string(self.role_file())
            .expect("the deck files the report at the role path");
        assert!(
            text.starts_with(FRAME_OPEN) && text.ends_with(FRAME_CLOSE) && text.contains(report),
            "{label}: the role file must hold exactly the report inside the deck's frame, got \
             {text:?}"
        );
        assert!(
            !filed.delivered.contains(COPY_PREFIX),
            "{label}: a report filed at the role path points the orchestrator at no copy, got \
             {:?}",
            filed.delivered
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join(file)).expect("the report's own file"),
            report,
            "{label}: the worker's own file is left as it was"
        );
    }
}

/// Park a worker-authored `text` at `path` with mode 0640 — not the 0600 the deck
/// gives its own files, so a re-mode shows.
fn park(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent directory"))
        .expect("create the file's directory");
    std::fs::write(path, text).expect("write the worker's file");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))
        .expect("set the worker's file's mode");
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("stat the file")
        .permissions()
        .mode()
        & 0o777
}

/// The worker's file at `path` is exactly what it was: its bytes and its mode.
fn assert_kept(path: &Path, text: &str, what: &str) {
    assert_eq!(
        std::fs::read(path).expect("read the worker's file"),
        text.as_bytes(),
        "{what}: the worker's file must survive byte for byte"
    );
    assert_eq!(
        mode_of(path),
        0o640,
        "{what}: the worker's file must keep its mode (the deck's own are 0600)"
    );
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .expect("a file name")
        .to_string_lossy()
        .into_owned()
}

/// The deck kept the worker's file and saved the report whole, inside its frame,
/// as one fresh copy that the orchestrator's message names.
fn assert_diverted(reported: &Reported, report: &str) {
    assert_eq!(
        reported.new_copies.len(),
        1,
        "the role path held a file the deck did not write, so the report is saved as exactly \
         one new `{COPY_PREFIX}*.md` copy; added: {:?}",
        reported.new_copies
    );
    let copy = &reported.new_copies[0];
    let text = std::fs::read_to_string(copy).expect("read the saved copy");
    assert!(
        text.starts_with(FRAME_OPEN) && text.ends_with(FRAME_CLOSE) && text.contains(report),
        "the saved copy must hold the whole report inside the deck's frame, got {text:?}"
    );
    assert!(
        reported.delivered.contains(&file_name(copy)),
        "the orchestrator's message must name the saved copy {}, got {:?}",
        file_name(copy),
        reported.delivered
    );
}

/// The names of the files directly inside `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("read the directory")
        .filter_map(Result::ok)
        .map(|entry| file_name(&entry.path()))
        .collect();
    found.sort();
    found
}

/// Scenario: A worker that wrote its own report to `.dot-agent-deck/work-done-coder.md` hands that very file to the real `work-done --task-id <id> --task-file`: the CLI exits 0, the file survives byte for byte and mode for mode, the report is saved whole inside the deck's frame as a fresh `full-report-work-done-coder-*.md` copy, and the orchestrator's inbox delivers it naming that copy. Controls: once the worker removes its file, a report from a file outside the namespace is filed at the role path by the deck, and the next same-role report replaces that deck-written file.
#[spec("orchestration/delegate/063")]
#[test]
fn delegate_063_a_task_file_at_the_role_path_is_kept_and_its_report_diverted() {
    let scene = Scene::new();
    let role = scene.role_file();

    // The shape: the worker wrote its own report where the deck files its own,
    // and hands that very file to `work-done` as the task file.
    let id = scene.assign("063");
    park(&role, WORKER_REPORT);
    let diverted = scene.submit(
        &id,
        scene.project(),
        ".dot-agent-deck/work-done-coder.md",
        WORKER_REPORT,
    );
    assert_kept(&role, WORKER_REPORT, "a task file at the role path");
    assert_diverted(&diverted, WORKER_REPORT);

    // Control: the worker tidies its own file away, and a report from a file
    // outside the namespace is filed at the role path by the deck, as it always
    // was.
    std::fs::remove_file(&role).expect("remove the worker's file");
    scene.control_filed(
        "063-outside",
        scene.project(),
        "worker-report.md",
        OUTSIDE_REPORT_1,
    );

    // Control: that file is one the deck wrote itself, so the next same-role
    // report replaces it (this process recorded what it wrote there).
    scene.control_filed(
        "063-replaced",
        scene.project(),
        "worker-report-2.md",
        OUTSIDE_REPORT_2,
    );
    let text = std::fs::read_to_string(&role).expect("the role file");
    assert!(
        !text.contains(mark(OUTSIDE_REPORT_1)),
        "the deck's own earlier report is replaced by the next one, not kept beside it, got \
         {text:?}"
    );
}

/// Scenario: A worker hands `work-done --task-id <id> --task-file` a symlink outside the namespace whose target is its own role-path file: the CLI exits 0, the file and the link are untouched, and the report is saved as a fresh framed copy and delivered naming it. Then the mirror image, a different mechanism: with the role path itself a symlink to a worker report elsewhere, the deck refuses to write through it (`O_NOFOLLOW`), leaves the target untouched, saves no copy and names no file, and the orchestrator still receives the report inline. Control: with the role path free again, a plain file outside the namespace is filed there by the deck.
#[spec("orchestration/delegate/036")]
#[test]
fn delegate_036_a_symlinked_task_file_is_kept_and_a_symlinked_role_path_is_never_written_through() {
    let scene = Scene::new();
    let project = scene.project();
    let role = scene.role_file();

    // Shape one: `--task-file` is a symlink OUTSIDE the namespace (a name that
    // is nothing like `work-done-*.md`) whose target is the worker's own file at
    // the role path. The CLI reads through the link and sends that text; the
    // daemon never sees the link, and judges the role file by what it holds.
    let id = scene.assign("036-link");
    park(&role, WORKER_REPORT);
    let link = project.join("external-report.md");
    std::os::unix::fs::symlink(&role, &link).expect("link the task file to the role file");
    let diverted = scene.submit(&id, project, "external-report.md", WORKER_REPORT);
    assert_kept(&role, WORKER_REPORT, "the target of a symlinked task file");
    assert_eq!(
        std::fs::read_link(&link).expect("the task file is still a link"),
        role,
        "the supplied link is still the link it was"
    );
    assert_diverted(&diverted, WORKER_REPORT);

    // Shape two, the mirror image the old client-side guard never caught: the
    // ROLE PATH is the symlink, to a worker report outside the namespace. The
    // deck opens the role path with `O_NOFOLLOW`, so it refuses and the target is
    // never written through. This is not a divert: nothing is saved, no file is
    // named, and the report travels inline in the orchestrator's message.
    std::fs::remove_file(&link).expect("remove the task-file link");
    std::fs::remove_file(&role).expect("remove the worker's file");
    let own = project.join("notes/own-report.md");
    park(&own, OWN_REPORT);
    std::os::unix::fs::symlink(&own, &role).expect("link the role path to the worker's report");
    let id = scene.assign("036-at-role");
    let inlined = scene.submit(&id, project, "notes/own-report.md", OWN_REPORT);
    assert_kept(&own, OWN_REPORT, "the target of a symlinked role path");
    assert_eq!(
        std::fs::read_link(&role).expect("the role path is still a link"),
        own,
        "the role path is still the link it was"
    );
    assert!(
        inlined.new_copies.is_empty() && !inlined.delivered.contains(COPY_PREFIX),
        "a symlinked role path is refused, not diverted: no copy is saved and the orchestrator \
         is pointed at no file, got copies {:?} and message {:?}",
        inlined.new_copies,
        inlined.delivered
    );

    // Control: with the role path free again, a plain file outside the namespace
    // is filed there by the deck.
    std::fs::remove_file(&role).expect("remove the role-path link");
    scene.control_filed("036-outside", project, "worker-report.md", OUTSIDE_REPORT_1);
}

/// Scenario: A worker's own report sits at the role path and `work-done --task-id <id> --task-file` names it through `alias/`, a directory symlink to `.dot-agent-deck`: the CLI exits 0, the file and the alias are untouched, and the report is saved as a fresh framed copy the orchestrator's message names. Control: with the role path free, a plain file outside the namespace is filed there by the deck.
#[spec("orchestration/delegate/064")]
#[test]
fn delegate_064_a_task_file_reached_through_a_directory_alias_is_kept_and_its_report_diverted() {
    let scene = Scene::new();
    let project = scene.project();
    let role = scene.role_file();

    let id = scene.assign("064");
    park(&role, WORKER_REPORT);
    // An intermediate directory symlink that aliases straight into
    // `.dot-agent-deck`: the argument's own parent segment is `alias`.
    let alias = project.join("alias");
    std::os::unix::fs::symlink(scene.context(), &alias)
        .expect("alias the context directory by a symlink");
    let diverted = scene.submit(&id, project, "alias/work-done-coder.md", WORKER_REPORT);
    assert_kept(&role, WORKER_REPORT, "a task file reached through an alias");
    assert_eq!(
        std::fs::read_link(&alias).expect("the alias is still a link"),
        scene.context(),
        "the alias is still the link it was"
    );
    assert_diverted(&diverted, WORKER_REPORT);

    // Control: with the role path free, a plain file outside the namespace is
    // filed there by the deck.
    std::fs::remove_file(&role).expect("remove the worker's file");
    scene.control_filed("064-outside", project, "worker-report.md", OUTSIDE_REPORT_1);
}

/// Scenario: The worker runs `work-done --task-id <id> --task-file` from a child directory that has a `.dot-agent-deck` of its own, naming the role file by absolute path, by a `../` path, and naming a decoy `work-done-decoy.md` there: each exits 0, the worker's role file and the decoy are untouched, and each report is saved as a fresh framed copy under the directory the worker was started in. The deck never writes under the CLI's own directory, and with the role path free a plain file outside the namespace is filed at the spawn directory's role path.
#[spec("orchestration/delegate/065")]
#[test]
fn delegate_065_a_task_file_named_from_another_directory_is_kept_and_filed_under_the_spawn_directory()
 {
    let scene = Scene::new();
    let project = scene.project();
    let role = scene.role_file();

    // The CLI's own cwd: a child directory the worker `cd`d into, with a second,
    // unrelated `.dot-agent-deck` of its own and a decoy report in it. The pane's
    // real role file and the CLI's cwd disagree.
    let child = project.join("subdir");
    let child_context = child.join(".dot-agent-deck");
    let decoy = child_context.join("work-done-decoy.md");
    park(&decoy, DECOY_REPORT);
    assert_eq!(names(&child_context), ["work-done-decoy.md"]);

    // 1) The role file by ABSOLUTE path, from the child.
    let id = scene.assign("065-absolute");
    park(&role, WORKER_REPORT);
    let absolute = role.to_str().expect("a utf-8 path").to_owned();
    let first = scene.submit(&id, &child, &absolute, WORKER_REPORT);
    assert_kept(&role, WORKER_REPORT, "the role file named by absolute path");
    assert_diverted(&first, WORKER_REPORT);

    // 2) The same file by a RELATIVE `../` path from the child.
    let id = scene.assign("065-relative");
    let second = scene.submit(
        &id,
        &child,
        "../.dot-agent-deck/work-done-coder.md",
        WORKER_REPORT,
    );
    assert_kept(&role, WORKER_REPORT, "the role file named by a `../` path");
    assert_diverted(&second, WORKER_REPORT);
    assert_ne!(
        first.new_copies, second.new_copies,
        "each report gets a copy of its own, under a name never reused"
    );

    // 3) The decoy under the CLI's own `.dot-agent-deck`: a file named like a
    // report that is not the pane's role file at all. It is only a task file; the
    // pane's role path still holds the worker's file, so this report is diverted
    // too, and neither file is written.
    let id = scene.assign("065-decoy");
    let third = scene.submit(
        &id,
        &child,
        ".dot-agent-deck/work-done-decoy.md",
        DECOY_REPORT,
    );
    assert_kept(
        &role,
        WORKER_REPORT,
        "the pane's role file, with a decoy named",
    );
    assert_kept(&decoy, DECOY_REPORT, "the decoy");
    assert_diverted(&third, DECOY_REPORT);

    // The deck wrote nothing under the CLI's own directory: every copy is under
    // the directory the worker was started in, which is where the daemon files.
    assert_eq!(
        names(&child_context),
        ["work-done-decoy.md"],
        "the deck files under the directory the worker was started in, never the CLI's cwd"
    );

    // Control: with the role path free, a plain file outside any
    // `.dot-agent-deck`, named from the child, is filed at the SPAWN directory's
    // role path, and still not under the child.
    std::fs::remove_file(&role).expect("remove the worker's file");
    scene.control_filed("065-outside", &child, "worker-report.md", OUTSIDE_REPORT_1);
    assert_eq!(
        names(&child_context),
        ["work-done-decoy.md"],
        "the filed report is at the spawn directory's role path, not under the child"
    );
}
