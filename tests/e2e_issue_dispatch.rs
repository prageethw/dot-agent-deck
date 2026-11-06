#![cfg(feature = "e2e")]

//! L2 GitHub-dispatch tests for the daemon-hosted scheduler (PRD #120).
//!
//! These drive the same headless `dot-agent-deck daemon serve` harness as the
//! `scheduler/spawn/*` family (run-now over the attach socket, no PTY), but
//! exercise the NEW `issue_dispatch` task type: on fire the daemon must
//! enumerate a repo's open issues via `gh`, provision the repo (clone), create a
//! per-issue worktree on `agent/issue-<n>`, dedup already-claimed issues, and
//! spawn one agent per remaining issue into its worktree — reusing #127's spawn
//! primitive (orchestration tab vs single-agent card).
//!
//! Everything runs OFFLINE behind the TESTABILITY SEAM: a stub `gh` script on
//! PATH branches on argv (`issue list` → canned JSON, `pr list` → canned JSON,
//! `repo clone` → `git clone` of a LOCAL fixture remote). The fixture remote is
//! a real one-commit git repo (optionally carrying a committed
//! `.dot-agent-deck.toml` so the dispatched worktree opens an orchestration tab).
//!
//! Implemented now (dispatch/001-009 all GREEN): firing an `issue_dispatch` task
//! runs the full GitHub-dispatch flow — the daemon clones the repo, derives a
//! per-issue worktree on `agent/issue-<n>`, dedups issues that already carry an
//! open PR or a live worktree, and spawns one agent per remaining issue into its
//! worktree. Each test asserts that observable end-state — clone present,
//! worktree created, per-issue agent(s) rooted in it — across the seam above.
//!
//! PRD #120 FINAL DECISION: the `experimental` flag gates only the CREATION UX
//! (the new-pane `schedule: issues` option and the issue-dispatch authoring
//! form), NOT the dispatch BEHAVIOR. A configured `issue_dispatch` task fires
//! regardless of the flag, so these daemon-spawn envs carry no
//! `DOT_AGENT_DECK_EXPERIMENTAL`. (The former `dispatch/010`, which asserted the
//! flow stayed inert with the flag off, was removed when the behavior gate went
//! away.) Flag-gated creation UX is covered by the `scheduler/cli`,
//! `new-pane/issue-dispatch`, and `scheduler/manager` catalog families.

mod common;

#[path = "support/inbox_view.rs"]
mod view;

use std::path::{Path, PathBuf};
use std::time::Duration;

use dot_agent_deck::agent_pty::{AgentRecord, TabMembership};
use dot_agent_deck::daemon_protocol::AttachRequest;
use dot_agent_deck::issue_dispatch::{
    IN_PROGRESS_LABEL, Identity, claim_comment_body, derive_issue_paths,
};
use spec::spec;

const CLAIM_MARKER: &str = "<!-- dot-agent-deck:claim -->";

// ---------------------------------------------------------------------------
// Stub `gh` + local fixture-remote harness (the TESTABILITY SEAM)
// ---------------------------------------------------------------------------

/// Offline GitHub seam: argv is logged before validation, writes persist repo
/// state, and unknown invocations fail. A missing label fails issue-edit just
/// as gh's label-name resolution does; `read-only` denies each write verb.
const GH_STUB_SCRIPT: &str = r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from email.utils import format_datetime

args = sys.argv[1:]
root = Path(os.environ['GHSTUB_DIR'])

# Two claim systems share this stub (sync ruling 36): UPSTREAM's
# `agent-deck/claimed` label plus claim record, and the FORK's `in-progress`
# label, `Claimed by` comment and assignee. Each family's tests assert on the
# exact calls THEIR system makes, so each log omits the other family's calls:
# `calls.jsonl` (`GhStub::calls`) leaves out the fork's, `gh-calls.log`
# (`GhStub::gh_calls`) leaves out upstream's claim calls. Reads both systems
# share (`issue list`, `pr list`, `repo clone`, `issue view`) are in both.
def is_fork_claim_call(a):
    return ((a[:2] == ['api', 'user'] and '--jq' in a)
            or (a[:2] == ['label', 'create'] and len(a) > 2 and a[2] != 'agent-deck/claimed')
            or a[:2] == ['label', 'list']
            or (a[:2] == ['issue', 'edit'] and 'agent-deck/claimed' not in a)
            or a[:2] == ['issue', 'comment'])

def is_upstream_claim_call(a):
    return (a in (['api', 'user'], ['api', '--include', 'user'])
            or a[:3] == ['label', 'create', 'agent-deck/claimed']
            or (a[:2] == ['issue', 'edit'] and 'agent-deck/claimed' in a)
            or (a[:1] == ['api'] and '--jq' not in a))

if not is_fork_claim_call(args):
    with (root / 'calls.jsonl').open('a') as log:
        log.write(json.dumps(args) + '\n')
if not is_upstream_claim_call(args):
    with (root / 'gh-calls.log').open('a') as log:
        log.write(' '.join(args) + '\n')

def fail(message):
    print(message, file=sys.stderr)
    sys.exit(1)

def option(name):
    return args[args.index(name) + 1]

def emit(value):
    print(json.dumps(value))
    sys.exit(0)

def hang_if_enabled(paths):
    # A file-controlled long call, without a child sleep process to leak.
    deadline = time.monotonic() + 120
    while any(path.exists() for path in paths) and time.monotonic() < deadline:
        time.sleep(0.05)

def fail_first(directory, kind):
    remaining_file = directory / ('fail-first-' + kind)
    if remaining_file.exists():
        remaining = int(remaining_file.read_text())
        if remaining > 0:
            remaining_file.write_text(str(remaining - 1))
            fail('HTTP 502: transient GitHub failure (' + kind + ')')

# ---------------------------------------------------------------------------
# FORK handlers (PRD #421 / fork#235): the `in-progress` claim system (label,
# `Claimed by` comment, assignee), kept beside upstream's `agent-deck/claimed`
# record below so ONE stub serves both families of tests (sync ruling 36). The
# fork's former shell stub is ported here handler for handler; the argv shapes
# below are the fork's, upstream's own shapes fall through to its handlers.
# ---------------------------------------------------------------------------
def fork_flag(name):
    return args[args.index(name) + 1] if name in args[:-1] else ''

def fork_issue_number():
    # The first bare number after `<group> <sub>` that is not a flag's value.
    skip = False
    for a in args[2:]:
        if skip:
            skip = False
            continue
        if a.startswith('--') and a != '--':
            skip = True
            continue
        if a[:1].isdigit():
            return a
    return ''

fork_key = fork_flag('--repo').replace('/', '_')

# `gh api user --jq .login` (PRD fork#235 M2, `gh_current_login_argv`):
# `GhStub::fail_api_user` arms the failure path, `GhStub::set_login` the canned
# reply; `gh-stub-user` when unset.
if args[:2] == ['api', 'user'] and '--jq' in args:
    if (root / 'fail-api-user').exists():
        fail('gh: HTTP 401: Bad credentials (stubbed failure)')
    login_file = root / 'login'
    sys.stdout.write(login_file.read_text() if login_file.exists() else 'gh-stub-user\n')
    sys.exit(0)

# `gh label create <name> ...` is idempotent (real gh passes `--force`) and adds
# `<name>` to the repo's known label set in `<repo>/labels` (one per line), so a
# create-then-add sequence succeeds against the tightened `issue edit` below.
# `GhStub::seed_labels` pre-populates the same file.
if args[:2] == ['label', 'create'] and len(args) > 2 and args[2] != 'agent-deck/claimed':
    if args[2] and fork_key:
        (root / fork_key).mkdir(parents=True, exist_ok=True)
        labels_file = root / fork_key / 'labels'
        known = labels_file.read_text().splitlines() if labels_file.exists() else []
        if args[2] not in known:
            with labels_file.open('a') as handle:
                handle.write(args[2] + '\n')
    sys.exit(0)

# `gh label list`: the CURRENT known label set, not an unconditional `[]`.
if args[:2] == ['label', 'list']:
    labels_file = root / fork_key / 'labels'
    names = [n for n in labels_file.read_text().splitlines() if n] if labels_file.exists() else []
    emit([{'name': n} for n in names])

# `gh issue edit`: `--add-label <name>` hard-errors BEFORE any mutation when the
# repo does not already carry that label (cli/cli `LabelsToIDs`), and records
# the applied label in `<repo>/applied-<n>` on success (`GhStub::label_applied`).
# Assignee writes (`--add-assignee`/`--remove-assignee`, replace-to-one) are
# always accepted, mirroring real gh dropping an assignee lacking repo access.
if args[:2] == ['issue', 'edit'] and 'agent-deck/claimed' not in args:
    issue_num = fork_issue_number()
    add_label = fork_flag('--add-label')
    if add_label:
        labels_file = root / fork_key / 'labels'
        known = labels_file.read_text().splitlines() if labels_file.exists() else []
        if add_label in known:
            with (root / fork_key / ('applied-' + issue_num)).open('a') as handle:
                handle.write(add_label + '\n')
            sys.exit(0)
        fail("gh: GraphQL: Could not resolve to a Label with the name '" + add_label + "'. (addLabelsToLabelable)")
    assignees_file = root / fork_key / ('assignees-' + issue_num)
    remove_assignee = fork_flag('--remove-assignee')
    add_assignee = fork_flag('--add-assignee')
    if remove_assignee and assignees_file.exists():
        kept = [n for n in assignees_file.read_text().splitlines() if n != remove_assignee]
        assignees_file.write_text(''.join(n + '\n' for n in kept))
    if add_assignee:
        known = assignees_file.read_text().splitlines() if assignees_file.exists() else []
        if add_assignee not in known:
            with assignees_file.open('a') as handle:
                handle.write(add_assignee + '\n')
    sys.exit(0)

# `gh issue comment`: recorded above, accepted unconditionally.
if args[:2] == ['issue', 'comment']:
    sys.exit(0)

# `gh issue view`: a per-issue fixture (`<repo>/issue-<n>.json`) when present,
# else the fork's default shape; the labels come from upstream's persisted state.
if args[:2] == ['issue', 'view']:
    issue_num = fork_issue_number()
    fixture = root / fork_key / ('issue-' + issue_num + '.json')
    if fixture.exists():
        sys.stdout.write(fixture.read_text())
        sys.exit(0)
    state_file = root / fork_key / 'state.json'
    held = json.loads(state_file.read_text())['issue_labels'].get(issue_num, []) if state_file.exists() else []
    emit({'number': int(issue_num or 0), 'labels': [{'name': n} for n in held], 'comments': []})

if args in (['api', 'user'], ['api', '--include', 'user']):
    hang_if_enabled([root / 'hang-claim', root / 'hang-user'])
    fail_first(root, 'user')
    user = json.loads((root / 'authenticated-user.json').read_text())
    if '--include' in args:
        sys.stdout.write('HTTP/2.0 200 OK\nContent-Type: application/json\r\nDate: '
                         + format_datetime(datetime.now(timezone.utc), usegmt=True)
                         + '\r\n\r\n')
    emit(user)

if args[:2] == ['repo', 'clone'] and len(args) == 4:
    remote = root / args[2].replace('/', '_') / 'remote'
    sys.exit(subprocess.call(['git', 'clone', '--quiet', str(remote), args[3]]))

if args[:2] in (['issue', 'list'], ['pr', 'list']):
    rd = root / option('--repo').replace('/', '_')
    if args[:2] == ['issue', 'list']:
        emit(json.loads((rd / 'issues.json').read_text()))
    n = option('--head').rsplit('-', 1)[-1]
    if (rd / ('fail-pr-' + n)).exists():
        fail('gh: simulated API error for issue ' + n + ' (agent/issue-' + n + ')')
    data = rd / ('pr-' + n + '.json')
    emit(json.loads(data.read_text()) if data.exists() else [])

# Claim reads return the same persisted label, events and comments that writes
# update. Keep these offline even when the production reader changes endpoint.
if args[:2] == ['issue', 'view']:
    rd = root / option('--repo').replace('/', '_')
    state = json.loads((rd / 'state.json').read_text())
    issue = next(a for a in args[2:] if a.isdigit())
    emit({'labels': [{'name': name} for name in state['issue_labels'].get(issue, [])]})
if args and args[0] == 'api' and '--method' not in args and '-X' not in args:
    endpoint = next((a for a in args[1:] if a.startswith('repos/')), '')
    match = re.fullmatch(r'repos/([^/]+/[^/]+)/issues/(\d+)(?:/(labels|events|timeline))?(?:\?per_page=100)?', endpoint)
    if match:
        repo, issue, resource = match.groups()
        rd = root / repo.replace('/', '_')
        state = json.loads((rd / 'state.json').read_text())
        labels = [{'name': name} for name in state['issue_labels'].get(issue, [])]
        if resource in ('events', 'timeline'):
            hang_if_enabled([root / 'hang-claim', rd / 'hang-events'])
            fail_first(rd, 'events')
            emit(state.get('events', {}).get(issue, []))
        emit(labels if resource == 'labels' else {'labels': labels})

kind = None
if (len(args) == 10 and args[:3] == ['label', 'create', 'agent-deck/claimed']
        and args[3] == '--repo' and args[5:7] == ['--force', '--color']
        and re.fullmatch('[0-9a-fA-F]{6}', args[7]) and args[8] == '--description'):
    repo, kind = args[4], 'label'
elif (len(args) == 8 and args[:3] == ['issue', 'edit', '--repo']
        and args[4:7] == ['--add-label', 'agent-deck/claimed', '--']
        and args[7].isdigit()):
    repo, issue, kind = args[3], args[7], 'issue'
elif args[:2] == ['api', '--paginate'] and len(args) == 3:
    match = re.fullmatch(r'repos/([^/]+/[^/]+)/issues/(\d+)/comments(?:\?per_page=100)?', args[2])
    if match:
        repo, issue = match.groups()
        kind = 'list'
elif (len(args) == 6 and args[:2] == ['api', '--method']
        and args[4] == '-f' and args[5].startswith('body=')):
    if args[2] == 'POST':
        match = re.fullmatch(r'repos/([^/]+/[^/]+)/issues/(\d+)/comments', args[3])
        if match:
            repo, issue = match.groups()
            kind = 'POST'
    elif args[2] == 'PATCH':
        match = re.fullmatch(r'repos/([^/]+/[^/]+)/issues/comments/(\d+)', args[3])
        if match:
            repo, comment_id = match.groups()
            kind = 'PATCH'
if kind is None:
    with (root / 'unhandled.jsonl').open('a') as log:
        log.write(json.dumps(args) + '\n')
    fail('gh stub: unhandled invocation: ' + repr(args))

rd = root / repo.replace('/', '_')
hang_paths = [root / 'hang-claim', rd / 'hang', rd / ('hang-' + kind)]
if kind in ('issue', 'list', 'POST'):
    hang_paths += [rd / ('hang-issue-' + issue), rd / ('hang-issue-' + issue + '-' + kind)]
hang_if_enabled(hang_paths)
fail_first(rd, kind)
state_file = rd / 'state.json'
state = json.loads(state_file.read_text())
if kind == 'list':
    emit(state['comments'].get(issue, []))
if (rd / 'read-only').exists():
    fail('HTTP 403: Resource not accessible by integration (' + kind + ')')
if kind == 'label':
    if (rd / 'fail-provision').exists():
        fail('HTTP 403: token may triage issues but cannot create labels')
    state['labels'] = ['agent-deck/claimed']
elif kind == 'issue':
    if 'agent-deck/claimed' not in state['labels']:
        fail("could not add label: 'agent-deck/claimed' not found")
    if 'agent-deck/claimed' not in state['issue_labels'].get(issue, []):
        state.setdefault('events', {}).setdefault(issue, []).append({
            'event': 'labeled', 'label': {'name': 'agent-deck/claimed'},
            'created_at': datetime.now(timezone.utc).isoformat()})
    state['issue_labels'][issue] = ['agent-deck/claimed']
elif kind == 'POST':
    comment = {'id': state['next_id'], 'body': args[5][5:],
               'user': json.loads((root / 'authenticated-user.json').read_text()),
               'author_association': 'OWNER',
               'created_at': datetime.now(timezone.utc).isoformat(),
               'updated_at': datetime.now(timezone.utc).isoformat()}
    state['next_id'] += 1
    state['comments'].setdefault(issue, []).append(comment)
elif kind == 'PATCH':
    found = [c for comments in state['comments'].values() for c in comments
             if c['id'] == int(comment_id)]
    if len(found) != 1:
        fail('HTTP 404: Not Found')
    # GitHub leaves updated_at unchanged when an edit repeats the same body.
    if found[0]['body'] != args[5][5:]:
        found[0]['body'] = args[5][5:]
        found[0]['updated_at'] = datetime.now(timezone.utc).isoformat()
temporary = state_file.with_suffix('.tmp')
temporary.write_text(json.dumps(state))
temporary.replace(state_file)
with (root / 'writes.jsonl').open('a') as log:
    log.write(json.dumps(args) + '\n')
emit(comment if kind == 'POST' else found[0] if kind == 'PATCH' else {})
"#;

/// A committed `.dot-agent-deck.toml` whose single-role orchestration runs `cat`
/// (which echoes the delivered prompt) — mirrors `scheduler/spawn/002`'s
/// orchestration fixture so a dispatched worktree opens an orchestration tab.
const ORCH_TOML: &str = "[[orchestrations]]\nname = \"dispatch-orch\"\n\n\
     [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n";

/// A committed `.dot-agent-deck.toml` whose orchestration has TWO roles
/// (`orchestrator` + `reviewer`, both `cat`). A dispatched worktree opens an
/// orchestration tab with two role panes that share ONE `orchestration_cwd` (the
/// issue worktree) — the fixture for `scheduler/dispatch/009`'s refcount check.
const MULTIROLE_ORCH_TOML: &str = "[[orchestrations]]\nname = \"dispatch-orch\"\n\n\
     [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
     [[orchestrations.roles]]\nname = \"reviewer\"\ncommand = \"cat\"\n";

/// Holds the stub `gh` + the per-repo fixture data dir, both rooted in a scratch
/// tempdir kept alive for the test's lifetime.
struct GhStub {
    _scratch: tempfile::TempDir,
    /// `$GHSTUB_DIR` — per-repo canned data lives under here.
    dir: PathBuf,
    /// Dir holding the `gh` script; prepended to the daemon's PATH.
    bindir: PathBuf,
}

impl GhStub {
    fn new() -> Self {
        let scratch = common::harness_tempdir().expect("gh stub scratch tempdir");
        let base = scratch.path().to_path_buf();
        let dir = base.join("ghstub");
        let bindir = base.join("bin");
        std::fs::create_dir_all(&dir).expect("create ghstub data dir");
        std::fs::create_dir_all(&bindir).expect("create ghstub bin dir");
        std::fs::write(
            dir.join("authenticated-user.json"),
            r#"{"id":12345,"login":"deck-bot"}"#,
        )
        .expect("seed authenticated GitHub account");
        let gh = bindir.join("gh");
        std::fs::write(&gh, GH_STUB_SCRIPT).expect("write gh stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))
                .expect("chmod gh stub");
        }
        GhStub {
            _scratch: scratch,
            dir,
            bindir,
        }
    }

    fn key(repo: &str) -> String {
        repo.replace('/', "_")
    }

    fn repo_dir(&self, repo: &str) -> PathBuf {
        self.dir.join(Self::key(repo))
    }

    /// Create the fixture remote for `repo` (a real one-commit git repo). When
    /// `with_orchestration`, the commit carries `.dot-agent-deck.toml`.
    fn add_repo(&self, repo: &str, with_orchestration: bool) {
        let rd = self.repo_dir(repo);
        std::fs::create_dir_all(&rd).expect("create repo fixture dir");
        init_remote(&rd.join("remote"), with_orchestration);
        self.seed_state(repo);
    }

    /// Like [`add_repo`] but the committed `.dot-agent-deck.toml` defines a
    /// TWO-role orchestration (orchestrator + reviewer, both `cat`), so a
    /// dispatched worktree opens an orchestration tab with two role panes that
    /// share the SAME issue worktree as their `orchestration_cwd`.
    fn add_repo_multirole(&self, repo: &str) {
        let rd = self.repo_dir(repo);
        std::fs::create_dir_all(&rd).expect("create repo fixture dir");
        init_remote_with_orch_toml(&rd.join("remote"), Some(MULTIROLE_ORCH_TOML));
        self.seed_state(repo);
    }

    fn seed_state(&self, repo: &str) {
        std::fs::write(
            self.repo_dir(repo).join("state.json"),
            r#"{"labels":[],"issue_labels":{},"comments":{},"next_id":101}"#,
        )
        .expect("seed repo without labels or claim comments");
    }

    fn read_only(&self, repo: &str) {
        std::fs::write(self.repo_dir(repo).join("read-only"), "").expect("deny GitHub writes");
    }

    fn set_authenticated_user(&self, id: u64, login: &str) {
        std::fs::write(
            self.dir.join("authenticated-user.json"),
            serde_json::json!({"id": id, "login": login}).to_string(),
        )
        .expect("configure authenticated GitHub account");
    }

    fn seed_comment(&self, repo: &str, issue: u64, mut comment: serde_json::Value) -> u64 {
        let mut state = self.state(repo);
        let id = state["next_id"].as_u64().unwrap();
        comment["id"] = id.into();
        state["next_id"] = (id + 1).into();
        let comments = &mut state["comments"][issue.to_string()];
        if !comments.is_array() {
            *comments = serde_json::json!([]);
        }
        comments.as_array_mut().unwrap().push(comment);
        std::fs::write(self.repo_dir(repo).join("state.json"), state.to_string())
            .expect("seed foreign claim comment");
        id
    }

    fn deny_provision_existing_label(&self, repo: &str) {
        let mut state = self.state(repo);
        state["labels"] = serde_json::json!(["agent-deck/claimed"]);
        std::fs::write(self.repo_dir(repo).join("state.json"), state.to_string())
            .expect("seed existing label");
        std::fs::write(self.repo_dir(repo).join("fail-provision"), "")
            .expect("deny label provisioning but allow issue triage");
    }

    fn hang_issue_label(&self, repo: &str, issue: u64) {
        std::fs::write(
            self.repo_dir(repo)
                .join(format!("hang-issue-{issue}-issue")),
            "",
        )
        .expect("hang only this issue's label write");
    }

    fn hang_claim_calls(&self) {
        std::fs::write(self.dir.join("hang-claim"), "").expect("hang every claim-path gh call");
    }

    fn fail_first_calls(&self, repo: &str, kind: &str, count: usize) {
        let directory = if kind == "user" {
            self.dir.clone()
        } else {
            self.repo_dir(repo)
        };
        std::fs::write(
            directory.join(format!("fail-first-{kind}")),
            count.to_string(),
        )
        .expect("fail the first N calls of this gh kind");
    }

    fn state(&self, repo: &str) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(self.repo_dir(repo).join("state.json")).expect("read gh state"),
        )
        .expect("valid gh state")
    }

    fn seed_claim(
        &self,
        repo: &str,
        issue: u64,
        author: u64,
        task: &str,
        host: &str,
        age_hours: i64,
    ) {
        let time = chrono::Utc::now() - chrono::Duration::hours(age_hours);
        self.seed_comment(
            repo,
            issue,
            serde_json::json!({
                "body": dot_agent_deck::issue_dispatch::render_claim_record(task, host, time),
                "user": {"id": author, "login": format!("claimant-{author}")},
                "author_association": "COLLABORATOR",
                "created_at": time.to_rfc3339(), "updated_at": time.to_rfc3339()
            }),
        );
        let mut state = self.state(repo);
        state["labels"] = serde_json::json!(["agent-deck/claimed"]);
        state["issue_labels"][issue.to_string()] = serde_json::json!(["agent-deck/claimed"]);
        if state["events"].is_null() {
            state["events"] = serde_json::json!({});
        }
        state["events"][issue.to_string()] = serde_json::json!([{
            "event": "labeled", "label": {"name": "agent-deck/claimed"},
            "created_at": time.to_rfc3339()
        }]);
        std::fs::write(self.repo_dir(repo).join("state.json"), state.to_string()).unwrap();
    }

    /// Add the writer's lifetime to a claim already seeded in the legacy format.
    fn seed_claim_lifetime(&self, repo: &str, issue: u64, lifetime_hours: u64) {
        let mut state = self.state(repo);
        let body = state["comments"][issue.to_string()][0]["body"]
            .as_str()
            .expect("seeded claim body");
        state["comments"][issue.to_string()][0]["body"] =
            format!("{body}- Lifetime (hours): {lifetime_hours}\n").into();
        std::fs::write(self.repo_dir(repo).join("state.json"), state.to_string())
            .expect("seed the claim writer's lifetime");
    }

    fn writes(&self) -> String {
        std::fs::read_to_string(self.dir.join("writes.jsonl")).unwrap_or_default()
    }

    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.dir.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("logged gh argv"))
            .collect()
    }

    fn assert_no_unhandled_calls(&self) {
        assert!(
            !self.dir.join("unhandled.jsonl").exists(),
            "gh received an unrecognized invocation: {:?}",
            std::fs::read_to_string(self.dir.join("unhandled.jsonl"))
        );
    }

    fn marker_comments(&self, repo: &str, issue: u64) -> Vec<serde_json::Value> {
        self.state(repo)["comments"][issue.to_string()]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| {
                c["body"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(CLAIM_MARKER)
            })
            .cloned()
            .collect()
    }

    /// Set the canned `gh issue list` output for `repo` (issue numbers, in
    /// returned order).
    fn set_issues(&self, repo: &str, numbers: &[u64]) {
        let body = numbers
            .iter()
            .map(|n| format!("{{\"number\":{n}}}"))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            self.repo_dir(repo).join("issues.json"),
            format!("[{body}]\n"),
        )
        .expect("write issues.json");
    }

    /// PRD #421: like [`set_issues`] but each issue carries its own label set
    /// (possibly empty), so a test can pre-seed a label GitHub already carries
    /// (applied by a human, another tool, or a prior deck) rather than one this
    /// deck just wrote. The labels are embedded in BOTH the `gh issue list`
    /// response (a `labels` field per item) and a per-issue `gh issue view`
    /// fixture (`issue-<n>.json`, also carrying an empty `comments` array) —
    /// M1.2's exact read mechanism isn't decided yet, so both plausible shapes
    /// are served identically.
    fn set_issues_with_labels(&self, repo: &str, entries: &[(u64, &[&str])]) {
        let labels_json = |labels: &[&str]| {
            labels
                .iter()
                .map(|l| format!("{{\"name\":\"{l}\"}}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let body = entries
            .iter()
            .map(|(n, labels)| format!("{{\"number\":{n},\"labels\":[{}]}}", labels_json(labels)))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            self.repo_dir(repo).join("issues.json"),
            format!("[{body}]\n"),
        )
        .expect("write issues.json with labels");
        for (n, labels) in entries {
            std::fs::write(
                self.repo_dir(repo).join(format!("issue-{n}.json")),
                format!(
                    "{{\"number\":{n},\"labels\":[{}],\"comments\":[]}}\n",
                    labels_json(labels)
                ),
            )
            .expect("write issue-<n>.json fixture");
        }
    }

    /// PRD #421: every `gh` invocation recorded so far (one line per call, in
    /// call order), so a test can assert on WHAT `gh` was asked to do — e.g.
    /// that `issue edit --add-label in-progress` was (or was never) invoked —
    /// without needing a new Rust type for a write path that doesn't exist yet.
    fn gh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("gh-calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// PRD #421 review fix: seed `repo`'s pre-existing label set — the labels
    /// a real repo already carries before this fire — so `gh issue edit
    /// --add-label <name>` succeeds against `<name>` (mirroring real `gh`'s
    /// label-name-to-ID resolution, which hard-errors on an unknown label).
    /// `gh label create` (idempotent, `--force`) also appends to this same
    /// set, so a create-then-add sequence succeeds without pre-seeding.
    fn seed_labels(&self, repo: &str, labels: &[&str]) {
        let rd = self.repo_dir(repo);
        std::fs::create_dir_all(&rd).expect("create repo fixture dir");
        let body: String = labels.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(rd.join("labels"), body).expect("write labels seed file");
    }

    /// PRD #421 review fix: whether a `gh issue edit --add-label <label>`
    /// call for `issue` on `repo` SUCCEEDED against the tightened stub — i.e.
    /// `label` was already in the repo's known label set at call time, so the
    /// mutation genuinely went through (mirroring real `gh`). Distinct from
    /// [`Self::gh_calls`], which records every invocation ATTEMPTED
    /// regardless of outcome.
    fn label_applied(&self, repo: &str, issue: u64, label: &str) -> bool {
        std::fs::read_to_string(self.repo_dir(repo).join(format!("applied-{issue}")))
            .unwrap_or_default()
            .lines()
            .any(|l| l == label)
    }

    /// PRD fork#235 M2: the current assignee list for `issue` on `repo`, one
    /// login per line, after every `--add-assignee`/`--remove-assignee`
    /// write so far (replace-to-one state).
    fn assignees(&self, repo: &str, issue: u64) -> Vec<String> {
        std::fs::read_to_string(
            self.dir
                .join(format!("{}/assignees-{issue}", Self::key(repo))),
        )
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
    }

    /// PRD fork#235 M2: arm `gh api user --jq .login` to fail (a
    /// read-only/expired token) — `scheduler/dispatch/022`'s scenario.
    fn fail_api_user(&self) {
        std::fs::write(self.dir.join("fail-api-user"), b"").expect("write fail-api-user marker");
    }

    /// Make `gh pr list --head agent/issue-<n>` report an open PR (skip signal).
    fn set_open_pr(&self, repo: &str, issue: u64) {
        std::fs::write(
            self.repo_dir(repo).join(format!("pr-{issue}.json")),
            "[{\"number\":4242}]\n",
        )
        .expect("write pr-<n>.json");
    }

    /// Make `gh pr list --head agent/issue-<n>` exit non-zero (a simulated
    /// per-issue GitHub error).
    fn fail_pr(&self, repo: &str, issue: u64) {
        std::fs::write(self.repo_dir(repo).join(format!("fail-pr-{issue}")), b"")
            .expect("write fail-pr-<n> marker");
    }

    /// PATH with the stub `gh` dir first, so the daemon's `gh` resolves here
    /// while `git` still comes from the real PATH.
    fn path_env(&self) -> String {
        format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn ghstub_dir(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }
}

/// Initialize a fixture remote: a real git repo with one commit (`README.md`,
/// plus the single-role [`ORCH_TOML`] `.dot-agent-deck.toml` when
/// `with_orchestration`). Thin wrapper over [`init_remote_with_orch_toml`].
fn init_remote(remote: &Path, with_orchestration: bool) {
    init_remote_with_orch_toml(remote, with_orchestration.then_some(ORCH_TOML));
}

/// Initialize a fixture remote committing `README.md` plus, when `orch_toml` is
/// `Some`, that exact `.dot-agent-deck.toml` content (so callers can choose a
/// single- or multi-role orchestration config). Commit identity is pinned inline
/// so the repo does not depend on the host's global git config.
fn init_remote_with_orch_toml(remote: &Path, orch_toml: Option<&str>) {
    std::fs::create_dir_all(remote).expect("create remote dir");
    run_git(remote, &["-c", "init.defaultBranch=main", "init", "-q"]);
    std::fs::write(remote.join("README.md"), "issue-dispatch fixture\n").expect("write README");
    if let Some(toml) = orch_toml {
        std::fs::write(remote.join(".dot-agent-deck.toml"), toml)
            .expect("write fixture .dot-agent-deck.toml");
    }
    run_git(remote, &["add", "-A"]);
    run_git(remote, &["commit", "-q", "-m", "init"]);
}

/// Through `common::fixture_git`, so the ambient git LOCATION variables are
/// cleared: a bare `git` with only `.current_dir` resolves an ambient `GIT_DIR`
/// in preference to it, which makes `init`/`add`/`commit` here writes against
/// whatever that names (issue #834). It also supplies the commit identity by
/// environment and neutralizes the host's config, which is what replaced the
/// `-c user.email=… -c user.name=… -c commit.gpgsign=false` this file used to
/// thread through the one `commit`. `dir` is its own sandbox root — every
/// caller passes a repository root.
fn run_git(dir: &Path, args: &[&str]) {
    let status = common::fixture_git(dir, dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

// ---------------------------------------------------------------------------
// schedules.toml builder
// ---------------------------------------------------------------------------

/// One `[[scheduled_tasks]]` block with an `[scheduled_tasks.issue_dispatch]`
/// sub-table (no top-level `command` — the per-issue command comes from each
/// cloned repo's config). The cron never fires on its own; fires are driven via
/// run-now.
fn dispatch_task(
    name: &str,
    working_dir: &str,
    prompt: &str,
    repo: &str,
    max_per_run: usize,
) -> String {
    format!(
        "[[scheduled_tasks]]\n\
         name = \"{name}\"\n\
         cron = \"0 0 1 1 *\"\n\
         working_dir = \"{working_dir}\"\n\
         prompt = \"{prompt}\"\n\
         enabled = true\n\
         \n\
         [scheduled_tasks.issue_dispatch]\n\
         repo = \"{repo}\"\n\
         max_per_run = {max_per_run}\n\
         \n"
    )
}

/// PRD fork#235: build a workspace tempdir + single-task `issue_dispatch`
/// schedule, then spawn a daemon over it — the setup shared verbatim by
/// `scheduler/dispatch/010`, `021`, and `022`, which each fire it via
/// `run_now` themselves and diverge only in their fixture (`stub`) and
/// assertions afterward. Returns the workspace `TempDir` (kept alive by the
/// caller for the daemon's lifetime), the workspace path as a `String` (for
/// `derive_issue_paths`), and the running daemon.
fn spawn_dispatch_daemon(
    stub: &GhStub,
    task_name: &str,
    id_template: &str,
    repo: &str,
) -> (tempfile::TempDir, String, common::DaemonProc) {
    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(task_name, &work_str, id_template, repo, 5);
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);
    (work_td, work_str, daemon)
}

/// Like [`dispatch_task`] but with `triage = true` appended to the
/// `[scheduled_tasks.issue_dispatch]` table (PRD #421 M2.0, opt-in). Today
/// `IssueDispatchConfig` carries no `triage` field and no
/// `deny_unknown_fields`, so this key parses cleanly and is silently
/// ignored — the RED this produces is about missing BEHAVIOR, not a config
/// parse error.
fn dispatch_task_with_triage(
    name: &str,
    working_dir: &str,
    prompt: &str,
    repo: &str,
    max_per_run: usize,
) -> String {
    format!(
        "{}triage = true\n",
        dispatch_task(name, working_dir, prompt, repo, max_per_run)
    )
}

/// PRD #421 M2.0's seven-label triage vocabulary, hyphenated to match house
/// style (`in-progress`, `ci-cd`) — settling the PRD's own open label-naming
/// question. The deck ensures these exist (idempotently) and delivers them
/// to the dispatched agent's prompt; the deck itself never applies one to an
/// issue (that is the spawned agent's job, via its own `gh` calls).
const TRIAGE_LABELS: [&str; 7] = [
    "priority-high",
    "priority-medium",
    "priority-low",
    "size-high",
    "size-medium",
    "size-low",
    "needs-triage",
];

// ---------------------------------------------------------------------------
// Observers
// ---------------------------------------------------------------------------

/// Whether `r` is the `orchestrator` role of an orchestration tab rooted at
/// `worktree` (the spawn target recorded as `orchestration_cwd`).
fn orchestrator_in(r: &AgentRecord, worktree: &Path) -> bool {
    let want = worktree.to_string_lossy();
    matches!(
        &r.tab_membership,
        Some(TabMembership::Orchestration { role_name, orchestration_cwd, .. })
            if role_name == "orchestrator" && orchestration_cwd.as_deref() == Some(want.as_ref())
    )
}

/// Whether `r` is the role named `role` of an orchestration tab rooted at
/// `worktree` (its `orchestration_cwd`). Generalises [`orchestrator_in`] so a
/// multi-role dispatch can match a specific sibling role (e.g. `reviewer`).
fn role_in(r: &AgentRecord, role: &str, worktree: &Path) -> bool {
    let want = worktree.to_string_lossy();
    matches!(
        &r.tab_membership,
        Some(TabMembership::Orchestration { role_name, orchestration_cwd, .. })
            if role_name == role && orchestration_cwd.as_deref() == Some(want.as_ref())
    )
}

/// Whether `r` is any orchestrator-role pane (used to count dispatched
/// orchestration tabs across issues).
fn is_orchestrator(r: &AgentRecord) -> bool {
    matches!(
        &r.tab_membership,
        Some(TabMembership::Orchestration { role_name, .. }) if role_name == "orchestrator"
    )
}

/// Whether `r` is a non-orchestration single-agent card whose spawn cwd is
/// `worktree`.
fn single_card_in(r: &AgentRecord, worktree: &Path) -> bool {
    let want = worktree.to_string_lossy();
    !matches!(r.tab_membership, Some(TabMembership::Orchestration { .. }))
        && r.cwd.as_deref() == Some(want.as_ref())
}

fn count_orchestrators(daemon: &common::DaemonProc) -> usize {
    daemon
        .agent_records()
        .iter()
        .filter(|r| is_orchestrator(r))
        .count()
}

/// Render the exact delimited fragment production writes into a claim
/// comment for a given worktree path + branch — `` working `<path>` on
/// branch `<branch>` `` — by constructing the [`Identity`] production would
/// build (canonicalising `path` exactly as `Identity::worktree` does,
/// falling back to the given path unchanged on a canonicalisation error) and
/// rendering it through the REAL [`claim_comment_body`], rather than
/// reimplementing the format string here. Reimplementing it is exactly
/// round 2's own mistake: `contains(path) && contains(branch)` as two
/// independent, undelimited substring checks is what let a comment naming a
/// SIBLING worktree — the expected path plus a `-stale` suffix — pass
/// undetected as long as it also carried the correct branch (fork#243 round
/// 3). Comparing against production's OWN rendering, backticks included,
/// closes that hole: `<path>-stale` is never a substring of `` `<path>` ``
/// (the closing backtick lands right after `<path>`, not after the suffix).
///
/// The two fields are located by their STRUCTURAL delimiters — the
/// backticks `claim_comment_body` wraps each field in — not by searching for
/// the prose that surrounds them (fork#243 round 4). `` " working `" `` and
/// `` "` on branch `" `` are safe to treat as fixed markers because they sit
/// OUTSIDE both fields, but `" on host "` is not: it is ordinary text a
/// worktree path or a claimant name can legitimately contain (e.g. a
/// `ScheduledTask` named `dispatch on host shadow`), and round 3's marker
/// search took its FIRST occurrence anywhere after the path opened —
/// including one embedded inside the path itself — which truncates the
/// fragment before the path (and the entire branch) has been read.
/// `claim_comment_body` sanitizes both the path and the branch through
/// [`sanitize_claimant_name`] before embedding them, which strips backticks
/// unconditionally, so neither field can ever contain one; that is what
/// makes a backtick an unambiguous field boundary while `" on host "` is
/// not. If the expected structure — `` working `<path>` on branch `<branch>` ``,
/// each field closed by a backtick — is not exactly present, this panics
/// rather than falling back to a shorter, weaker fragment that would match
/// more than the real one.
fn worktree_identity_fragment(path: &Path, branch: &str) -> String {
    const OPEN: &str = " working `";
    const MID: &str = "` on branch `";

    let identity = Identity::worktree(path, branch);
    let body = claim_comment_body(&identity, "0000-00-00T00:00:00Z", None, None);

    let start = body
        .find(OPEN)
        .expect("claim_comment_body always renders ` working ` for an Identity::Worktree");
    let path_field_start = start + OPEN.len();
    let path_field_end = body[path_field_start..]
        .find('`')
        .map(|i| path_field_start + i)
        .expect(
            "the path field must close with a backtick — sanitize_claimant_name strips \
             backticks from the path (src/issue_dispatch.rs), so a missing closing backtick \
             means the rendered body isn't shaped the way claim_comment_body renders it, not \
             that the path legitimately contains one",
        );

    let mid_start = path_field_end;
    let mid_actual = body.get(mid_start..mid_start + MID.len());
    assert_eq!(
        mid_actual,
        Some(MID),
        "expected the literal {MID:?} immediately after the path field's closing backtick \
         (claim_comment_body's fixed ` on branch ` clause); found {mid_actual:?} in body {body:?}",
    );

    let branch_field_start = mid_start + MID.len();
    let branch_field_end = body[branch_field_start..]
        .find('`')
        .map(|i| branch_field_start + i)
        .expect(
            "the branch field must close with a backtick — sanitize_claimant_name strips \
             backticks from the branch too",
        );

    body[start..=branch_field_end].to_string()
}

/// Whether any recorded `gh` call line is an `issue comment` invocation
/// whose body contains the EXACT delimited identity fragment
/// ([`worktree_identity_fragment`]) for `path` + `branch`. This is the
/// actual predicate [`assert_claim_names_worktree_identity`] asserts,
/// pulled out into its own pure function — no `GhStub`, no `wait_until`, no
/// daemon — so
/// `worktree_identity_fragment_rejects_a_path_sharing_only_a_prefix` below
/// can pin its rejection behaviour against synthetic input directly.
fn gh_calls_name_worktree_identity(calls: &[String], path: &Path, branch: &str) -> bool {
    let fragment = worktree_identity_fragment(path, branch);
    calls
        .iter()
        .any(|l| l.contains("issue") && l.contains("comment") && l.contains(&fragment))
}

/// Assert that a recorded `gh issue comment` call names the round-3 identity
/// anchor EXACTLY as production renders it — the delimited fragment ``
/// working `<canonical path>` on branch `<branch>` `` (CLAUDE.md rule 23),
/// matched via [`gh_calls_name_worktree_identity`] against the REAL
/// `claim_comment_body` output, not two independently `contains()`-checked
/// substrings. Round 2's independent-substrings version is bypassable by a
/// comment naming a DIFFERENT worktree that merely shares the expected path
/// as a PREFIX (`<path>-stale`) alongside the correct branch; see
/// [`worktree_identity_fragment`]'s own doc for why the delimited-fragment
/// match closes that hole, and
/// `worktree_identity_fragment_rejects_a_path_sharing_only_a_prefix` for the
/// counterexample that pins it (fork#243 round 3). `decoration` fills the
/// site-specific tail of the failure message — pass a full clause such as
/// `"decorate it with the claiming task name"` / `"decorate it with the
/// orchestration name"`.
fn assert_claim_names_worktree_identity(
    stub: &GhStub,
    worktree_dir: &Path,
    branch: &str,
    decoration: &str,
) {
    let fragment = worktree_identity_fragment(worktree_dir, branch);
    let named_identity = common::wait_until(Duration::from_secs(3), || {
        gh_calls_name_worktree_identity(&stub.gh_calls(), worktree_dir, branch)
    });
    assert!(
        named_identity,
        "the claim comment must name the round-3 identity anchor exactly as production renders \
         it ({fragment:?}, CLAUDE.md rule 23) — not merely {decoration}; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );
}

/// The counterexample fork#243 round 3 exists for: pins that
/// [`gh_calls_name_worktree_identity`] — the predicate
/// [`assert_claim_names_worktree_identity`] asserts on — REJECTS a recorded
/// claim comment naming a DIFFERENT worktree that merely shares the
/// expected canonical path as a PREFIX (`<path>-stale`), even paired with
/// the exactly correct branch. That is precisely the shape round 2's
/// `contains(path) && contains(branch)` predicate let through undetected:
/// three rounds of this same assertion each stayed green for a weaker
/// reason than intended, and none of the first two shipped a test that
/// could have caught its own weakness. No `GhStub`, no daemon, no dispatch
/// run — a synthetic recorded-call line is built by hand in the exact shape
/// `claim_comment_body` renders, so this pins pure predicate logic; it is
/// gated behind the `e2e` feature only because this whole file is (rule 5),
/// not because it needs a PTY, a daemon, or a subprocess of its own.
#[test]
fn worktree_identity_fragment_rejects_a_path_sharing_only_a_prefix() {
    let dir = common::harness_tempdir().expect("tempdir");
    // Canonicalise up front so every use of `expected_path` below already IS
    // the canonical form `Identity::worktree`'s own (idempotent) internal
    // canonicalisation would produce — otherwise a raw-vs-canonical
    // divergence (e.g. a `/var` -> `/private/var` symlink on macOS) would
    // make the `-stale` suffix built below land on the wrong string and the
    // counterexample would prove nothing.
    let expected_path = std::fs::canonicalize(dir.path()).expect("tempdir canonicalizes");
    let branch = "agent/issue-41";

    let correct_fragment = worktree_identity_fragment(&expected_path, branch);
    let correct_calls = vec![format!(
        "gh issue comment 41 --body \"Claimed by the orchestration{correct_fragment} on host h \
         at 2026-08-13T00:00:00Z.\""
    )];
    assert!(
        gh_calls_name_worktree_identity(&correct_calls, &expected_path, branch),
        "the predicate must ACCEPT a call naming the exact canonical path and branch; built \
         call:\n{}",
        correct_calls.join("\n")
    );

    // The counterexample: a SIBLING worktree whose path has the expected
    // canonical path as a PREFIX, still paired with the correct branch.
    // Built by hand (not via `worktree_identity_fragment`, which would
    // canonicalise this non-existent `-stale` path right back down to
    // `expected_path` itself via its `unwrap_or_else` fallback and defeat
    // the point of this test) in the exact delimited shape
    // `claim_comment_body` renders.
    let stale_path = format!("{}-stale", expected_path.display());
    let stale_calls = vec![format!(
        "gh issue comment 41 --body \"Claimed by the orchestration working `{stale_path}` on \
         branch `{branch}` on host h at 2026-08-13T00:00:00Z.\""
    )];
    assert!(
        !gh_calls_name_worktree_identity(&stale_calls, &expected_path, branch),
        "the predicate must REJECT a call naming a different worktree that merely shares the \
         expected canonical path as a prefix ({stale_path:?}) — round 2's \
         `contains(path) && contains(branch)` predicate let exactly this shape through \
         undetected (fork#243 round 3); observed synthetic calls:\n{}",
        stale_calls.join("\n")
    );
}

/// The counterexample fork#243 round 4 exists for: pins that
/// [`gh_calls_name_worktree_identity`] locates the fragment's end by its
/// STRUCTURAL delimiter (the path field's closing backtick), not by
/// searching for `" on host "` as a marker — closing the hole round 3's
/// marker-search parser reopened. A `ScheduledTask` legitimately named
/// e.g. `dispatch on host shadow` produces a worktree path CONTAINING the
/// literal text `" on host "` (`sanitize_clone_segment` preserves ordinary
/// spaces and text; `claim_comment_body` interpolates the path unchanged
/// apart from control characters and backticks), so round 3's parser —
/// which took the FIRST `" on host "` after the path opened as the
/// fragment's end — stopped INSIDE the path itself, well before its
/// closing backtick, and a regression that dropped the rest of the path
/// and the entire branch clause, while still writing that truncated prefix
/// into an `issue comment` call, would have passed the round-3 predicate
/// undetected. No `GhStub`, no daemon, no dispatch run — mirrors
/// `worktree_identity_fragment_rejects_a_path_sharing_only_a_prefix` above,
/// same shape, different hole.
#[test]
fn worktree_identity_fragment_rejects_the_round_3_marker_search_truncation() {
    let base = common::harness_tempdir().expect("tempdir");
    // The path segment itself contains the literal text `" on host "` —
    // exactly the marker round 3's parser searched for — so a REAL,
    // legitimately-named worktree exercises the hole rather than a
    // contrived string.
    let nested = base
        .path()
        .join("dispatch on host shadow")
        .join(".worktrees")
        .join("issue-41");
    std::fs::create_dir_all(&nested).expect("create nested dir with marker text in a segment");
    let expected_path = std::fs::canonicalize(&nested).expect("canonicalizes");
    let branch = "agent/issue-41";

    let correct_fragment = worktree_identity_fragment(&expected_path, branch);
    assert!(
        correct_fragment.contains("` on branch `"),
        "sanity: the correct fragment must include the full path AND the branch clause; got \
         {correct_fragment:?}",
    );

    // Round 3's parser, replayed by hand: `" working `"`, then the FIRST
    // `" on host "` after it — which for THIS path lands inside the path's
    // own text, well before the path field's actual closing backtick.
    let path_str = expected_path.to_string_lossy().into_owned();
    let marker_in_path = path_str
        .find(" on host ")
        .expect("this path is built to contain the marker text");
    let round_3_truncated_fragment = format!(" working `{}", &path_str[..marker_in_path]);
    assert!(
        correct_fragment.starts_with(&round_3_truncated_fragment)
            && round_3_truncated_fragment.len() < correct_fragment.len(),
        "sanity: the truncated prefix must be a genuine, strictly shorter prefix of the \
         correct fragment, or this path doesn't exercise the hole; truncated \
         {round_3_truncated_fragment:?}, correct {correct_fragment:?}",
    );

    // The regression: a comment that never gets past that truncated
    // prefix — the rest of the path AND the entire branch clause dropped —
    // is exactly the shape round 3's marker-search parser would have
    // accepted.
    let truncated_calls = vec![format!(
        "gh issue comment 41 --body \"Claimed by the orchestration{round_3_truncated_fragment} \
         on host h at 2026-08-13T00:00:00Z.\""
    )];
    assert!(
        !gh_calls_name_worktree_identity(&truncated_calls, &expected_path, branch),
        "the predicate must REJECT a call carrying only the prefix round 3's `\" on host \"` \
         marker-search parser would have accepted, truncated because the path itself contains \
         that marker text ({round_3_truncated_fragment:?}) — round 3 (fork#243) closed the \
         sibling-path hole but reopened this one via the same marker-search parser (fork#243 \
         round 4); observed synthetic calls:\n{}",
        truncated_calls.join("\n")
    );
}

/// Whether the clone's `git worktree list` still references `worktree`.
fn git_worktree_listed(clone: &Path, worktree: &Path) -> bool {
    let out = common::fixture_git(clone, clone)
        .args(["worktree", "list", "--porcelain"])
        .output();
    match out {
        Ok(o) => {
            let listed = String::from_utf8_lossy(&o.stdout);
            let needle = worktree.to_string_lossy();
            listed.contains(needle.as_ref())
        }
        Err(_) => false,
    }
}

/// Whether the clone has a local branch named `branch`.
fn git_branch_exists(clone: &Path, branch: &str) -> bool {
    let out = common::fixture_git(clone, clone)
        .args(["branch", "--list", branch])
        .output();
    matches!(out, Ok(o) if !String::from_utf8_lossy(&o.stdout).trim().is_empty())
}

/// The ceiling on a dispatch outcome that must HAPPEN — a clone, a worktree, an
/// agent registering, its output, a removal. Each is real `git` work and a
/// process start on the daemon's side, so it is [`common::load_scaled`]: a flat
/// 15 s failed `dispatch_007`, `dispatch_008` and `dispatch_009` in PRD #1631's
/// broad e2e run with `io full` at 75-79%, and all three passed alone. Never used
/// for a window in which something must NOT happen; those stay flat.
fn w() -> Duration {
    common::load_scaled(Duration::from_secs(15))
}

fn is_claim_write(call: &[String]) -> bool {
    call.starts_with(&["label".into(), "create".into()])
        || call.starts_with(&["issue".into(), "edit".into()])
        || call.starts_with(&["api".into(), "--method".into()])
}

fn comment_writes(stub: &GhStub, method: &str) -> usize {
    stub.calls()
        .iter()
        .filter(|call| call.starts_with(&["api".into(), "--method".into(), method.into()]))
        .count()
}

fn scheduler_messages(daemon: &common::DaemonProc) -> Vec<String> {
    // DaemonProc's public HOME is under the same sandbox root as its captured
    // stderr; observing this file uses the existing StderrNotifier seam.
    std::fs::read_to_string(daemon.home.parent().unwrap().join("daemon-stderr.log"))
        .expect("read daemon notifications")
        .lines()
        .filter(|line| line.starts_with("[scheduler]"))
        .map(str::to_owned)
        .collect()
}

fn claim_failure_messages(daemon: &common::DaemonProc) -> Vec<String> {
    claim_failure_messages_for(daemon, 7)
}

fn claim_failure_messages_for(daemon: &common::DaemonProc, issue: u64) -> Vec<String> {
    scheduler_messages(daemon)
        .into_iter()
        .filter(|line| {
            let lower = line.to_lowercase();
            lower.contains("claim")
                && lower.contains("fail")
                && lower.contains(&format!("#{issue}"))
        })
        .collect()
}

fn claim_task(work: &Path, gating: Option<bool>, ttl: u64, cron: &str) -> String {
    let mut task = dispatch_task(
        "dispatch-task",
        work.to_str().unwrap(),
        "CLAIM-{{issue_number}}",
        "acme/widgets",
        5,
    )
    .replace("0 0 1 1 *", cron);
    if let Some(enabled) = gating {
        task.push_str(&format!("claim_gating = {enabled}\n"));
    }
    task.push_str(&format!("claim_ttl_hours = {ttl}\n"));
    task
}

fn claim_daemon(stub: &GhStub, toml: &str) -> common::DaemonProc {
    common::spawn_daemon_serve_with_env(
        Some(toml),
        "0",
        &[
            ("PATH", &stub.path_env()),
            ("GHSTUB_DIR", &stub.ghstub_dir()),
        ],
    )
}

fn claim_fixture() -> (GhStub, tempfile::TempDir, PathBuf) {
    let stub = GhStub::new();
    stub.add_repo("acme/widgets", true);
    stub.set_issues("acme/widgets", &[7]);
    let directory = common::harness_tempdir().unwrap();
    let work = directory.path().join("ws");
    std::fs::create_dir_all(&work).unwrap();
    (stub, directory, work)
}

fn wait_for_claim_dispatch(daemon: &common::DaemonProc, stub: &GhStub, work: &Path, issue: u64) {
    let paths = derive_issue_paths(work, "dispatch-task", issue);
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .is_some(),
        "issue #{issue} must dispatch; messages={:?}; calls={:?}",
        scheduler_messages(daemon),
        stub.calls()
    );
    assert!(daemon.wait_for_stderr_contains(&format!("dispatched issue #{issue}"), w()));
    stub.assert_no_unhandled_calls();
}

/// Scenario: Fire a gated schedule with a fresh collaborator claim on issue 7, an unclaimed sibling, and unaffiliated authors copying the collaborator's record onto issues 9 and 10.
/// Only the collaborator's issue skips, identifying its actual GitHub login, while both copied claims permit dispatch.
/// With an hourly schedule and a 3-hour reader TTL, a 24-hour-old collaborator claim stating a 168-hour lifetime still skips, while its legacy-format counterpart dispatches.
#[spec("scheduler/dispatch/032")]
#[test]
fn dispatch_032_fresh_foreign_claim_skips_with_claimant() {
    let (stub, _directory, work) = claim_fixture();
    stub.set_issues("acme/widgets", &[7, 8, 9, 10]);
    stub.seed_claim(
        "acme/widgets",
        7,
        99999,
        "other-schedule",
        "h1-1234567890abcdef",
        1,
    );
    for (issue, association) in [(9, "NONE"), (10, "CONTRIBUTOR")] {
        stub.seed_claim(
            "acme/widgets",
            issue,
            88888,
            "other-schedule",
            "h1-1234567890abcdef",
            1,
        );
        let mut state = stub.state("acme/widgets");
        state["comments"][issue.to_string()][0]["author_association"] = association.into();
        // Copy the trusted deck's complete marker-bearing body; only GitHub's
        // authenticated author/association differ, including after an edit.
        state["comments"][issue.to_string()][0]["body"] = state["comments"]["7"][0]["body"].clone();
        state["comments"][issue.to_string()][0]["updated_at"] =
            chrono::Utc::now().to_rfc3339().into();
        std::fs::write(
            stub.repo_dir("acme/widgets").join("state.json"),
            state.to_string(),
        )
        .unwrap();
    }
    let daemon = claim_daemon(&stub, &claim_task(&work, Some(true), 168, "0 0 * * *"));
    daemon.run_now("dispatch-task").unwrap();
    wait_for_claim_dispatch(&daemon, &stub, &work, 8);
    for issue in [9, 10] {
        wait_for_claim_dispatch(&daemon, &stub, &work, issue);
        assert!(!daemon.stderr_contains(&format!("issue #{issue} of acme/widgets failed:")));
    }
    assert!(
        !derive_issue_paths(&work, "dispatch-task", 7)
            .worktree_dir
            .exists(),
        "fresh foreign claim must skip issue #7 while sibling dispatches; messages={:?}",
        scheduler_messages(&daemon)
    );
    let lines = scheduler_messages(&daemon);
    assert!(
        lines.iter().any(|line| line.contains("#7")
            && line.contains("other-schedule")
            && line.contains("h1-1234567890abcdef")
            && line.contains("GitHub account claimant-99999")
            && line.to_lowercase().contains("claim")
            && regex::Regex::new(r"(?i)(age|hours?|[0-9]+h|seconds?|minutes?)")
                .unwrap()
                .is_match(line)
            && !line.to_lowercase().contains("worktree")
            && !line.to_lowercase().contains("open pr")),
        "claimed skip must name schedule, machine and age with its own reason: {lines:?}"
    );
    drop(daemon);

    let (stub, _directory, work) = claim_fixture();
    stub.set_issues("acme/widgets", &[7, 8]);
    for issue in [7, 8] {
        stub.seed_claim(
            "acme/widgets",
            issue,
            99999,
            "other-schedule",
            "h1-1234567890abcdef",
            24,
        );
    }
    // Both claims outlive this reader's TTL, but issue 7's writer refreshes
    // against its own longer lifetime; issue 8 models an older deck's format.
    stub.seed_claim_lifetime("acme/widgets", 7, 168);
    let foreign = stub.marker_comments("acme/widgets", 7);
    let daemon = claim_daemon(&stub, &claim_task(&work, Some(true), 3, "0 * * * *"));
    daemon.run_now("dispatch-task").unwrap();
    wait_for_claim_dispatch(&daemon, &stub, &work, 8);
    assert!(
        !daemon.stderr_contains("claim_gating is OFF"),
        "an hourly cron must keep gating enabled at a 3-hour reader TTL"
    );
    let lines = scheduler_messages(&daemon);
    assert!(
        lines.iter().any(|line| line.contains("#7")
            && line.contains("skipping already-claimed")
            && line.contains("other-schedule")
            && line.contains("GitHub account claimant-99999")),
        "a foreign claim within its writer's lifetime must skip despite exceeding the reader's TTL: {lines:?}"
    );
    assert!(
        !derive_issue_paths(&work, "dispatch-task", 7)
            .worktree_dir
            .exists(),
        "the still-live foreign claim must not create an issue-7 worktree"
    );
    assert_eq!(stub.marker_comments("acme/widgets", 7), foreign);
    assert_eq!(count_orchestrators(&daemon), 1);
}

/// Scenario: Fire a gated schedule against a foreign claim and label event older than the TTL.
/// Issue 7 starts an agent, leaves the foreign record intact, and gains a current deck-owned claimant record.
#[spec("scheduler/dispatch/033")]
#[test]
fn dispatch_033_stale_foreign_claim_dispatches_and_advances_age() {
    let (stub, _directory, work) = claim_fixture();
    stub.seed_claim(
        "acme/widgets",
        7,
        99999,
        "other-schedule",
        "h1-1234567890abcdef",
        200,
    );
    let foreign = stub.marker_comments("acme/widgets", 7).remove(0);
    let before = chrono::Utc::now();
    let daemon = claim_daemon(&stub, &claim_task(&work, Some(true), 168, "0 0 * * *"));
    daemon.run_now("dispatch-task").unwrap();
    wait_for_claim_dispatch(&daemon, &stub, &work, 7);
    let records = stub.marker_comments("acme/widgets", 7);
    assert!(
        records.contains(&foreign),
        "stale foreign record stays unchanged"
    );
    let own = records
        .iter()
        .find(|record| record["user"]["id"] == 12345)
        .expect("new own claim");
    let updated =
        chrono::DateTime::parse_from_rfc3339(own["updated_at"].as_str().unwrap()).unwrap();
    assert!(
        updated >= before,
        "dispatch must advance the derived claim time: {records:?}"
    );
}

/// Scenario: Dispatch issue 7, SIGKILL its daemon, delete the workspace and daemon state, then fire a fresh daemon against unchanged GitHub state.
/// An own dead claim and an additional stale foreign claim permit recovery; an additional fresh foreign claim blocks it.
#[spec("scheduler/dispatch/034")]
#[test]
fn dispatch_034_sigkill_workspace_loss_recovers_without_repo_write() {
    for foreign_age in [None, Some(200), Some(1)] {
        let (stub, _directory, work) = claim_fixture();
        if let Some(age) = foreign_age {
            stub.seed_claim(
                "acme/widgets",
                7,
                99999,
                "other-schedule",
                "h1-1234567890abcdef",
                age,
            );
        }
        // The initial dispatch deliberately has gating off, so the live-foreign
        // arm can start with BOTH records already persisted before the crash.
        let daemon = claim_daemon(&stub, &claim_task(&work, Some(false), 168, "0 0 * * *"));
        daemon.run_now("dispatch-task").unwrap();
        wait_for_claim_dispatch(&daemon, &stub, &work, 7);
        let state = stub.state("acme/widgets");
        let writes = stub.writes();
        let daemon_root = daemon.home.parent().unwrap().to_path_buf();
        // DaemonProc::drop SIGKILLs the isolated process group and reaps it;
        // it sends neither StopAgent nor a graceful daemon-shutdown request.
        drop(daemon);
        assert!(
            !daemon_root.exists(),
            "crashed daemon state directory is gone"
        );
        std::fs::remove_dir_all(&work).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let daemon = claim_daemon(&stub, &claim_task(&work, Some(true), 168, "0 0 * * *"));
        assert_eq!(
            stub.writes(),
            writes,
            "no repo write between fires; arm={foreign_age:?}"
        );
        assert_eq!(
            stub.state("acme/widgets"),
            state,
            "GitHub state survives workspace loss unchanged"
        );
        daemon.run_now("dispatch-task").unwrap();
        if foreign_age == Some(1) {
            assert!(
                common::wait_until(w(), || scheduler_messages(&daemon)
                    .iter()
                    .any(|line| line.contains("#7")
                        && line.contains("other-schedule")
                        && line.contains("h1-1234567890abcdef"))),
                "live foreign record must still gate after own workspace loss; messages={:?}",
                scheduler_messages(&daemon)
            );
            assert!(
                !derive_issue_paths(&work, "dispatch-task", 7)
                    .worktree_dir
                    .exists()
            );
        } else {
            wait_for_claim_dispatch(&daemon, &stub, &work, 7);
        }
        stub.assert_no_unhandled_calls();
    }
}

/// Scenario: Fire schedules with claim gating omitted and explicitly false while a fresh foreign claim exists.
/// Both dispatch issue 7 and write their own claim beside the preserved foreign record.
#[spec("scheduler/dispatch/035")]
#[test]
fn dispatch_035_default_and_false_disable_gating_but_write_claim() {
    for gating in [None, Some(false)] {
        let (stub, _directory, work) = claim_fixture();
        stub.seed_claim(
            "acme/widgets",
            7,
            99999,
            "other-schedule",
            "h1-1234567890abcdef",
            1,
        );
        let foreign = stub.marker_comments("acme/widgets", 7).remove(0);
        let daemon = claim_daemon(&stub, &claim_task(&work, gating, 168, "0 0 * * *"));
        daemon.run_now("dispatch-task").unwrap();
        wait_for_claim_dispatch(&daemon, &stub, &work, 7);
        let records = stub.marker_comments("acme/widgets", 7);
        assert_eq!(
            records.len(),
            2,
            "gating={gating:?} still writes its own claim"
        );
        assert!(records.contains(&foreign));
        assert_eq!(
            stub.state("acme/widgets")["issue_labels"]["7"],
            serde_json::json!(["agent-deck/claimed"])
        );
    }
}

/// Scenario: Dispatch two issues, backdate one own record past half-TTL, then fire twice with their worktrees present, with gating on, omitted, false, or disabled by cron validation.
/// The aged record refreshes once, and a fresh skip costs one account lookup plus one comments read per worktree, with no events or writes.
#[spec("scheduler/dispatch/036")]
#[test]
fn dispatch_036_worktree_skip_refreshes_owner_once_per_half_ttl() {
    for (gating, ttl) in [
        (Some(true), 168),
        (None, 168),
        (Some(false), 168),
        (Some(true), 48),
    ] {
        let (stub, _directory, work) = claim_fixture();
        stub.set_issues("acme/widgets", &[7, 9, 8]);
        stub.set_open_pr("acme/widgets", 8);
        // Issue 8 is a read-only end-of-fire barrier: its skip is emitted after
        // issue 7's refresh finishes, so assertions cannot race a pending edit.
        let completed_fires = |daemon: &common::DaemonProc| {
            scheduler_messages(daemon)
                .iter()
                .filter(|line| line.contains("#8") && line.to_lowercase().contains("skip"))
                .count()
        };
        let daemon = claim_daemon(&stub, &claim_task(&work, gating, ttl, "0 0 * * *"));
        daemon.run_now("dispatch-task").unwrap();
        wait_for_claim_dispatch(&daemon, &stub, &work, 7);
        assert!(common::wait_until(w(), || completed_fires(&daemon) == 1));
        let first = stub.marker_comments("acme/widgets", 7).remove(0);
        // Server-owned metadata is the freshness clock. Backdate the GitHub
        // fixture, preserving the real record's author, task, machine and body.
        let mut state = stub.state("acme/widgets");
        state["comments"]["7"][0]["updated_at"] = (chrono::Utc::now()
            - chrono::Duration::hours((ttl / 2 + 1) as i64))
        .to_rfc3339()
        .into();
        state["events"]["7"][0]["created_at"] = (chrono::Utc::now()
            - chrono::Duration::hours(ttl as i64))
        .to_rfc3339()
        .into();
        std::fs::write(
            stub.repo_dir("acme/widgets").join("state.json"),
            state.to_string(),
        )
        .unwrap();
        daemon.run_now("dispatch-task").unwrap();
        assert!(
            common::wait_until(w(), || comment_writes(&stub, "PATCH") == 1),
            "worktree skip must refresh its old own record once; messages={:?}; calls={:?}",
            scheduler_messages(&daemon),
            stub.calls()
        );
        assert!(common::wait_until(w(), || stub
            .marker_comments("acme/widgets", 7)[0]["body"]
            != first["body"]));
        let refreshed = stub.marker_comments("acme/widgets", 7).remove(0);
        assert!(common::wait_until(w(), || completed_fires(&daemon) == 2));
        let calls_before = stub.calls().len();
        daemon.run_now("dispatch-task").unwrap();
        assert!(common::wait_until(w(), || completed_fires(&daemon) == 3));
        assert_eq!(
            comment_writes(&stub, "PATCH"),
            1,
            "fresh owner record is not edited again"
        );
        assert_eq!(stub.marker_comments("acme/widgets", 7), vec![refreshed]);
        assert_eq!(count_orchestrators(&daemon), 2);
        let calls = stub.calls();
        let fresh_calls = &calls[calls_before..];
        let mut expected = vec![
            vec!["api".to_string(), "--include".into(), "user".into()],
            vec![
                "api".into(),
                "--paginate".into(),
                "repos/acme/widgets/issues/7/comments?per_page=100".into(),
            ],
            vec![
                "api".into(),
                "--paginate".into(),
                "repos/acme/widgets/issues/9/comments?per_page=100".into(),
            ],
        ];
        let mut claim_calls: Vec<_> = fresh_calls
            .iter()
            .filter(|call| call[0] == "api" || is_claim_write(call))
            .cloned()
            .collect();
        claim_calls.sort();
        expected.sort();
        assert_eq!(
            claim_calls, expected,
            "fresh worktree skips have an explicit three-read cost; gating={gating:?}; ttl={ttl}; calls={fresh_calls:?}"
        );
        assert_eq!(
            fresh_calls.len(),
            5,
            "only issue enumeration and the issue-8 PR check accompany the three claim reads: {fresh_calls:?}"
        );
        if ttl == 48 {
            assert!(daemon.stderr_contains(
                "[scheduler] warning: schedule \"dispatch-task\": claim_gating is OFF"
            ));
        }
        assert!(
            scheduler_messages(&daemon)
                .iter()
                .any(|line| line.contains("#7") && line.to_lowercase().contains("worktree")),
            "skip retains the worktree reason"
        );
        stub.assert_no_unhandled_calls();
    }
}

/// Scenario: Load a weekday 09:00 schedule with gating enabled and a 100-hour TTL, shorter than twice its 72-hour weekend gap.
/// The daemon warns that gating is off for this task, loads it successfully, and dispatches a freshly foreign-claimed issue.
#[spec("scheduler/dispatch/037")]
#[test]
fn dispatch_037_short_ttl_warns_and_disables_gating() {
    let (stub, _directory, work) = claim_fixture();
    stub.seed_claim(
        "acme/widgets",
        7,
        99999,
        "other-schedule",
        "h1-1234567890abcdef",
        1,
    );
    let daemon = claim_daemon(
        &stub,
        &claim_task(&work, Some(true), 100, "0 9 * * MON-FRI"),
    );
    daemon.run_now("dispatch-task").unwrap();
    wait_for_claim_dispatch(&daemon, &stub, &work, 7);
    let log =
        std::fs::read_to_string(daemon.home.parent().unwrap().join("daemon-stderr.log")).unwrap();
    assert!(
        log.lines().any(|line| {
            let lower = line.to_lowercase();
            line.starts_with("[scheduler] warning:")
                && line.contains("dispatch-task")
                && line.contains("claim_gating is OFF")
                && lower.contains("ttl")
                && lower.contains("gating")
                && (lower.contains("off") || lower.contains("disabl"))
        }),
        "invalid TTL must warn that gating is off for dispatch-task; log={log}"
    );
}

/// Scenario: Fire a gated schedule with fresh foreign claims on two issues after a human removes the claim label from issue 7.
/// Issue 7 starts an agent and preserves the foreign comment, while issue 8 keeps its label and is skipped for its foreign claim.
#[spec("scheduler/dispatch/038")]
#[test]
fn dispatch_038_removed_label_releases_fresh_foreign_claim() {
    let (stub, _directory, work) = claim_fixture();
    // The control runs first, so the successful dispatch is also a barrier
    // proving the still-labeled issue has finished its decision.
    stub.set_issues("acme/widgets", &[8, 7]);
    for issue in [7, 8] {
        stub.seed_claim(
            "acme/widgets",
            issue,
            99999,
            "other-schedule",
            "h1-1234567890abcdef",
            1,
        );
    }
    let foreign = stub.marker_comments("acme/widgets", 7).remove(0);
    let control = stub.marker_comments("acme/widgets", 8);
    let mut state = stub.state("acme/widgets");
    state["issue_labels"]["7"] = serde_json::json!([]);
    state["events"]["7"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "event": "unlabeled", "label": {"name": "agent-deck/claimed"},
            "created_at": (chrono::Utc::now() - chrono::Duration::minutes(30)).to_rfc3339()
        }));
    std::fs::write(
        stub.repo_dir("acme/widgets").join("state.json"),
        state.to_string(),
    )
    .unwrap();

    let daemon = claim_daemon(&stub, &claim_task(&work, Some(true), 168, "0 0 * * *"));
    daemon.run_now("dispatch-task").unwrap();
    wait_for_claim_dispatch(&daemon, &stub, &work, 7);
    let messages = scheduler_messages(&daemon);
    assert!(
        messages
            .iter()
            .any(|line| line.contains("skipping already-claimed issue #8")
                && line.contains("other-schedule")
                && line.contains("h1-1234567890abcdef")),
        "the still-labeled control must skip for its fresh foreign claim: {messages:?}"
    );
    assert!(
        !derive_issue_paths(&work, "dispatch-task", 8)
            .worktree_dir
            .exists(),
        "the still-labeled control must not start work"
    );
    let records = stub.marker_comments("acme/widgets", 7);
    assert!(
        records.contains(&foreign),
        "manual release preserves the foreign record"
    );
    assert!(
        records.iter().any(|record| record["user"]["id"] == 12345),
        "the released issue receives the dispatching deck's own claim: {records:?}"
    );
    assert_eq!(stub.marker_comments("acme/widgets", 8), control);
}

/// Scenario: Fire a gated schedule with fresh foreign claims while an events, comments, or account read fails with HTTP 502 or hangs until the shared claim budget runs out.
/// Every issue still dispatches with an explanation and no dispatch failure; transient errors recover for claim writes, while budget exhaustion bounds calls across three issues.
#[spec("scheduler/dispatch/039")]
#[test]
fn dispatch_039_claim_read_failure_dispatches_with_warning() {
    for (kind, hanging) in [
        ("events", false),
        ("list", false),
        ("user", false),
        ("events", true),
        ("list", true),
        ("user", true),
    ] {
        let (stub, _directory, work) = claim_fixture();
        stub.seed_claim(
            "acme/widgets",
            7,
            99999,
            "other-schedule",
            "h1-1234567890abcdef",
            1,
        );
        let foreign = stub.marker_comments("acme/widgets", 7).remove(0);
        // Fail only the decision-time read; the post-spawn claim read can
        // recover, distinguishing a failed gate read from a failed write.
        if hanging {
            stub.set_issues("acme/widgets", &[7, 8, 9]);
            for issue in [8, 9] {
                stub.seed_claim(
                    "acme/widgets",
                    issue,
                    99999,
                    "other-schedule",
                    "h1-1234567890abcdef",
                    1,
                );
            }
            let directory = if kind == "user" {
                stub.dir.clone()
            } else {
                stub.repo_dir("acme/widgets")
            };
            std::fs::write(directory.join(format!("hang-{kind}")), "").unwrap();
        } else {
            stub.fail_first_calls("acme/widgets", kind, 1);
        }
        let daemon = common::spawn_daemon_serve_with_env(
            Some(&claim_task(&work, Some(true), 168, "0 0 * * *")),
            "0",
            &[
                ("PATH", &stub.path_env()),
                ("GHSTUB_DIR", &stub.ghstub_dir()),
                ("DOT_AGENT_DECK_ISSUE_CLAIM_TIMEOUT_MS", "5000"),
                ("DOT_AGENT_DECK_ISSUE_CLAIM_BUDGET_MS", "2000"),
            ],
        );
        daemon.run_now("dispatch-task").unwrap();
        wait_for_claim_dispatch(&daemon, &stub, &work, 7);
        let messages = scheduler_messages(&daemon);
        assert!(
            messages
                .iter()
                .any(|line| line.contains("dispatched issue #7")
                    && line.contains("without checking its claims")
                    && line.contains("could not be read")
                    && line.contains(if hanging { "budget" } else { "HTTP 502" })),
            "a failed {kind} read must dispatch with an explanatory warning: {messages:?}"
        );
        let records = stub.marker_comments("acme/widgets", 7);
        assert!(
            records.contains(&foreign),
            "a failed read preserves the foreign record"
        );
        assert!(!daemon.stderr_contains("issue #7 of acme/widgets failed:"));
        if hanging {
            for issue in [8, 9] {
                wait_for_claim_dispatch(&daemon, &stub, &work, issue);
                assert!(
                    !daemon.stderr_contains(&format!("issue #{issue} of acme/widgets failed:"))
                );
                assert!(scheduler_messages(&daemon).iter().any(|line| {
                    line.contains(&format!("dispatched issue #{issue}"))
                        && line.contains("without checking its claims")
                        && line.contains("budget")
                }));
            }
            let calls: Vec<_> = stub
                .calls()
                .into_iter()
                .filter(|call| call[0] == "api" || is_claim_write(call))
                .collect();
            let expected = match kind {
                "events" => 1,
                "list" => 2,
                "user" => 3,
                _ => unreachable!(),
            };
            assert_eq!(
                calls.len(),
                expected,
                "no claim call follows budget exhaustion during {kind}: {calls:?}"
            );
            assert!(
                stub.writes().is_empty(),
                "an exhausted read budget permits no later writes"
            );
            assert_eq!(count_orchestrators(&daemon), 3);
            stub.assert_no_unhandled_calls();
            continue;
        }
        assert!(
            records.iter().any(|record| record["user"]["id"] == 12345),
            "a failed {kind} gate read must still permit an own claim write: {records:?}"
        );
    }
}

/// Scenario: Dispatch issue 7, backdate its own claim past half-TTL, then fire with the worktree present while PATCH fails or a refresh read/PATCH hangs.
/// Every arm finishes with a clean worktree skip, preserves the old claim and running agent, and emits no dispatch or claim-failure notification.
#[spec("scheduler/dispatch/040")]
#[test]
fn dispatch_040_failed_refresh_preserves_clean_worktree_skip() {
    for (kind, hanging) in [
        ("PATCH", false),
        ("PATCH", true),
        ("list", true),
        ("user", true),
        ("events", true),
    ] {
        let (stub, _directory, work) = claim_fixture();
        stub.set_issues("acme/widgets", &[7, 8]);
        stub.set_open_pr("acme/widgets", 8);
        let daemon = common::spawn_daemon_serve_with_env(
            Some(&claim_task(&work, Some(true), 168, "0 0 * * *")),
            "0",
            &[
                ("PATH", &stub.path_env()),
                ("GHSTUB_DIR", &stub.ghstub_dir()),
                ("DOT_AGENT_DECK_ISSUE_CLAIM_TIMEOUT_MS", "5000"),
                ("DOT_AGENT_DECK_ISSUE_CLAIM_BUDGET_MS", "2000"),
            ],
        );
        let completed = || {
            scheduler_messages(&daemon)
                .iter()
                .filter(|line| line.contains("#8") && line.contains("skip"))
                .count()
        };
        daemon.run_now("dispatch-task").unwrap();
        wait_for_claim_dispatch(&daemon, &stub, &work, 7);
        assert!(common::wait_until(w(), || completed() == 1));
        let mut state = stub.state("acme/widgets");
        state["comments"]["7"][0]["updated_at"] = (chrono::Utc::now()
            - chrono::Duration::hours(85))
        .to_rfc3339()
        .into();
        state["events"]["7"][0]["created_at"] = (chrono::Utc::now() - chrono::Duration::hours(90))
            .to_rfc3339()
            .into();
        std::fs::write(
            stub.repo_dir("acme/widgets").join("state.json"),
            state.to_string(),
        )
        .unwrap();
        let records = stub.marker_comments("acme/widgets", 7);
        let before = stub.calls().len();
        if hanging {
            let directory = if kind == "user" {
                stub.dir.clone()
            } else {
                stub.repo_dir("acme/widgets")
            };
            std::fs::write(directory.join(format!("hang-{kind}")), "").unwrap();
        } else {
            stub.fail_first_calls("acme/widgets", kind, 1);
        }
        daemon.run_now("dispatch-task").unwrap();
        assert!(
            common::wait_until(Duration::from_secs(15), || completed() == 2),
            "refresh failure must not hold the worktree skip; kind={kind}; messages={:?}; calls={:?}",
            scheduler_messages(&daemon),
            stub.calls()
        );
        let messages = scheduler_messages(&daemon);
        assert_eq!(
            messages
                .iter()
                .filter(|line| line.contains("#7")
                    && line.contains("worktree")
                    && line.contains("skip"))
                .count(),
            1,
            "refresh {kind} must leave a clean worktree skip: {messages:?}"
        );
        assert!(!daemon.stderr_contains("issue #7 of acme/widgets failed:"));
        assert!(
            claim_failure_messages(&daemon).is_empty(),
            "refresh failure is not a failed dispatch claim: {messages:?}"
        );
        assert_eq!(stub.marker_comments("acme/widgets", 7), records);
        assert_eq!(count_orchestrators(&daemon), 1);
        assert!(
            derive_issue_paths(&work, "dispatch-task", 7)
                .worktree_dir
                .is_dir()
        );
        let calls = stub.calls();
        let claims: Vec<_> = calls[before..]
            .iter()
            .filter(|call| call[0] == "api" || is_claim_write(call))
            .collect();
        let expected = match kind {
            "list" => 1,
            "user" => 2,
            "events" => 3,
            "PATCH" => 4,
            _ => unreachable!(),
        };
        assert_eq!(
            claims.len(),
            expected,
            "refresh stops at the failed/timed-out step: {claims:?}"
        );
        stub.assert_no_unhandled_calls();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Scenario: Fire a two-issue dispatch against an unlabelled repo, a repo whose token cannot provision an existing label, and a missing label whose first provisioning attempt fails transiently.
/// Both issue worktrees and agents appear with their prompts, and both issues receive the claimed label.
/// Provisioning happens once unless the label is missing after a transient failure, when one retry recovers it.
#[spec("scheduler/dispatch/001")]
#[test]
fn dispatch_001_clone_worktree_spawn() {
    let mut provision_counts = Vec::new();
    for (case, expected_provisions) in [("missing", 1), ("denied", 1), ("transient", 2)] {
        let stub = GhStub::new();
        let repo = "acme/widgets";
        stub.add_repo(repo, true);
        stub.set_issues(repo, &[7, 8]);
        if case == "denied" {
            stub.deny_provision_existing_label(repo);
        } else if case == "transient" {
            stub.fail_first_calls(repo, "label", 1);
        }

        let work_td = common::harness_tempdir().expect("workspace tempdir");
        let work = work_td.path().join("ws");
        std::fs::create_dir_all(&work).expect("create workspace root");
        let work_str = work.to_string_lossy().into_owned();

        let toml = dispatch_task(
            "dispatch-task",
            &work_str,
            "ISSUEDISPATCH-{{issue_number}}",
            repo,
            5,
        );
        let path = stub.path_env();
        let ghdir = stub.ghstub_dir();
        let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
        let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

        daemon
            .run_now("dispatch-task")
            .expect("run-now dispatch-task");

        let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);

        assert!(
            common::wait_for_path(&paths.worktree_dir, w()),
            "firing an issue_dispatch task must create the per-issue worktree at {:?}",
            paths.worktree_dir
        );
        assert!(
            paths.clone_dir.is_dir(),
            "the repo must be cloned to {:?}",
            paths.clone_dir
        );
        assert!(
            common::wait_until(common::load_scaled(Duration::from_secs(5)), || {
                git_branch_exists(&paths.clone_dir, &paths.branch)
            }),
            "the per-issue branch {} must exist in the clone",
            paths.branch
        );

        let agent = daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .unwrap_or_else(|| {
                panic!(
                    "dispatch must spawn an orchestrator-role agent into {:?}",
                    paths.worktree_dir
                )
            });
        assert!(
            view::wait_for(
                &daemon.attach_socket,
                agent.pane_id_env.as_deref().expect("the agent's pane id"),
                "ISSUEDISPATCH-7",
                w(),
            ),
            "the substituted per-issue prompt must be delivered to the dispatched orchestrator"
        );
        let sibling_paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 8);
        let sibling = daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &sibling_paths.worktree_dir), w())
            .expect("the second issue must spawn an orchestrator too");
        assert!(view::wait_for(
            &daemon.attach_socket,
            sibling.pane_id_env.as_deref().unwrap(),
            "ISSUEDISPATCH-8",
            w(),
        ));
        assert!(daemon.wait_for_stderr_contains("dispatched issue #8", w()));
        assert_eq!(
            stub.state(repo)["issue_labels"]["7"],
            serde_json::json!(["agent-deck/claimed"]),
            "successful dispatch must persist agent-deck/claimed on issue #7; case={case}; notifications={:?}; gh calls: {:?}",
            scheduler_messages(&daemon),
            stub.calls()
        );
        assert!(
            stub.state(repo)["issue_labels"]["8"] == serde_json::json!(["agent-deck/claimed"])
                && !stub.marker_comments(repo, 8).is_empty(),
            "both issues must be labelled after provisioning failure; case={case}; calls: {:?}",
            stub.calls()
        );
        let calls = stub.calls();
        let provisions: Vec<_> = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.starts_with(&["label".into(), "create".into()]))
            .collect();
        provision_counts.push((case, provisions.len()));
        assert_eq!(
            provisions.len(),
            expected_provisions,
            "retry provisioning only while the label is missing; case={case}; calls: {calls:?}"
        );
        assert!(!provisions.is_empty(), "attempt just-in-time provisioning");
        assert!(provisions[0].1.iter().any(|arg| arg == "--force"));
        let claim = calls
            .iter()
            .position(|call| call.starts_with(&["issue".into(), "edit".into()]))
            .expect("issue label write");
        assert!(provisions[0].0 < claim, "provision before adding the label");
        stub.assert_no_unhandled_calls();
    }
    assert_eq!(
        provision_counts,
        vec![("missing", 1), ("denied", 1), ("transient", 2)],
        "provision once per run, retrying only to recover a missing label"
    );
}

/// Scenario: Fire an `issue_dispatch` task twice (one open issue, no intervening
/// close). After the first fire the issue's worktree and one orchestrator agent
/// exist; after the second fire the already-claimed issue is skipped — the
/// worktree and clone are still present, NO duplicate agent is spawned (the
/// orchestrator count stays at one), and a skip is surfaced through the notifier.
#[spec("scheduler/dispatch/002")]
#[test]
fn dispatch_002_idempotent_skip_existing_worktree() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);

    // First fire: dispatch issue 7.
    daemon.run_now("dispatch-task").expect("run-now (first)");
    assert!(
        common::wait_for_path(&paths.worktree_dir, w()),
        "the first fire must create the issue-7 worktree"
    );
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .is_some(),
        "the first fire must spawn an orchestrator agent for issue 7"
    );
    assert_eq!(
        count_orchestrators(&daemon),
        1,
        "exactly one dispatch after the first fire"
    );

    // Second fire: issue 7 is already claimed (its worktree exists) → skip.
    daemon.run_now("dispatch-task").expect("run-now (second)");

    // No duplicate spawn: the orchestrator count must NOT grow to two.
    let grew = common::wait_until(Duration::from_secs(5), || count_orchestrators(&daemon) > 1);
    assert!(
        !grew,
        "a second fire must NOT re-dispatch the already-claimed issue 7"
    );
    assert!(
        paths.worktree_dir.is_dir(),
        "the existing worktree must be preserved"
    );
    assert!(
        paths.clone_dir.is_dir(),
        "the clone must be preserved (no re-clone error)"
    );

    let surfaced = common::wait_until(common::load_scaled(Duration::from_secs(10)), || {
        daemon.stderr_contains("issue-7")
            || daemon.stderr_contains("issue 7")
            || daemon.stderr_contains("skip")
            || daemon.stderr_contains("Skip")
    });
    assert!(
        surfaced,
        "the skip of the already-claimed issue must be logged/surfaced"
    );
}

/// Scenario: With two open issues where `gh pr list` reports an open PR whose
/// head is `agent/issue-7` (and none for issue 8), fire the task. Issue 8
/// dispatches (its worktree exists, an orchestrator agent runs there), proving
/// the flow ran; issue 7 is skipped via the secondary idempotency signal — no
/// `issue-7` worktree is created and no agent is spawned for it.
#[spec("scheduler/dispatch/003")]
#[test]
fn dispatch_003_skip_open_pr() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7, 8]);
    stub.set_open_pr(repo, 7);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let p7 = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    let p8 = derive_issue_paths(Path::new(&work_str), "dispatch-task", 8);

    // Issue 8 (no PR) dispatches — proves the flow ran end to end.
    assert!(
        common::wait_for_path(&p8.worktree_dir, w()),
        "issue 8 (no open PR) must be dispatched: its worktree must exist"
    );
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &p8.worktree_dir), w())
            .is_some(),
        "issue 8 must spawn an orchestrator agent"
    );

    // Issue 7 is skipped by the open-PR secondary signal.
    assert!(
        !p7.worktree_dir.exists(),
        "issue 7 (open PR on agent/issue-7) must be skipped — no worktree created"
    );
    assert_eq!(
        count_orchestrators(&daemon),
        1,
        "only issue 8 should dispatch; issue 7 (open PR) is skipped"
    );
}

/// Scenario: Fire two `issue_dispatch` tasks whose resolved commands are bare
/// Codex — one from a cloned orchestration role and one from the single-agent
/// registry default. Both prompts must reach their panes, and PATH recorders must
/// prove both dispatch paths launched through the Codex Wrapper strategy.
#[spec("scheduler/dispatch/004")]
#[test]
fn dispatch_004_orchestration_vs_single_agent() {
    let stub = GhStub::new();
    let orch_repo = "acme/orch";
    let plain_repo = "acme/plain";
    let orch_fixture = stub.repo_dir(orch_repo);
    std::fs::create_dir_all(&orch_fixture).expect("create Codex orchestration fixture");
    init_remote_with_orch_toml(
        &orch_fixture.join("remote"),
        Some(
            "[[orchestrations]]\nname = \"dispatch-orch\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"codex\"\nstart = true\n",
        ),
    );
    stub.add_repo(plain_repo, false);
    stub.set_issues(orch_repo, &[11]);
    stub.set_issues(plain_repo, &[22]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    // A single-agent dispatch resolves its command from `default_command`.
    let cfg_td = common::harness_tempdir().expect("config tempdir");
    let cfg = cfg_td.path().join("config.toml");
    std::fs::write(&cfg, "default_command = \"codex\"\n").expect("write config.toml");
    let launch_record = cfg_td.path().join("dispatch-launch.log");
    let wrapper_stub = stub.bindir.join("dot-agent-deck");
    let codex_stub = stub.bindir.join("codex");
    std::fs::write(
        &wrapper_stub,
        "#!/bin/sh\nprintf 'WRAPPED %s\\n' \"$*\" >> \"$CODEX_DISPATCH_RECORD\"\nexec cat\n",
    )
    .expect("write dispatch wrapper recorder");
    std::fs::write(
        &codex_stub,
        "#!/bin/sh\nprintf 'BARE codex %s\\n' \"$*\" >> \"$CODEX_DISPATCH_RECORD\"\nexec cat\n",
    )
    .expect("write dispatch bare recorder");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for executable in [&wrapper_stub, &codex_stub] {
            std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755))
                .expect("chmod dispatch recorder");
        }
    }

    let mut toml = dispatch_task(
        "task-orch",
        &work_str,
        "ORCHDISP-{{issue_number}}",
        orch_repo,
        5,
    );
    toml.push_str(&dispatch_task(
        "task-plain",
        &work_str,
        "PLAINDISP-{{issue_number}}",
        plain_repo,
        5,
    ));

    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let cfg_str = cfg.to_string_lossy().into_owned();
    let record_str = launch_record.to_string_lossy().into_owned();
    let wrapper_stub_str = wrapper_stub.to_string_lossy().into_owned();
    let env: Vec<(&str, &str)> = vec![
        ("PATH", path.as_str()),
        ("GHSTUB_DIR", ghdir.as_str()),
        ("DOT_AGENT_DECK_CONFIG", cfg_str.as_str()),
        ("CODEX_DISPATCH_RECORD", record_str.as_str()),
        // The rewrite names the co-located build by absolute path now, so a
        // fake `dot-agent-deck` on PATH is no longer what the wrapper
        // resolves to. Point the rewrite at the recorder explicitly.
        ("DOT_AGENT_DECK_WRAP_BIN", wrapper_stub_str.as_str()),
    ];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon.run_now("task-orch").expect("run-now task-orch");
    daemon.run_now("task-plain").expect("run-now task-plain");

    let orch_wt = derive_issue_paths(Path::new(&work_str), "task-orch", 11).worktree_dir;
    let plain_wt = derive_issue_paths(Path::new(&work_str), "task-plain", 22).worktree_dir;

    // Orchestration clone → orchestration tab + prompt to the orchestrator role.
    assert!(
        common::wait_for_path(&orch_wt, w()),
        "orchestration issue worktree must exist"
    );
    let orch_agent = daemon
        .wait_for_agent_where(|r| orchestrator_in(r, &orch_wt), w())
        .expect("orchestration clone must open an orchestrator tab in the worktree");
    assert!(
        view::wait_for(
            &daemon.attach_socket,
            orch_agent
                .pane_id_env
                .as_deref()
                .expect("the agent's pane id"),
            "ORCHDISP-11",
            w(),
        ),
        "the prompt must reach the orchestrator role of the orchestration dispatch"
    );

    // Plain clone → single-agent card + prompt delivered.
    assert!(
        common::wait_for_path(&plain_wt, w()),
        "single-agent issue worktree must exist"
    );
    let plain_agent = daemon
        .wait_for_agent_where(|r| single_card_in(r, &plain_wt), w())
        .expect("plain clone must open a single-agent card in the worktree");
    assert!(
        view::wait_for(
            &daemon.attach_socket,
            plain_agent
                .pane_id_env
                .as_deref()
                .expect("the agent's pane id"),
            "PLAINDISP-22",
            w(),
        ),
        "the prompt must reach the single-agent card of the plain dispatch"
    );
    assert!(
        common::wait_for_file_substr_count(&launch_record, "WRAPPED", 2, w()),
        "both issue-dispatch Codex paths must reach a launch recorder"
    );
    // PRD #20 §4.2.1: the daemon's startup scoped-trust step runs `codex
    // app-server` to read Codex's own hook hashes, which this PATH recorder also
    // logs. It is a metadata probe, not an agent launch, so it is filtered out.
    let launches = common::recorded_agent_launches(&launch_record);
    assert_eq!(
        launches,
        vec![
            "WRAPPED wrap --agent codex -- codex",
            "WRAPPED wrap --agent codex -- codex",
        ],
        "issue-dispatch single-agent and role spawns must both cross the Wrapper strategy; observed:\n{}",
        launches.join("\n")
    );
}

/// Scenario: With a stub `gh` returning five open issues but `max_per_run = 2`,
/// fire the task. Only the first two issues (in returned order) get worktrees
/// and orchestrator spawns; the remaining three are left untouched (no
/// worktrees), and exactly two orchestrator agents exist.
#[spec("scheduler/dispatch/005")]
#[test]
fn dispatch_005_respects_max_per_run() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[1, 2, 3, 4, 5]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        2,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let pd = |n: u64| derive_issue_paths(Path::new(&work_str), "dispatch-task", n);

    // The first two issues are dispatched — the worktree AND the orchestrator
    // agent, waited for separately, because they are two steps of the dispatch
    // and only the first has landed when the directory appears. The count
    // below is a snapshot with no wait of its own, so without this it reads
    // the registry mid-spawn and sees one orchestrator where two are coming:
    // measured on a loaded box, `left: 1, right: 2`. Every sibling test in
    // this file already pairs the two waits this way (`dispatch_003`,
    // `dispatch_007`, `dispatch_008`); this one was the exception.
    for n in [1u64, 2] {
        let paths = pd(n);
        assert!(
            common::wait_for_path(&paths.worktree_dir, w()),
            "issue {n} must be dispatched"
        );
        assert!(
            daemon
                .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
                .is_some(),
            "issue {n} must spawn an orchestrator agent before the cap is counted"
        );
    }

    // The remaining three are left untouched.
    for n in [3u64, 4, 5] {
        assert!(
            !pd(n).worktree_dir.exists(),
            "issue {n} is past max_per_run=2 and must NOT be dispatched"
        );
    }

    assert_eq!(
        count_orchestrators(&daemon),
        2,
        "max_per_run=2 must cap the run at two per-issue spawns"
    );
}

/// Scenario: Dispatch an issue and close its tab through StopAgent on the orchestrator.
/// Its worktree disappears from disk and git's registry while the clone remains, and close makes no GitHub release calls or changes to the claim state.
#[spec("scheduler/dispatch/006")]
#[test]
fn dispatch_006_close_removes_worktree_preserves_clone() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    assert!(
        common::wait_for_path(&paths.worktree_dir, w()),
        "the issue worktree must exist before close"
    );
    let agent = daemon
        .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
        .expect("dispatch must spawn the orchestrator before close");

    assert!(daemon.wait_for_stderr_contains("dispatched issue #7", w()));
    let calls_before_close = stub.calls();
    let claim_before_close = stub.state(repo);

    // Close the dispatched tab.
    daemon
        .send_attach_request(&AttachRequest::StopAgent {
            id: agent.id.clone(),
        })
        .expect("StopAgent over the attach socket");

    // Cleanup removes the worktree (disk + git registry) but preserves the clone.
    let removed = common::wait_until(w(), || {
        !paths.worktree_dir.exists() && !git_worktree_listed(&paths.clone_dir, &paths.worktree_dir)
    });
    assert!(
        removed,
        "closing the dispatched tab must remove the worktree from disk and `git worktree list`"
    );
    assert!(
        paths.clone_dir.is_dir(),
        "the clone directory must be preserved after worktree cleanup"
    );
    assert!(
        !common::wait_until(Duration::from_secs(2), || stub.calls()
            != calls_before_close),
        "closing a dispatched tab must make no GitHub calls, including label removal, comment DELETE, or another release: {:?}",
        stub.calls()
    );
    assert_eq!(
        stub.state(repo),
        claim_before_close,
        "close preserves the GitHub claim"
    );
    stub.assert_no_unhandled_calls();
}

/// Scenario: With two open issues where `gh pr list --head agent/issue-11` errors
/// (a simulated per-issue GitHub failure) while issue 10 is healthy, fire the
/// task. Issue 10 still dispatches (its worktree exists, one orchestrator runs),
/// issue 11 does not (no worktree), and the failure is surfaced through the
/// notifier rather than swallowed — proving one issue's failure does not abort
/// the rest of the run.
#[spec("scheduler/dispatch/007")]
#[test]
fn dispatch_007_one_issue_fails_others_dispatch() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[10, 11]);
    stub.fail_pr(repo, 11);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let p10 = derive_issue_paths(Path::new(&work_str), "dispatch-task", 10);
    let p11 = derive_issue_paths(Path::new(&work_str), "dispatch-task", 11);

    // The healthy issue still dispatches.
    assert!(
        common::wait_for_path(&p10.worktree_dir, w()),
        "issue 10 must still dispatch despite issue 11 failing"
    );
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &p10.worktree_dir), w())
            .is_some(),
        "issue 10 must spawn an orchestrator agent"
    );

    // The failing issue does not.
    assert!(
        !p11.worktree_dir.exists(),
        "the failing issue 11 must NOT produce a worktree"
    );
    assert_eq!(
        count_orchestrators(&daemon),
        1,
        "only the healthy issue 10 should dispatch"
    );

    // The failure is surfaced, not swallowed.
    let surfaced = common::wait_until(common::load_scaled(Duration::from_secs(10)), || {
        daemon.stderr_contains("issue-11")
            || daemon.stderr_contains("issue 11")
            || daemon.stderr_contains("#11")
    });
    assert!(
        surfaced,
        "the per-issue failure for issue 11 must be surfaced through the notifier"
    );
}

/// Scenario: Copy a genuine claim from a separate issue into issue 7 as a comment authored by another GitHub account, then dispatch issue 7.
/// The deck leaves the forged record untouched and POSTs its own authenticated record.
/// Close issue 7's agent and fire again: the worktree and agent return, and only the deck-owned record is PATCHed with a changed body and a later update time.
#[spec("scheduler/dispatch/008")]
#[test]
fn dispatch_008_refire_after_close_redispatches() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_authenticated_user(54321, "claim-owner");
    stub.set_issues(repo, &[6]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    // Learn the exact host field from this daemon's genuine record, so the
    // forgery remains byte-for-byte valid when its digest representation changes.
    daemon.run_now("dispatch-task").expect("write source claim");
    assert!(common::wait_until(w(), || {
        stub.marker_comments(repo, 6).len() == 1
    }));
    assert!(daemon.wait_for_stderr_contains("dispatched issue #6", w()));
    let mut forged = stub.marker_comments(repo, 6).remove(0);
    assert_eq!(forged["user"]["id"], 54321);
    assert!(forged["body"].as_str().unwrap().starts_with(CLAIM_MARKER));
    assert!(forged["body"].as_str().unwrap().contains("dispatch-task"));
    forged["user"] = serde_json::json!({"id": 99999, "login": "forger"});
    let forged_id = stub.seed_comment(repo, 7, forged);
    let forged_record = stub.marker_comments(repo, 7).remove(0);
    stub.set_issues(repo, &[7]);
    let own_records = || {
        stub.marker_comments(repo, 7)
            .into_iter()
            .filter(|comment| comment["user"]["id"] == 54321)
            .collect::<Vec<_>>()
    };

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);

    // First fire: dispatch issue 7.
    daemon.run_now("dispatch-task").expect("run-now (first)");
    assert!(
        common::wait_for_path(&paths.worktree_dir, w()),
        "the first fire must create the issue-7 worktree"
    );
    let agent = daemon
        .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
        .expect("the first fire must spawn the orchestrator for issue 7");

    assert!(common::wait_until(w(), || {
        comment_writes(&stub, "POST") + comment_writes(&stub, "PATCH") > 1
    }));
    let forged_endpoint = format!("repos/{repo}/issues/comments/{forged_id}");
    assert!(
        !stub.calls().iter().any(|call| {
            call.starts_with(&["api".into(), "--method".into(), "PATCH".into()])
                && call.get(3) == Some(&forged_endpoint)
        }),
        "a byte-copy claim authored by another user must never be PATCHed; gh calls: {:?}",
        stub.calls()
    );
    assert!(
        common::wait_until(w(), || own_records().len() == 1),
        "first dispatch must POST its own authenticated marker comment beside the forgery; gh calls: {:?}",
        stub.calls()
    );
    let first_record = own_records().remove(0);
    assert_eq!(stub.marker_comments(repo, 7).len(), 2);
    assert!(
        first_record["body"]
            .as_str()
            .unwrap()
            .contains("dispatch-task")
    );
    assert_eq!(
        comment_writes(&stub, "POST"),
        2,
        "source claim and issue-7 claim each POST their own record once"
    );
    assert_eq!(comment_writes(&stub, "PATCH"), 0);

    // Close the dispatched tab. Cleanup removes the worktree but `git worktree
    // remove --force` LEAVES the branch `agent/issue-7` behind.
    daemon
        .send_attach_request(&AttachRequest::StopAgent {
            id: agent.id.clone(),
        })
        .expect("StopAgent over the attach socket");
    assert!(
        common::wait_until(w(), || !paths.worktree_dir.exists()),
        "closing the dispatched tab must remove the worktree (precondition for the re-fire)"
    );
    // Precondition for B1: the branch survives the close, so a naive
    // `worktree add -b agent/issue-7` on the next fire would collide.
    assert!(
        common::wait_until(common::load_scaled(Duration::from_secs(5)), || {
            git_branch_exists(&paths.clone_dir, &paths.branch)
        }),
        "the close preserves branch {} (the leftover that makes the re-fire trip)",
        paths.branch
    );

    // Second fire: issue still open, no PR, worktree gone → must re-dispatch.
    daemon.run_now("dispatch-task").expect("run-now (second)");

    // B1 fixed: `create_worktree` tolerates the pre-existing `agent/issue-7`
    // branch (reusing it rather than `-b`-creating it), so the second fire
    // re-creates the worktree and re-dispatches instead of surfacing an
    // `IssueDispatchFailed`.
    assert!(
        common::wait_for_path(&paths.worktree_dir, w()),
        "re-firing must re-create the issue-7 worktree (B1: worktree-add must tolerate the existing branch)"
    );
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .is_some(),
        "re-firing must spawn the orchestrator again for the reclaimed issue 7"
    );
    assert!(
        common::wait_until(w(), || {
            let records = own_records();
            comment_writes(&stub, "PATCH") == 1
                && records.len() == 1
                && records[0]["updated_at"].as_str().unwrap()
                    > first_record["updated_at"].as_str().unwrap()
                && records[0]["body"] != first_record["body"]
        }),
        "re-claim must PATCH a changed body and advance updated_at on the existing record; first record: {first_record:?}; current records: {:?}; gh calls: {:?}",
        own_records(),
        stub.calls()
    );
    let records = own_records();
    assert_eq!(
        records.len(),
        1,
        "re-claim leaves exactly one deck-owned marker comment"
    );
    assert_eq!(
        records[0]["id"], first_record["id"],
        "edit the original record"
    );
    assert!(
        records[0]["body"]
            .as_str()
            .unwrap()
            .contains("dispatch-task")
    );
    assert_eq!(
        comment_writes(&stub, "POST"),
        2,
        "re-claim must not POST again"
    );
    assert_eq!(
        stub.marker_comments(repo, 7)
            .into_iter()
            .find(|comment| comment["id"] == forged_id)
            .unwrap(),
        forged_record,
        "the foreign record remains unchanged across dispatch and re-claim"
    );
    let own_endpoint = format!("repos/{repo}/issues/comments/{}", first_record["id"]);
    assert!(
        stub.calls()
            .iter()
            .filter(|call| { call.starts_with(&["api".into(), "--method".into(), "PATCH".into()]) })
            .all(|call| call.get(3) == Some(&own_endpoint))
    );
    stub.assert_no_unhandled_calls();
    // The re-dispatch must not surface a per-issue failure for issue 7.
    let failed = common::wait_until(Duration::from_secs(3), || {
        daemon.stderr_contains("failed:") || daemon.stderr_contains("already exists")
    });
    assert!(
        !failed,
        "re-dispatch must NOT surface an IssueDispatchFailed for issue 7"
    );
}

/// Scenario: Dispatch one open issue whose cloned repo carries a TWO-role
/// orchestration (orchestrator + reviewer), so the issue worktree hosts two role
/// panes sharing one `orchestration_cwd`. Closing ONE role pane must leave the
/// worktree on disk (a sibling role is still live in it); only closing the LAST
/// role pane removes the worktree — and the clone is preserved (S1: refcount the
/// worktree, removing it only when the last rooted agent closes).
#[spec("scheduler/dispatch/009")]
#[test]
fn dispatch_009_multirole_orchestration_cleanup_refcount() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo_multirole(repo);
    stub.set_issues(repo, &[7]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    let wt = paths.worktree_dir.clone();
    assert!(
        common::wait_for_path(&wt, w()),
        "dispatch must create the issue worktree"
    );

    // Both role panes spawn into the SAME issue worktree.
    let orchestrator = daemon
        .wait_for_agent_where(|r| role_in(r, "orchestrator", &wt), w())
        .expect("the orchestrator role must spawn into the issue worktree");
    let reviewer = daemon
        .wait_for_agent_where(|r| role_in(r, "reviewer", &wt), w())
        .expect("the reviewer role must spawn into the same issue worktree");

    // Close ONE role (the reviewer). The worktree must SURVIVE — the
    // orchestrator role is still live in it.
    daemon
        .send_attach_request(&AttachRequest::StopAgent {
            id: reviewer.id.clone(),
        })
        .expect("StopAgent reviewer over the attach socket");

    // S1 fixed: the shared worktree is refcounted, so the first role-pane close
    // does not run `git worktree remove --force` while the orchestrator role is
    // still live in it. Give the async cleanup time to (wrongly) fire, then
    // assert the worktree is still present on disk and in `git worktree list`.
    let nuked_early = common::wait_until(Duration::from_secs(5), || {
        !wt.exists() || !git_worktree_listed(&paths.clone_dir, &wt)
    });
    assert!(
        !nuked_early,
        "closing ONE role of a multi-role orchestration must NOT remove the shared worktree (S1: refcount — remove only on the LAST close)"
    );

    // Close the LAST role (the orchestrator). Now the worktree must be removed,
    // while the clone is preserved.
    daemon
        .send_attach_request(&AttachRequest::StopAgent {
            id: orchestrator.id.clone(),
        })
        .expect("StopAgent orchestrator over the attach socket");
    let removed = common::wait_until(w(), || {
        !wt.exists() && !git_worktree_listed(&paths.clone_dir, &wt)
    });
    assert!(
        removed,
        "closing the LAST role must remove the shared worktree from disk and `git worktree list`"
    );
    assert!(
        paths.clone_dir.is_dir(),
        "the clone directory must be preserved after worktree cleanup"
    );
}

/// Scenario: Dispatch an issue, arm a failing open-PR check, and fire again with its worktree still present.
/// The issue is skipped without a failure or duplicate spawn, its clone and worktree remain, and this nothing-to-dispatch fire makes no label-provisioning or claim-write calls.
#[spec("scheduler/dispatch/012")]
#[test]
fn dispatch_012_worktree_present_skips_without_pr_check() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);

    // First fire (no fail marker yet): issue 7 dispatches normally, so its
    // worktree and one orchestrator exist — the precondition for the short-circuit.
    daemon.run_now("dispatch-task").expect("run-now (first)");
    assert!(
        common::wait_for_path(&paths.worktree_dir, w()),
        "the first fire must create the issue-7 worktree"
    );
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .is_some(),
        "the first fire must spawn an orchestrator agent for issue 7"
    );
    assert_eq!(
        count_orchestrators(&daemon),
        1,
        "exactly one dispatch after the first fire"
    );

    // Arm the hazard: from now on `gh pr list --head agent/issue-7` exits
    // non-zero. With the OLD code this transient error — reached because the PR
    // check ran unconditionally — turned the worktree-present SKIP into a
    // spurious IssueDispatchFailed. The fix short-circuits before the check.
    stub.fail_pr(repo, 7);

    assert!(daemon.wait_for_stderr_contains("dispatched issue #7", w()));
    let writes_before_refire = stub
        .calls()
        .iter()
        .filter(|call| is_claim_write(call))
        .count();

    // Second fire: the worktree already exists → primary signal → SKIP without
    // ever calling `issue_has_open_pr`.
    daemon.run_now("dispatch-task").expect("run-now (second)");

    // No duplicate spawn / re-creation: the orchestrator count must NOT grow.
    let grew = common::wait_until(Duration::from_secs(5), || count_orchestrators(&daemon) > 1);
    assert!(
        !grew,
        "a worktree-present second fire must NOT re-dispatch or re-spawn issue 7"
    );

    // The present worktree must short-circuit to an IssueDispatchSkipped notice.
    // (With the OLD code the PR-check error fires first and no skip is surfaced,
    // so this wait would time out — making the test RED against the regression.)
    // The skip line is upstream's, `skipping already-claimed issue #N of R (B):
    // <cause>`, and it names its cause ("its worktree exists" for this one):
    // PRD #421 M1.3's per-cause wording on upstream's line (sync ruling 44),
    // replacing the fork's earlier `skipping issue #N of R (B) -- <reason>`.
    let skipped = common::wait_until(common::load_scaled(Duration::from_secs(10)), || {
        daemon.stderr_contains("skipping already-claimed issue #7 of acme/widgets")
            && daemon.stderr_contains("its worktree exists")
    });
    assert!(
        skipped,
        "the present worktree must short-circuit to a SKIP (IssueDispatchSkipped) for issue 7"
    );

    // ...and because the PR check is never reached, the simulated `gh pr list`
    // error must NEVER surface as a per-issue failure for issue 7.
    assert!(
        !daemon.stderr_contains("issue #7 of acme/widgets failed"),
        "the worktree-present skip must short-circuit before issue_has_open_pr, so the simulated gh pr list error must NOT surface as IssueDispatchFailed"
    );

    // The existing worktree and clone are preserved (no re-creation, no re-clone).
    assert!(
        paths.worktree_dir.is_dir(),
        "the existing issue-7 worktree must be preserved"
    );
    assert!(
        paths.clone_dir.is_dir(),
        "the clone must be preserved (no re-clone)"
    );
    assert_eq!(
        stub.calls()
            .iter()
            .filter(|call| is_claim_write(call))
            .count(),
        writes_before_refire,
        "a fire dispatching nothing must not provision labels or write claims; calls: {:?}",
        stub.calls()
    );
    stub.assert_no_unhandled_calls();
}

/// Scenario: Fire issue dispatch against a repo whose token rejects every GitHub write, then a two-issue run whose first authenticated-account lookup fails transiently.
/// Agents still receive their prompts, and each failed claim produces one notification naming both outcomes separately from dispatch success or failure.
/// After the transient lookup failure, the next issue receives a claimant record owned by the authenticated account.
#[spec("scheduler/dispatch/029")]
#[test]
fn dispatch_029_read_only_claim_failure_keeps_agent_running() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7]);
    stub.read_only(repo);
    let initial_state = stub.state(repo);
    let work = common::harness_tempdir().expect("workspace");
    let task = dispatch_task(
        "dispatch-task",
        &work.path().to_string_lossy(),
        "READONLY-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let daemon = common::spawn_daemon_serve_with_env(
        Some(&task),
        "0",
        &[("PATH", &path), ("GHSTUB_DIR", &ghdir)],
    );
    daemon
        .run_now("dispatch-task")
        .expect("fire read-only dispatch");
    let paths = derive_issue_paths(work.path(), "dispatch-task", 7);
    let agent = daemon
        .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
        .expect("read-only GitHub must not prevent the agent spawn");
    assert!(
        view::wait_for(
            &daemon.attach_socket,
            agent.pane_id_env.as_deref().unwrap(),
            "READONLY-7",
            w()
        ),
        "read-only dispatch delivers its prompt"
    );
    assert!(
        common::wait_until(w(), || !claim_failure_messages(&daemon).is_empty()),
        "read-only claim must raise IssueClaimFailed separately from dispatch notifications; notifications: {:?}; gh calls: {:?}",
        scheduler_messages(&daemon),
        stub.calls()
    );
    assert!(daemon.wait_for_stderr_contains("dispatched issue #7", w()));
    assert!(
        !common::wait_until(Duration::from_secs(2), || claim_failure_messages(&daemon)
            .len()
            > 1),
        "one failed claim raises exactly one notification"
    );
    let messages = claim_failure_messages(&daemon);
    assert_eq!(messages.len(), 1);
    let message = messages[0].to_lowercase();
    assert!(
        message.contains("label") && message.contains("record"),
        "IssueClaimFailed names both outcomes: {messages:?}"
    );
    assert!(message.contains("dispatch-task") && message.contains(repo));
    assert!(
        !daemon.stderr_contains("issue #7 of acme/widgets failed:"),
        "claim failure is not IssueDispatchFailed"
    );
    assert_eq!(
        stub.state(repo),
        initial_state,
        "permission denial mutates no GitHub state"
    );
    assert_eq!(
        comment_writes(&stub, "POST"),
        1,
        "record creation is attempted even if label provisioning fails"
    );
    stub.assert_no_unhandled_calls();
    drop(daemon);

    let stub = GhStub::new();
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7, 8]);
    stub.set_authenticated_user(54321, "recovered-claim-owner");
    stub.fail_first_calls(repo, "user", 1);
    let work = common::harness_tempdir().expect("lookup-recovery workspace");
    let task = dispatch_task(
        "dispatch-task",
        &work.path().to_string_lossy(),
        "LOOKUPRECOVERY-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let daemon = common::spawn_daemon_serve_with_env(
        Some(&task),
        "0",
        &[("PATH", &path), ("GHSTUB_DIR", &ghdir)],
    );
    daemon
        .run_now("dispatch-task")
        .expect("fire lookup recovery");
    assert!(daemon.wait_for_stderr_contains("dispatched issue #8", w()));
    for issue in [7, 8] {
        let paths = derive_issue_paths(work.path(), "dispatch-task", issue);
        let agent = daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .expect("lookup failure must not prevent either agent from spawning");
        assert!(view::wait_for(
            &daemon.attach_socket,
            agent.pane_id_env.as_deref().unwrap(),
            &format!("LOOKUPRECOVERY-{issue}"),
            w(),
        ));
    }
    let messages = claim_failure_messages(&daemon);
    assert_eq!(messages.len(), 1, "first lookup failure: {messages:?}");
    assert!(
        messages[0].to_lowercase().contains("record") && messages[0].contains("502"),
        "the first issue's claim-failure notification names its failed record outcome: {messages:?}"
    );
    assert!(stub.marker_comments(repo, 7).is_empty());
    let records = stub.marker_comments(repo, 8);
    assert_eq!(
        records.len(),
        1,
        "a failed account lookup must not be replayed for issue 8: it needs a claimant record; notifications={:?}; gh calls={:?}",
        scheduler_messages(&daemon),
        stub.calls()
    );
    assert_eq!(records[0]["user"]["id"], 54321);
    assert!(
        records[0]["body"]
            .as_str()
            .unwrap()
            .contains("dispatch-task")
    );
    assert_eq!(comment_writes(&stub, "POST"), 1);
    assert!(claim_failure_messages_for(&daemon, 8).is_empty());
    assert_eq!(
        stub.calls()
            .iter()
            .filter(|call| call.as_slice() == ["api", "--include", "user"])
            .count(),
        2,
        "the second issue retries the failed authenticated-account lookup"
    );
    stub.assert_no_unhandled_calls();
}

/// Scenario: Fire issue dispatch against an orchestration whose start role names a missing executable.
/// The issue worktree is prepared but spawning fails visibly, no agent runs there, and GitHub receives no label provisioning, issue edit or comment write.
#[spec("scheduler/dispatch/030")]
#[test]
fn dispatch_030_spawn_failure_writes_no_claim() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, false);
    let remote = stub.repo_dir(repo).join("remote");
    std::fs::write(
        remote.join(".dot-agent-deck.toml"),
        ORCH_TOML.replace(
            "command = \"cat\"",
            "command = \"/missing-dispatch-agent-421\"",
        ),
    )
    .expect("seed missing agent command");
    run_git(&remote, &["add", ".dot-agent-deck.toml"]);
    run_git(&remote, &["commit", "-q", "-m", "unavailable agent"]);
    stub.set_issues(repo, &[7]);
    let work = common::harness_tempdir().expect("workspace");
    let task = dispatch_task(
        "dispatch-task",
        &work.path().to_string_lossy(),
        "SPAWNFAIL-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let daemon = common::spawn_daemon_serve_with_env(
        Some(&task),
        "0",
        &[("PATH", &path), ("GHSTUB_DIR", &ghdir)],
    );
    daemon.run_now("dispatch-task").expect("fire failing spawn");
    assert!(
        daemon.wait_for_stderr_contains("issue #7 of acme/widgets failed:", w()),
        "spawn failure must be surfaced: {:?}",
        scheduler_messages(&daemon)
    );
    let paths = derive_issue_paths(work.path(), "dispatch-task", 7);
    assert!(
        paths.worktree_dir.is_dir(),
        "spawn was attempted after preparing the issue worktree"
    );
    assert!(
        !daemon
            .agent_records()
            .iter()
            .any(|r| orchestrator_in(r, &paths.worktree_dir)),
        "no agent spawned for the failed issue"
    );
    assert!(
        !common::wait_until(Duration::from_secs(2), || stub
            .calls()
            .iter()
            .any(|call| is_claim_write(call))),
        "spawn failure must make no label create, issue edit or comment write: {:?}",
        stub.calls()
    );
    stub.assert_no_unhandled_calls();
}

/// Scenario: Dispatch two issues with a hanging first label write and a 1500 ms call timeout, then three issues with every claim call hanging, a 5000 ms call timeout and a 3000 ms shared claim budget.
/// The healthy sibling in the first run gets its prompt and claim, while the second run finishes dispatching three agents within 15 seconds.
/// Each failed claim raises one notification naming both outcomes, and later issues in the second run say their claims were skipped for the run's time budget.
#[spec("scheduler/dispatch/031")]
#[test]
fn dispatch_031_hanging_claim_does_not_hold_queue() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7, 8]);
    stub.hang_issue_label(repo, 7);
    let work = common::harness_tempdir().expect("workspace");
    let task = dispatch_task(
        "dispatch-task",
        &work.path().to_string_lossy(),
        "TIMEOUT-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let daemon = common::spawn_daemon_serve_with_env(
        Some(&task),
        "0",
        &[
            ("PATH", &path),
            ("GHSTUB_DIR", &ghdir),
            ("DOT_AGENT_DECK_ISSUE_CLAIM_TIMEOUT_MS", "1500"),
        ],
    );
    daemon.run_now("dispatch-task").expect("fire two issues");
    let first = derive_issue_paths(work.path(), "dispatch-task", 7);
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &first.worktree_dir), w())
            .is_some(),
        "the first issue spawns before its hanging claim"
    );
    assert!(common::wait_until(w(), || {
        stub.calls().iter().any(|call| {
            call.starts_with(&["issue".into(), "edit".into()])
                && call.last().map(String::as_str) == Some("7")
        })
    }));
    let started = std::time::Instant::now();
    // This is a responsiveness contract, not a load-scaled eventual outcome:
    // 20 s leaves room for git/spawn while detecting the old 600 s gh bound.
    let ceiling = Duration::from_secs(20);
    let second = derive_issue_paths(work.path(), "dispatch-task", 8);
    let sibling = daemon
        .wait_for_agent_where(|r| orchestrator_in(r, &second.worktree_dir), ceiling)
        .unwrap_or_else(|| {
            panic!(
                "a hanging issue-7 claim must not hold issue 8 beyond 20 s; elapsed={:?}; notifications={:?}; gh calls={:?}",
                started.elapsed(),
                scheduler_messages(&daemon),
                stub.calls()
            )
        });
    assert!(started.elapsed() < ceiling);
    assert_eq!(count_orchestrators(&daemon), 2);
    assert!(view::wait_for(
        &daemon.attach_socket,
        sibling.pane_id_env.as_deref().unwrap(),
        "TIMEOUT-8",
        w(),
    ));
    assert!(common::wait_until(w(), || {
        stub.state(repo)["issue_labels"]["8"] == serde_json::json!(["agent-deck/claimed"])
            && stub.marker_comments(repo, 8).len() == 1
            && !claim_failure_messages(&daemon).is_empty()
    }));
    assert!(daemon.wait_for_stderr_contains("dispatched issue #8", w()));
    assert!(
        !common::wait_until(Duration::from_secs(2), || {
            claim_failure_messages(&daemon).len() > 1
        }),
        "a timed-out claim raises one notification, not one per step"
    );
    let messages = claim_failure_messages(&daemon);
    assert_eq!(messages.len(), 1);
    let message = messages[0].to_lowercase();
    assert!(
        message.contains("label") && (message.contains("timed out") || message.contains("timeout")),
        "IssueClaimFailed must name the timed-out label step: {messages:?}"
    );
    assert!(
        message.contains("record") && message.contains(repo) && message.contains("dispatch-task")
    );
    assert!(
        !daemon.stderr_contains("issue #7 of acme/widgets failed:"),
        "claim timeout must not become a dispatch failure"
    );
    stub.assert_no_unhandled_calls();
    drop(daemon);

    let stub = GhStub::new();
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7, 8, 9]);
    stub.hang_claim_calls();
    let work = common::harness_tempdir().expect("budget workspace");
    let task = dispatch_task(
        "dispatch-task",
        &work.path().to_string_lossy(),
        "CLAIMBUDGET-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let daemon = common::spawn_daemon_serve_with_env(
        Some(&task),
        "0",
        &[
            ("PATH", &path),
            ("GHSTUB_DIR", &ghdir),
            ("DOT_AGENT_DECK_ISSUE_CLAIM_TIMEOUT_MS", "5000"),
            ("DOT_AGENT_DECK_ISSUE_CLAIM_BUDGET_MS", "3000"),
        ],
    );
    let started = std::time::Instant::now();
    daemon.run_now("dispatch-task").expect("fire budget run");
    // Fixed responsiveness bound: the old call-by-call waits need at least
    // 25 s for this run, while a shared 3 s budget leaves room for git/spawn.
    let ceiling = Duration::from_secs(15);
    assert!(
        daemon.wait_for_stderr_contains("dispatched issue #9", ceiling),
        "three hanging claims must share the run's 3000 ms budget and finish dispatch within 15 s; elapsed={:?}; notifications={:?}; gh calls={:?}",
        started.elapsed(),
        scheduler_messages(&daemon),
        stub.calls()
    );
    assert!(started.elapsed() < ceiling);
    assert_eq!(count_orchestrators(&daemon), 3);
    for issue in [7, 8, 9] {
        let paths = derive_issue_paths(work.path(), "dispatch-task", issue);
        let agent = daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .expect("exhausting the claim budget must not skip dispatch");
        assert!(view::wait_for(
            &daemon.attach_socket,
            agent.pane_id_env.as_deref().unwrap(),
            &format!("CLAIMBUDGET-{issue}"),
            w(),
        ));
        let messages = claim_failure_messages_for(&daemon, issue);
        assert_eq!(
            messages.len(),
            1,
            "each issue gets exactly one claim-failure notice: issue={issue}; notifications={:?}",
            scheduler_messages(&daemon)
        );
        let message = messages[0].to_lowercase();
        assert!(message.contains("label") && message.contains("record"));
        if issue != 7 {
            assert!(
                message.contains("budget"),
                "later issues report a claim skipped for the shared time budget: {messages:?}"
            );
        }
        assert!(!daemon.stderr_contains(&format!("issue #{issue} of {repo} failed:")));
    }
    let claim_calls: Vec<_> = stub
        .calls()
        .into_iter()
        .filter(|call| {
            is_claim_write(call)
                || call.starts_with(&["api".into(), "--paginate".into()])
                || call.as_slice() == ["api", "--include", "user"]
        })
        .collect();
    assert_eq!(
        claim_calls.len(),
        1,
        "after the first hanging call exhausts the budget, no further claim-path call is made: {claim_calls:?}"
    );
    stub.assert_no_unhandled_calls();
}

// ---------------------------------------------------------------------------
// PRD #421 Phase 1 — the `in-progress` claim (RED tests)
// ---------------------------------------------------------------------------

/// A `.dot-agent-deck.toml` whose orchestration role names a binary that does
/// not exist on `PATH`, so `spawn`'s `spawn_command` fails synchronously on
/// exec resolution — a deterministic spawn failure with no timing/race
/// dependency (unlike a shell-wrapped command, a single bare word is exec'd
/// directly, so Rust's `std::process::Command::spawn()` surfaces the ENOENT
/// as an `Err` before this stub script or any shell is ever involved).
const FAILING_ORCH_TOML: &str = "[[orchestrations]]\nname = \"dispatch-orch\"\n\n\
     [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"dad-nonexistent-binary-421\"\nstart = true\n";

/// Scenario: Fire a SINGLE-AGENT `issue_dispatch` task (fixture remote
/// carries no `.dot-agent-deck.toml`, mirroring `scheduler/dispatch/045`) for
/// one open issue on a repo that already carries the `in-progress` label
/// (seeded via [`GhStub::seed_labels`], mirroring a repo where the label
/// vocabulary pre-exists — see `scheduler/dispatch/043` for the counterpart
/// where it doesn't). After the dispatch succeeds (worktree + single agent
/// card present, as in `scheduler/dispatch/045`), the stub `gh` log must show
/// an `issue edit ... --add-label in-progress` invocation for the issue AND
/// an `issue comment` invocation whose body names the claiming task
/// (the task name as DECORATION on the two-field worktree-path-plus-branch
/// identity, extracted from production's own rendering by its structural
/// backtick delimiters rather than by searching for surrounding prose
/// (fork#243 round 4) and asserted in full — this is the genuinely
/// single-agent claim path PRD #421 originally covered here, kept distinct
/// from the orchestration-named claim `scheduler/dispatch/044` exists to
/// pin) — and,
/// because `in-progress` pre-existed, the add-label call must have actually
/// SUCCEEDED, not merely been attempted.
#[spec("scheduler/dispatch/010")]
#[test]
fn dispatch_010_success_writes_label_and_claim_comment() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, false);
    stub.seed_labels(repo, &[IN_PROGRESS_LABEL]);
    stub.set_issues(repo, &[7]);

    let (_work_td, work_str, daemon) = spawn_dispatch_daemon(
        &stub,
        "dispatch-task",
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
    );
    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    assert!(
        daemon
            .wait_for_agent_where(|r| single_card_in(r, &paths.worktree_dir), w())
            .is_some(),
        "the dispatch must succeed (precondition for a label/comment write)"
    );

    let labelled = common::wait_until(Duration::from_secs(10), || {
        stub.gh_calls().iter().any(|l| {
            l.contains("issue")
                && l.contains("edit")
                && l.contains("--add-label")
                && l.contains("in-progress")
                && l.contains('7')
        })
    });
    assert!(
        labelled,
        "a successful dispatch must write the `in-progress` label via `gh issue edit --add-label in-progress`; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    let commented = common::wait_until(Duration::from_secs(5), || {
        stub.gh_calls()
            .iter()
            .any(|l| l.contains("issue") && l.contains("comment") && l.contains("dispatch-task"))
    });
    assert!(
        commented,
        "a successful dispatch must post a claim comment naming the claiming task (`dispatch-task`); observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    // PRD fork#235 round 3: the identity itself is the dispatched worktree's
    // absolute path plus its branch (`agent/issue-<n>`, CLAUDE.md rule 23),
    // not `issue-dispatch:<task>#<issue>@…` — the task name above is
    // DECORATION, not the compared identity string. `derive_issue_paths`
    // builds `paths.worktree_dir` lexically, while the identity rendered
    // into the comment is physically resolved (`canonicalize_identity_path`),
    // so `assert_claim_names_worktree_identity` builds the `Identity`
    // production would build and matches against production's OWN
    // `claim_comment_body` rendering (fork#243 round 3) — the delimited
    // fragment `` working `<path>` on branch `<branch>` ``, not two
    // independently `contains()`-checked substrings: a regression that
    // serialises the correct branch but omits the path, or pairs the branch
    // with a DIFFERENT canonical path (including one sharing the expected
    // path as a mere prefix), must still fail here.
    assert_claim_names_worktree_identity(
        &stub,
        &paths.worktree_dir,
        &paths.branch,
        "decorate it with the claiming task name",
    );

    // The fixture pre-seeded `in-progress`, so the add-label call must have
    // actually SUCCEEDED against the stub's real-`gh`-accurate label-name
    // resolution — not merely been attempted (`labelled` above only proves
    // the attempt, since the raw argv is logged before the stub decides
    // whether to accept or reject it).
    let applied = common::wait_until(Duration::from_secs(3), || {
        stub.label_applied(repo, 7, IN_PROGRESS_LABEL)
    });
    assert!(
        applied,
        "the `in-progress` label write must SUCCEED when the label already exists on the repo; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    // PRD fork#235 M2: the assignee is written alongside the label and
    // comment — replace-to-one, the human who owns the agent working the
    // issue right now. This is the ONLY new assertion added by fork#235; the
    // wording and assertions above are unchanged from PRD #421.
    let assigned = common::wait_until(Duration::from_secs(5), || {
        !stub.assignees(repo, 7).is_empty()
    });
    assert!(
        assigned,
        "a successful dispatch must also write the assignee alongside the label and comment (PRD fork#235 M2); observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );
}

/// Scenario: Fire an `issue_dispatch` task against a fixture repo whose
/// orchestration role names a nonexistent binary, so worktree creation
/// succeeds but the spawn step fails deterministically. Assert the run
/// surfaces an `IssueDispatchFailed` for the issue, and that the stub `gh`
/// log carries no `issue edit`/`issue comment` invocation for it — a failed
/// dispatch must leave the issue completely unmarked (PRD #421 risk
/// mitigation: a false claim on a failed dispatch would make the issue
/// permanently un-dispatchable once the label is read back as a signal).
#[spec("scheduler/dispatch/022")]
#[test]
fn dispatch_022_failed_spawn_leaves_issue_unmarked() {
    let stub = GhStub::new();
    let repo = "acme/failing";
    let fixture = stub.repo_dir(repo);
    std::fs::create_dir_all(&fixture).expect("create failing-spawn fixture dir");
    init_remote_with_orch_toml(&fixture.join("remote"), Some(FAILING_ORCH_TOML));
    stub.set_issues(repo, &[9]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 9);
    assert!(
        common::wait_for_path(&paths.worktree_dir, w()),
        "the worktree is created BEFORE the spawn attempt, so it must exist even though the spawn itself fails"
    );

    let failed = common::wait_until(Duration::from_secs(10), || {
        daemon.stderr_contains("issue #9") && daemon.stderr_contains("failed")
    });
    assert!(
        failed,
        "the nonexistent-binary orchestrator role must fail the spawn, surfaced as an IssueDispatchFailed for issue 9; observed stderr:\n{}",
        daemon.stderr_text()
    );

    let stray_calls: Vec<String> = stub
        .gh_calls()
        .into_iter()
        .filter(|l| l.contains("issue") && (l.contains("edit") || l.contains("comment")))
        .collect();
    assert!(
        stray_calls.is_empty(),
        "a FAILED dispatch must leave the issue completely unmarked — no `gh issue edit`/`gh issue comment` call may ever be made for it; observed gh calls: {stray_calls:?}"
    );
}

/// Scenario: Seed the stub so issue 7 already carries an `in-progress` label
/// (via [`GhStub::set_issues_with_labels`], covering both a plausible
/// list-embedded and a per-issue-view read mechanism) with NO worktree on
/// disk and NO open PR — the label is the only claim signal present. Fire the
/// task and assert the issue is SKIPPED (no worktree ever created), even
/// though today's `dispatch_decision` only consults the worktree/PR signals
/// and would otherwise dispatch it. RED until the label is read as a third
/// idempotency signal (PRD #421 M1.2).
#[spec("scheduler/dispatch/042")]
#[test]
fn dispatch_042_externally_labelled_issue_skips_with_no_other_signal() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues_with_labels(repo, &[(7, &["in-progress"])]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);

    // Give the (would-be, buggy) dispatch time to run, then assert it did
    // NOT happen. `wait_until` exits EARLY if the worktree appears (catching
    // the bug fast); it only waits out the full timeout when the worktree
    // correctly never appears.
    let dispatched = common::wait_until(Duration::from_secs(10), || paths.worktree_dir.exists());
    assert!(
        !dispatched,
        "an issue already labelled `in-progress` must be skipped even with no worktree and no open PR (M1.2 third signal), but the worktree was created at {:?}",
        paths.worktree_dir
    );
    assert_eq!(
        count_orchestrators(&daemon),
        0,
        "a labelled issue with no other claim signal must not spawn an orchestrator"
    );
}

/// Scenario: Seed issue 7 with an `in-progress` label but NO recorded claim
/// comment (simulating a human or an external tool applying the label
/// directly, never through this deck). Fire the task and assert the skip is
/// surfaced AND its rendered text specifically reports that no claimant was
/// recorded — distinguishing it from a skip whose claimant IS known. RED
/// today: the label isn't read at all, so the issue dispatches instead of
/// skipping, and no "no claimant" text is ever rendered (PRD #421 M1.2 +
/// claimant reporting).
#[spec("scheduler/dispatch/024")]
#[test]
fn dispatch_024_externally_labelled_issue_skip_reports_no_claimant() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues_with_labels(repo, &[(7, &["in-progress"])]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let skipped_no_claimant = common::wait_until(Duration::from_secs(10), || {
        daemon.stderr_contains("no claimant")
    });
    assert!(
        skipped_no_claimant,
        "an issue labelled `in-progress` by a human/external tool (no deck claim comment) must be \
         skipped, and the skip must report that no claimant was recorded; observed stderr:\n{}",
        daemon.stderr_text()
    );

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    assert!(
        !paths.worktree_dir.exists(),
        "the externally-labelled issue must not be dispatched"
    );
}

/// Strip the issue-number and branch substrings out of a rendered skip line,
/// leaving only the shared template — used by `dispatch_025` to prove two
/// lines are (or aren't) the SAME template independent of which issue they
/// name.
fn normalize_skip_line(line: &str, issue: u64, branch: &str) -> String {
    line.replace(&format!("issue #{issue}"), "issue #N")
        .replace(branch, "BRANCH")
}

/// Find the first line of `text` naming `issue #<issue>` that is NOT the
/// `IssueDispatched` success line — which ALSO contains that exact substring
/// (`"... dispatched issue #<issue> of <repo> ..."`), so a caller looking for
/// a SKIP/FAILURE line for a specific issue can't be fooled by a same-issue
/// dispatch-success line elsewhere in the captured stderr. This distinction
/// matters concretely: pre-fix, issue 33 (the label-only cause) dispatches
/// instead of skipping, so a naive substring search for `"issue #33"` finds
/// that dispatch line and wrongly reads as "a skip line for cause C exists".
fn find_non_dispatch_line_for(text: &str, issue: u64) -> Option<&str> {
    let needle = format!("issue #{issue}");
    text.lines()
        .find(|l| l.contains(&needle) && !l.contains("dispatched issue"))
}

/// Scenario: Drive THREE of PRD #421's four skip causes in one repo — issue 31
/// (worktree-exists, via a re-fire), issue 32 (an open PR), issue 33 (a
/// pre-seeded `in-progress` label, both other signals absent) — and assert
/// their rendered skip lines are pairwise distinguishable (with the
/// issue-number/branch stripped out, so two causes can't appear "distinct"
/// merely because they name different issues). Today's `StderrNotifier`
/// renders every `IssueDispatchSkipped` identically regardless of cause, and
/// the label cause doesn't even skip (dispatches instead) — so this is RED
/// until BOTH the label read-back (M1.2) and a per-cause reason (M1.3) land.
///
/// Does not assert: the fourth cause (a concurrent creator winning the
/// `git worktree add` TOCTOU race) — see the tester's report; it has no
/// deterministic black-box trigger in this harness.
#[spec("scheduler/dispatch/025")]
#[test]
fn dispatch_025_skip_causes_render_distinguishably() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues_with_labels(repo, &[(31, &[]), (32, &[]), (33, &["in-progress"])]);
    stub.set_open_pr(repo, 32);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    let p31 = derive_issue_paths(Path::new(&work_str), "dispatch-task", 31);

    // First fire: 31 has no signal (dispatches, proving the flow ran); 32 is
    // skipped (open PR — cause B); 33 is skipped POST-FIX (label — cause C),
    // captured now BEFORE a second fire could ever give it a worktree too.
    daemon.run_now("dispatch-task").expect("run-now (first)");
    assert!(
        common::wait_for_path(&p31.worktree_dir, w()),
        "issue 31 (no signal) must dispatch — the worktree appearing proves the flow started"
    );
    // The worktree appearing is NOT the first fire finishing: the fork writes
    // its `in-progress` claim (comment, label, assignee) and upstream's claim
    // record only after the worktree exists and the agent is spawned, and
    // `IssueDispatched` is the issue's last word. Snapshotting there and firing
    // again would hit "previous run still active" and drop the second fire. The
    // first fire is complete once its last issue (33) has rendered its line;
    // the bounded wait does not assert, so a regression that makes 33 dispatch
    // or stay silent still fails on the cause-C assertion below with its own
    // message.
    common::wait_until(w(), || {
        let text = daemon.stderr_text();
        text.contains("dispatched issue #31")
            && find_non_dispatch_line_for(&text, 32).is_some()
            && find_non_dispatch_line_for(&text, 33).is_some()
    });
    let snapshot_1 = daemon.stderr_text();
    let line_b = find_non_dispatch_line_for(&snapshot_1, 32).unwrap_or_default();
    let line_c = find_non_dispatch_line_for(&snapshot_1, 33).unwrap_or_default();

    // Second fire: 31's worktree now exists → skipped (cause A). Captured from
    // the DELTA past snapshot_1 so it can't be confused with any line already
    // seen (stderr is append-only). The scheduler answers `run-now` with
    // `started` or `skipped` (previous run still active), so a fire that lost
    // the race against the first run's last bookkeeping is repeated until one
    // is accepted instead of waiting 10 s for output that will never come.
    let second_fire_accepted = common::wait_until(w(), || {
        let response = daemon.run_now("dispatch-task").expect("run-now (second)");
        dot_agent_deck::daemon_client::run_now_outcome_from_agents(&response.agents)
            == dot_agent_deck::daemon_client::RunNowOutcome::Started
    });
    assert!(
        second_fire_accepted,
        "the scheduler never accepted the second fire (previous run still active); observed \
         stderr:\n{}",
        daemon.stderr_text()
    );
    let cause_a_seen = common::wait_until(Duration::from_secs(10), || {
        daemon.stderr_text()[snapshot_1.len()..].contains("issue #31")
    });
    assert!(
        cause_a_seen,
        "a re-fire with issue 31's worktree present must surface a skip for it; observed stderr:\n{}",
        daemon.stderr_text()
    );
    let snapshot_2 = daemon.stderr_text();
    let delta_2 = &snapshot_2[snapshot_1.len()..];
    let line_a = find_non_dispatch_line_for(delta_2, 31).unwrap_or_default();

    // Each cause must actually have rendered SOMETHING — the label cause (C)
    // is the one expected to be empty today (M1.2 missing), which is exactly
    // the RED this test pins.
    assert!(
        !line_a.is_empty(),
        "expected a skip line for cause A (worktree exists, issue #31); stderr:\n{}",
        daemon.stderr_text()
    );
    assert!(
        !line_b.is_empty(),
        "expected a skip line for cause B (open PR, issue #32); stderr:\n{}",
        daemon.stderr_text()
    );
    assert!(
        !line_c.is_empty(),
        "expected a skip line for cause C (in-progress label, issue #33) — RED until M1.2 reads \
         the label as a third idempotency signal; stderr:\n{}",
        daemon.stderr_text()
    );

    let norm_a = normalize_skip_line(line_a, 31, "agent/issue-31");
    let norm_b = normalize_skip_line(line_b, 32, "agent/issue-32");
    let norm_c = normalize_skip_line(line_c, 33, "agent/issue-33");
    assert_ne!(
        norm_a, norm_b,
        "cause A (worktree exists) and cause B (open PR) must render distinguishably; both \
         currently read: {norm_a:?}"
    );
    assert_ne!(
        norm_a, norm_c,
        "cause A (worktree exists) and cause C (in-progress label) must render distinguishably; \
         both currently read: {norm_a:?}"
    );
    assert_ne!(
        norm_b, norm_c,
        "cause B (open PR) and cause C (in-progress label) must render distinguishably; both \
         currently read: {norm_b:?}"
    );
}

/// Scenario: Fire an `issue_dispatch` task with `triage = true` for one open
/// issue. After the dispatch succeeds (worktree + orchestrator agent
/// present, as in `scheduler/dispatch/001`), the stub `gh` log must show a
/// `gh label ...` invocation naming EACH of PRD #421 M2.0's seven triage
/// labels (the create/list/force mechanism is the coder's choice — only that
/// every label was requested), and the prompt DELIVERED to the dispatched
/// agent (echoed by `cat`) must carry the full vocabulary plus the
/// `needs-triage` uncertainty rule: under uncertainty, apply `needs-triage`
/// and leave priority unset rather than guess. RED today: `triage` is not a
/// recognized `IssueDispatchConfig` field (it parses and is silently
/// ignored), so neither the label-ensure calls nor the prompt text exist in
/// any code path yet.
#[spec("scheduler/dispatch/026")]
#[test]
fn dispatch_026_triage_enabled_ensures_labels_and_prompts_agent() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task_with_triage(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    let agent = daemon
        .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
        .unwrap_or_else(|| {
            panic!(
                "a triage-enabled dispatch must still succeed and spawn into {:?}",
                paths.worktree_dir
            )
        });

    // Labels ensured to exist: some `gh label ...` invocation names each of
    // the seven vocabulary labels. Filtered on the `label` COMMAND GROUP
    // (argv starts with "label "), not a loose substring match, so this
    // can't be fooled by the unrelated `--add-label in-progress` FLAG on the
    // already-shipped `gh issue edit` claim write (PRD #421 M1.0).
    let label_group_calls = || -> Vec<String> {
        stub.gh_calls()
            .into_iter()
            .filter(|l| l.starts_with("label "))
            .collect()
    };
    let ensured = common::wait_until(Duration::from_secs(10), || {
        let calls = label_group_calls();
        TRIAGE_LABELS
            .iter()
            .all(|label| calls.iter().any(|l| l.contains(label)))
    });
    assert!(
        ensured,
        "triage=true must ensure all seven vocabulary labels exist via `gh label ...`; \
         observed `gh label` calls:\n{}",
        label_group_calls().join("\n")
    );

    // The delivered prompt must carry the full vocabulary and the
    // uncertainty rule, captured once so multiple substrings can be checked
    // against the same snapshot.
    let delivered = daemon.attach_and_capture_output(&agent.id, Duration::from_millis(600), w());
    for label in TRIAGE_LABELS {
        assert!(
            delivered.contains(label),
            "the prompt delivered to the dispatched agent must carry the triage vocabulary, \
             missing {label:?}; delivered:\n{delivered}"
        );
    }
    let lower = delivered.to_lowercase();
    let states_uncertainty = ["uncertain", "not confident", "unsure"]
        .iter()
        .any(|kw| lower.contains(kw));
    let states_unset_priority = [
        "unset",
        "no priority",
        "without a priority",
        "leave priority",
    ]
    .iter()
    .any(|kw| lower.contains(kw));
    assert!(
        states_uncertainty && states_unset_priority,
        "the delivered prompt must state the uncertainty rule — under uncertainty apply \
         `needs-triage` and leave priority unset rather than guess; delivered:\n{delivered}"
    );
}

/// Scenario: Fire an `issue_dispatch` task WITHOUT `triage` set (the
/// default, off) for one open issue. Assert no `gh label ...` invocation
/// naming one of the seven triage-vocabulary labels is ever recorded (the
/// unconditional `in-progress` claim, PRD #421 M1.0, also uses the `label`
/// command group via `gh label create`/`gh issue edit --add-label` when the
/// review fix lands, and is deliberately exempt from this guard — it is not
/// triage) and the prompt delivered to the dispatched agent carries none of
/// PRD #421 M2.0's seven triage-vocabulary terms — a regression guard
/// against the triage feature leaking into the default dispatch path.
#[spec("scheduler/dispatch/027")]
#[test]
fn dispatch_027_triage_off_by_default_no_label_no_prompt_text() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[7]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    let agent = daemon
        .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
        .unwrap_or_else(|| {
            panic!(
                "dispatch must succeed with triage absent too, into {:?}",
                paths.worktree_dir
            )
        });

    let delivered = daemon.attach_and_capture_output(&agent.id, Duration::from_millis(600), w());
    for label in TRIAGE_LABELS {
        assert!(
            !delivered.contains(label),
            "triage is off by default — the delivered prompt must not carry the triage \
             vocabulary, but found {label:?}; delivered:\n{delivered}"
        );
    }

    // Narrowed from "no `label` call at all" to "no `label` call naming the
    // triage vocabulary" (PRD #421 review fix): the unconditional
    // `in-progress` claim (M1.0) also uses the `label` command group
    // (`gh label create`/`gh issue edit --add-label`) once the fix lands, but
    // `in-progress` is NOT triage vocabulary — it is the claim, which is
    // unconditional by design. This is a faithful expression of the test's
    // original intent ("the triage feature does not leak into the default
    // path"), not a weakening of it.
    let vocabulary_label_calls: Vec<String> = stub
        .gh_calls()
        .into_iter()
        .filter(|l| l.starts_with("label ") && TRIAGE_LABELS.iter().any(|label| l.contains(label)))
        .collect();
    assert!(
        vocabulary_label_calls.is_empty(),
        "triage is off by default — no `gh label ...` call may ever name the triage vocabulary; \
         observed: {vocabulary_label_calls:?}"
    );
}

/// Scenario: Fire an `issue_dispatch` task into a repo whose `in-progress`
/// label does NOT pre-exist — the PRD #421 review's own counterexample
/// (`vfarcic/dot-ai`, the docs' own example target, carries none of the
/// seven-label vocabulary and would silently no-op pre-fix; deliberately no
/// [`GhStub::seed_labels`] call here, unlike `scheduler/dispatch/010`).
/// Assert the dispatch still ends with the issue GENUINELY claimed: an
/// `issue edit ... --add-label in-progress` call is attempted, and — the
/// actual defect this test pins — the add-label call SUCCEEDS
/// (`GhStub::label_applied`), which only happens if `in-progress` is ensured
/// to exist before the add. RED today: nothing ensures `in-progress` exists
/// first, so the tightened stub's real-`gh`-accurate rejection fires and the
/// claim silently fails (swallowed into `tracing::warn!`, so the run still
/// reports success).
#[spec("scheduler/dispatch/043")]
#[test]
fn dispatch_043_claim_succeeds_when_label_does_not_preexist() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, true);
    // Deliberately no `stub.seed_labels(...)` call — `in-progress` does not
    // exist on this repo yet, mirroring a fresh GitHub repo.
    stub.set_issues(repo, &[7]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 7);
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .is_some(),
        "the dispatch must succeed even when `in-progress` doesn't pre-exist on the repo"
    );

    let attempted = common::wait_until(Duration::from_secs(10), || {
        stub.gh_calls().iter().any(|l| {
            l.contains("issue")
                && l.contains("edit")
                && l.contains("--add-label")
                && l.contains("in-progress")
                && l.contains('7')
        })
    });
    assert!(
        attempted,
        "expected an `issue edit --add-label in-progress` attempt for issue 7; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    let applied = common::wait_until(Duration::from_secs(5), || {
        stub.label_applied(repo, 7, IN_PROGRESS_LABEL)
    });
    assert!(
        applied,
        "the `in-progress` label must be ENSURED to exist before being added, so the add-label \
         call succeeds even on a repo that starts with none of the label vocabulary — but the \
         tightened stub (modeling real `gh`'s label-name-to-ID resolution) rejected it because \
         nothing created `in-progress` first; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );
}

/// Scenario: Fire an `issue_dispatch` task against a fixture repo whose
/// `.dot-agent-deck.toml` opens an ORCHESTRATION tab (`ORCH_TOML`'s
/// `dispatch-orch`). Assert the posted claim comment names the
/// ORCHESTRATION's own typed name (`dispatch-orch`) as DECORATION, and
/// separately anchors on the COMPLETE round-3 identity — the dispatched
/// worktree's canonical absolute path AND its branch (`agent/issue-<n>`,
/// CLAUDE.md rule 23) — regardless of which spawn kind fired it. The test
/// matches against production's OWN `claim_comment_body` rendering of the
/// `Identity` production would build for `derive_issue_paths`' lexically-built
/// path (via `worktree_identity_fragment`): the identity rendered into the
/// comment is physically resolved (`canonicalize_identity_path`), so a raw
/// lexical comparison would falsely diverge whenever the workspace root is
/// reached through a symlink — and the fragment's path and branch fields are
/// located by their structural backtick delimiters, not by searching for the
/// prose around them (fork#243 round 4), so a comment naming a different
/// worktree that merely shares the expected path as a prefix can't satisfy
/// the match either (the hole round 2's independent substring checks left
/// open). What differs between an orchestration dispatch
/// and a single-agent one (`scheduler/dispatch/010`) is only which name is
/// rendered as decoration.
#[spec("scheduler/dispatch/044")]
#[test]
fn dispatch_044_orchestration_dispatch_names_the_orchestration_in_the_claim() {
    let stub = GhStub::new();
    let repo = "acme/orch-claim";
    stub.add_repo(repo, true);
    stub.seed_labels(repo, &[IN_PROGRESS_LABEL]);
    stub.set_issues(repo, &[41]);

    let (_work_td, work_str, daemon) =
        spawn_dispatch_daemon(&stub, "claim-task-021", "ORCHCLAIM-{{issue_number}}", repo);
    daemon
        .run_now("claim-task-021")
        .expect("run-now claim-task-021");

    let paths = derive_issue_paths(Path::new(&work_str), "claim-task-021", 41);
    assert!(
        daemon
            .wait_for_agent_where(|r| orchestrator_in(r, &paths.worktree_dir), w())
            .is_some(),
        "the orchestration dispatch must succeed (precondition for a claim-comment write)"
    );

    let commented = common::wait_until(Duration::from_secs(10), || {
        stub.gh_calls()
            .iter()
            .any(|l| l.contains("issue") && l.contains("comment") && l.contains("dispatch-orch"))
    });
    assert!(
        commented,
        "an orchestration dispatch's claim comment must name the ORCHESTRATION's own typed name \
         (`dispatch-orch`), not merely be posted; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    // PRD fork#235 round 3: the identity itself is the dispatched worktree's
    // absolute path plus its branch (CLAUDE.md rule 23) regardless of spawn
    // kind — `dispatch-orch` above is decoration only. `derive_issue_paths`
    // builds `paths.worktree_dir` lexically, while the identity rendered
    // into the comment is physically resolved (`canonicalize_identity_path`),
    // so `assert_claim_names_worktree_identity` builds the `Identity`
    // production would build and matches against production's OWN
    // `claim_comment_body` rendering (fork#243 round 3) — the delimited
    // fragment `` working `<path>` on branch `<branch>` ``, not two
    // independently `contains()`-checked substrings: a regression that
    // serialises the correct branch but omits the path, or pairs the branch
    // with a DIFFERENT canonical path (including one sharing the expected
    // path as a mere prefix), must still fail here.
    assert_claim_names_worktree_identity(
        &stub,
        &paths.worktree_dir,
        &paths.branch,
        "decorate it with the orchestration name",
    );

    let named_orchestration = stub
        .gh_calls()
        .iter()
        .any(|l| l.contains("issue") && l.contains("comment") && l.contains("dispatch-orch"));
    assert!(
        named_orchestration,
        "the claim comment must name the ORCHESTRATION (`dispatch-orch`) as decoration \
         alongside the round-3 identity anchor asserted above — PRD #421 named the scheduled \
         task exclusively; fork#235 M1/M2 derives the claimant's decoration from the bound \
         spawn handle's `SpawnKind` instead. This does NOT assert the scheduled task's name \
         (`claim-task-021`) is absent from the comment: `derive_issue_paths` keys the clone \
         directory on the task name for every `SpawnKind`, so the canonical worktree path \
         asserted above (via `assert_claim_names_worktree_identity`) legitimately contains \
         `claim-task-021` as a path segment — asserting its absence would contradict that path \
         assertion; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );
}

/// Scenario: Arm the stub so `gh api user --jq .login` fails (a read-only or
/// expired token), then fire a plain single-agent dispatch. Assert the label
/// and claim comment still land (best-effort, PRD fork#235 M2), no assignee
/// is ever written, and the run is NOT reported as a failed dispatch — a
/// claim-identity resolution failure must never turn an already-successful
/// dispatch into `IssueDispatchFailed`, the exact defect PRD #421's Risks
/// section named for the label/comment writes, now extended to the new
/// assignee write.
#[spec("scheduler/dispatch/045")]
#[test]
fn dispatch_045_gh_api_user_failure_skips_assignee_without_failing_dispatch() {
    let stub = GhStub::new();
    let repo = "acme/no-login";
    stub.add_repo(repo, false);
    stub.seed_labels(repo, &[IN_PROGRESS_LABEL]);
    stub.set_issues(repo, &[51]);
    stub.fail_api_user();

    let (_work_td, work_str, daemon) =
        spawn_dispatch_daemon(&stub, "claim-task-022", "NOLOGIN-{{issue_number}}", repo);

    daemon
        .run_now("claim-task-022")
        .expect("run-now claim-task-022");

    let paths = derive_issue_paths(Path::new(&work_str), "claim-task-022", 51);
    assert!(
        daemon
            .wait_for_agent_where(|r| single_card_in(r, &paths.worktree_dir), w())
            .is_some(),
        "the dispatch must succeed (precondition for a label/comment write)"
    );

    let labelled = common::wait_until(Duration::from_secs(10), || {
        stub.label_applied(repo, 51, IN_PROGRESS_LABEL)
    });
    assert!(
        labelled,
        "the `in-progress` label must still land even when `gh api user` fails; observed gh \
         calls:\n{}",
        stub.gh_calls().join("\n")
    );
    let commented = common::wait_until(Duration::from_secs(5), || {
        stub.gh_calls()
            .iter()
            .any(|l| l.contains("issue") && l.contains("comment"))
    });
    // PRD fork#235 round 3: identity resolution (the dispatched worktree's
    // absolute path plus its branch, CLAUDE.md rule 23) needs no `gh api
    // user` call at all — only the ASSIGNEE login does — so the comment must
    // still name the round-3 identity even when the login resolution that
    // feeds the assignee write fails.
    let worktree_str = paths.worktree_dir.to_string_lossy().into_owned();
    let named_worktree_identity = common::wait_until(Duration::from_secs(3), || {
        stub.gh_calls().iter().any(|l| {
            l.contains("issue")
                && l.contains("comment")
                && l.contains(&worktree_str)
                && l.contains(&paths.branch)
        })
    });
    assert!(
        named_worktree_identity,
        "the claim comment must name the round-3 identity anchor — the dispatched worktree's \
         absolute path ({worktree_str:?}) and its branch ({:?}) — even when `gh api user` fails \
         to resolve a login for the assignee write; observed gh calls:\n{}",
        paths.branch,
        stub.gh_calls().join("\n")
    );
    assert!(
        commented,
        "the claim comment must still be posted even when `gh api user` fails; observed gh \
         calls:\n{}",
        stub.gh_calls().join("\n")
    );

    // Give the (would-be) assignee write time to happen, then assert it
    // never did. `wait_until` exits EARLY if an assignee appears (catching
    // the bug fast); it only waits out the full timeout when none ever does.
    let assigned = common::wait_until(Duration::from_secs(5), || {
        !stub.assignees(repo, 51).is_empty()
    });
    assert!(
        !assigned,
        "no assignee may ever be written when `gh api user` fails to resolve a login — got \
         assignees {:?}",
        stub.assignees(repo, 51)
    );

    assert!(
        !(daemon.stderr_contains("issue #51") && daemon.stderr_contains("failed")),
        "a `gh api user` failure must never turn this already-successful dispatch into an \
         IssueDispatchFailed — the exact defect PRD #421's Risks section named for the \
         label/comment writes, now extended to the assignee write; stderr:\n{}",
        daemon.stderr_text()
    );
}

// ---------------------------------------------------------------------------
// Issue #171 — repo allowlist + dry-run
// ---------------------------------------------------------------------------

/// Scenario: Fire an `issue_dispatch` task whose `[scheduled_tasks.issue_dispatch]`
/// table carries a `repo_allowlist` that does NOT include the task's own
/// `repo` — a misconfigured/over-broad schedule pointed at the wrong target.
/// Assert the run refuses outright, fail-closed: NO `gh` invocation of any
/// kind is ever made (not the issue enumeration, not the repo clone), no
/// per-issue worktree is created, and the daemon's stderr names the refusal.
#[spec("scheduler/dispatch/046")]
#[test]
fn dispatch_046_repo_allowlist_refuses_when_repo_not_listed() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    // A fully working fixture (real clone-able remote, a real open issue) so
    // this test's guard, if it were ever removed, would produce an OBSERVABLE
    // dispatch (worktree + agent) rather than coincidentally erroring on a
    // missing fixture — the RED this test pins is about the guard, not about
    // an incidentally-broken repo.
    stub.add_repo(repo, true);
    stub.set_issues(repo, &[9]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let mut toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    toml.push_str("repo_allowlist = [\"acme/other\"]\n");

    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let refused = daemon.wait_for_stderr_contains("repo_allowlist", w());
    assert!(
        refused,
        "a repo excluded from repo_allowlist must be refused with a message naming the \
         allowlist; stderr:\n{}",
        daemon.stderr_text()
    );

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 9);
    assert!(
        !paths.worktree_dir.exists(),
        "a refused repo must never be dispatched into a per-issue worktree"
    );

    assert!(
        stub.gh_calls().is_empty(),
        "a repo excluded by repo_allowlist must produce ZERO `gh` invocations — not even the \
         issue-enumeration read — since the refusal must happen before any `gh`/`git` call; \
         observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );
}

/// Scenario: Fire an `issue_dispatch` task with `dry_run = true` against a
/// repo/issue with no other claim signal. Assert the dry run reports a
/// genuine dispatch decision (a daemon stderr line naming the issue) but
/// performs NONE of the actual dispatch work — no per-issue worktree or
/// branch is created on disk, no agent is spawned — and, as before, none of
/// the three claim writes (`gh issue comment`, `gh issue edit --add-label`,
/// `gh issue edit --add-assignee`) nor the unconditional `gh label create`
/// ensure ever reach the stub `gh`. Auditor F2 (PR #753): a dry run that
/// reached `create_worktree` here would leave real on-disk state — the exact
/// worktree/branch/marker a real dispatch writes — that a SUBSEQUENT real
/// run's idempotency check would then mistake for "already claimed";
/// `scheduler/dispatch/048` pins that two-run scenario end-to-end, and this
/// test pins the prerequisite it depends on: a dry run must never reach
/// `create_worktree`/`spawn` at all.
#[spec("scheduler/dispatch/047")]
#[test]
fn dispatch_047_dry_run_reports_decision_without_side_effects() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, false);
    stub.seed_labels(repo, &[IN_PROGRESS_LABEL]);
    stub.set_issues(repo, &[11]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let mut toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    toml.push_str("dry_run = true\n");

    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];
    let daemon = common::spawn_daemon_serve_with_env(Some(&toml), "0", &env);

    daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task");

    let reported = daemon.wait_for_stderr_contains("dry run", w());
    assert!(
        reported,
        "a dry run must report the dispatch decision for issue #11 on stderr; stderr:\n{}",
        daemon.stderr_text()
    );

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 11);
    assert!(
        !paths.worktree_dir.exists(),
        "a dry run must never create the per-issue worktree on disk — auditor F2 (PR #753): \
         that on-disk state is exactly what a later REAL run's idempotency check would mistake \
         for \"already claimed\""
    );

    // Give a (regressed) spawn time to land — `wait_until` exits EARLY the
    // moment one appears (catching a regression fast), mirroring
    // `dispatch_030`'s identical negative-assertion pattern.
    let spawned = common::wait_until(Duration::from_secs(3), || {
        daemon
            .agent_records()
            .iter()
            .any(|r| single_card_in(r, &paths.worktree_dir))
    });
    assert!(
        !spawned,
        "a dry run must never spawn a real agent into the per-issue worktree"
    );

    // `l.contains("issue comment")`, NOT the looser `contains("issue") &&
    // contains("comment")`: the claim-issue flow's OWN read
    // (`issue_view_claim_state_argv`'s "issue view --json
    // comments,assignees") legitimately contains both substrings
    // independently and must not be mistaken for the write this asserts
    // against. Moot for THIS test now (a dry run never reaches `claim_issue`
    // at all), kept so a regression that made it reach that far would still
    // be caught by name rather than only by the worktree/spawn assertions
    // above.
    let wrote_comment = common::wait_until(Duration::from_secs(3), || {
        stub.gh_calls().iter().any(|l| l.contains("issue comment"))
    });
    assert!(
        !wrote_comment,
        "dry_run must suppress the claim comment write entirely; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    let wrote_label_add = common::wait_until(Duration::from_secs(3), || {
        stub.gh_calls().iter().any(|l| l.contains("--add-label"))
    });
    assert!(
        !wrote_label_add,
        "dry_run must suppress the `in-progress` label write entirely; observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    let wrote_label_create = common::wait_until(Duration::from_secs(3), || {
        stub.gh_calls()
            .iter()
            .any(|l| l.contains("label") && l.contains("create"))
    });
    assert!(
        !wrote_label_create,
        "dry_run must suppress the unconditional `gh label create` claim-label ensure too; \
         observed gh calls:\n{}",
        stub.gh_calls().join("\n")
    );

    let assigned = common::wait_until(Duration::from_secs(3), || {
        !stub.assignees(repo, 11).is_empty()
    });
    assert!(
        !assigned,
        "dry_run must suppress the assignee write entirely — got assignees {:?}",
        stub.assignees(repo, 11)
    );

    assert!(
        !stub.label_applied(repo, 11, IN_PROGRESS_LABEL),
        "dry_run must never actually apply the `in-progress` label"
    );
}

/// Scenario: Fire an `issue_dispatch` task with `dry_run = true` against a
/// repo/issue, then fire a SECOND, separate daemon configured with
/// `dry_run = false` (the default) against the exact same workspace, repo,
/// and issue. Assert the real run genuinely claims/dispatches the issue — a
/// per-issue worktree is created, an agent is spawned, and the claim comment
/// / `in-progress` label are written for real. This is auditor finding F2 on
/// PR #753, end to end: before the fix, the dry run's `create_worktree` call
/// left the exact on-disk marker a real dispatch writes, so the PRIMARY
/// idempotency signal (`worktree_exists`) treated the issue as already
/// claimed on this second, real fire and silently skipped it — the real
/// claim comment/label/assignee were never posted. `scheduler/dispatch/047`
/// pins the prerequisite (a dry run alone never creates the worktree); this
/// test pins the consequence that actually mattered — a REAL run after a
/// dry run must still do real work.
#[spec("scheduler/dispatch/048")]
#[test]
fn dispatch_048_dry_run_then_real_run_still_dispatches_for_real() {
    let stub = GhStub::new();
    let repo = "acme/widgets";
    stub.add_repo(repo, false);
    stub.set_issues(repo, &[21]);

    let work_td = common::harness_tempdir().expect("workspace tempdir");
    let work = work_td.path().join("ws");
    std::fs::create_dir_all(&work).expect("create workspace root");
    let work_str = work.to_string_lossy().into_owned();

    let path = stub.path_env();
    let ghdir = stub.ghstub_dir();
    let env: Vec<(&str, &str)> = vec![("PATH", path.as_str()), ("GHSTUB_DIR", ghdir.as_str())];

    let paths = derive_issue_paths(Path::new(&work_str), "dispatch-task", 21);

    // First fire: a dry run against issue #21, over its own daemon.
    {
        let mut dry_toml = dispatch_task(
            "dispatch-task",
            &work_str,
            "ISSUEDISPATCH-{{issue_number}}",
            repo,
            5,
        );
        dry_toml.push_str("dry_run = true\n");
        let dry_daemon = common::spawn_daemon_serve_with_env(Some(&dry_toml), "0", &env);
        dry_daemon
            .run_now("dispatch-task")
            .expect("run-now dispatch-task (dry run)");

        let reported = dry_daemon.wait_for_stderr_contains("dry run", w());
        assert!(
            reported,
            "the dry run must report a dispatch decision for issue #21; stderr:\n{}",
            dry_daemon.stderr_text()
        );

        assert!(
            !paths.worktree_dir.exists(),
            "the dry run must never create the per-issue worktree — a subsequent real run's \
             idempotency check treating it as already-claimed is exactly the auditor F2 defect \
             this test pins; worktree unexpectedly present at {:?}",
            paths.worktree_dir
        );

        // `dry_daemon` (and the agent it must never have spawned) is torn
        // down here, before the second, real daemon starts against the same
        // workspace — Drop SIGKILLs its whole process group.
    }

    // Second fire: a SEPARATE daemon, `dry_run = false` (the default),
    // against the SAME workspace/repo/issue the dry run above just touched.
    let real_toml = dispatch_task(
        "dispatch-task",
        &work_str,
        "ISSUEDISPATCH-{{issue_number}}",
        repo,
        5,
    );
    let real_daemon = common::spawn_daemon_serve_with_env(Some(&real_toml), "0", &env);
    real_daemon
        .run_now("dispatch-task")
        .expect("run-now dispatch-task (real run)");

    assert!(
        real_daemon
            .wait_for_agent_where(|r| single_card_in(r, &paths.worktree_dir), w())
            .is_some(),
        "the REAL run after a preceding dry run must still dispatch issue #21 for real — a \
         dry run must never leave behind on-disk state that a later real run's idempotency \
         check mistakes for \"already claimed\""
    );

    let claimed_comment = common::wait_until(w(), || {
        stub.gh_calls().iter().any(|l| l.contains("issue comment"))
    });
    assert!(
        claimed_comment,
        "the real run must post the claim comment for issue #21 — if this never lands, the \
         preceding dry run silently poisoned the real run's idempotency ledger; observed gh \
         calls:\n{}",
        stub.gh_calls().join("\n")
    );

    assert!(
        stub.label_applied(repo, 21, IN_PROGRESS_LABEL),
        "the real run must actually apply the `in-progress` label to issue #21 — a skip caused \
         by the dry run's leftover worktree would leave this false"
    );

    // `resolve_current_login` resolves to the stub's default `gh-stub-user`
    // login (no `GhStub::fail_api_user`/`set_login` armed here), so a
    // genuinely successful claim also writes the assignee — a fourth
    // independent signal that this run is a real dispatch, not a skip.
    let assigned = common::wait_until(w(), || !stub.assignees(repo, 21).is_empty());
    assert!(
        assigned,
        "the real run must write the assignee for issue #21 — got assignees {:?}",
        stub.assignees(repo, 21)
    );
}
