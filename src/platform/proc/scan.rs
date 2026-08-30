//! PRD #386 M1/M2 — the descendant-process scan the shell-activity signal is
//! built on: a process table, a cycle-safe descendant walk over it, and the
//! **structural discriminator** that classifies a pane as busy or idle.
//!
//! Everything here is pure data and compiles on every platform. Only the act of
//! *sampling* the machine is platform code: [`super::process_table`] is
//! implemented in `unix.rs` and is an unconditional `None` in `windows.rs`,
//! matching `foreground_pgid`'s existing contract. One narrow exception:
//! [`descendant_shell_activity`]'s #644 `wrap`-detection reads this build's
//! own binary identity (`crate::wrap::is_wrap_invocation`) to validate a
//! candidate's argv, which does real I/O (`current_exe()`) rather than
//! working purely off the sampled table — see that function's doc comment.
//!
//! ## Why the discriminator is structural
//!
//! A Claude pane always has long-lived children (`npm exec
//! @upstash/context7-mcp`, `engram mcp`, `caffeinate -i -t 300`, …), so a naive
//! "has descendants" test is `true` 100% of the time and would pin every pane
//! at `Working` forever. The measurement behind PRD #386
//! (2026-08-06, Claude Code 2.1.220) found the separation is structural rather
//! than textual: Claude Code `setsid`-detaches its Bash-tool child into a POSIX
//! session of its own, while **every** other child of the agent stays in the
//! agent's session on the pane's tty. So a pane is busy iff the agent has a
//! transitive descendant whose session id differs from the agent's own.
//!
//! ## The CI trap this must never fall into
//!
//! "The descendant has no controlling terminal" looks like the same test on a
//! developer machine and is **vacuous in a container**, where the agent itself
//! has no controlling terminal either — every descendant matches and the pane
//! pins at `Working` forever. [`ProcessInfo::has_controlling_tty`] is therefore
//! recorded as corroborating evidence only; [`descendant_shell_activity`]
//! compares session ids against **the agent's own** and never reads that field.

use std::collections::{HashMap, HashSet, VecDeque};

/// One row of the process table: the facts the descendant scan needs about a
/// single process.
///
/// `session_id` is the POSIX session id as reported by `getsid(2)` — **not** by
/// `ps -o sess=`, which prints `0` for a non-root caller on macOS and is
/// useless here. A non-positive value means the session id could not be read
/// (the process exited between the sample and the `getsid` call), and
/// [`descendant_shell_activity`] treats such a row as unclassifiable rather
/// than as "different session".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    /// Process id.
    pub pid: i32,
    /// Parent process id, as sampled. A table sampled non-atomically can
    /// contain a cycle after PID reuse — see [`descendants`].
    pub ppid: i32,
    /// POSIX session id (`getsid(2)`). Non-positive when it could not be read.
    pub session_id: i32,
    /// Whether the process has a controlling terminal. **Corroborating only** —
    /// see this module's docs for why it is never sufficient on its own.
    pub has_controlling_tty: bool,
    /// Whether the process leads its own session (`getsid(pid) == pid`).
    pub session_leader: bool,
    /// The process's command line, **or a statement that this sample did not
    /// read it** — see [`CommandLine`], and issue #862 for why the sample no
    /// longer reads every process's.
    pub command_line: CommandLine,
}

/// What a sample knows about one process's command line (issue #862).
///
/// The bulk process-table sample deliberately does **not** ask `ps` for the
/// `args` column any more, because that column is what made the sample
/// load-sensitive: measured with `strace`, `ps -A -o pid=,ppid=,tty=,args=`
/// opens `/proc/<pid>/cmdline` *and* `/proc/<pid>/environ` for every process on
/// the machine (372 of each on a 382-process box), while
/// `ps -A -o pid=,ppid=,tty=` opens neither. Both of those two go through the
/// kernel's `access_remote_vm()` and therefore take the *target's* `mmap_lock`;
/// `/proc/<pid>/stat` and `/proc/<pid>/status`, which supply `pid`/`ppid`/`tty`,
/// do not. So the argv column made the sample's wall time the sum of every
/// unrelated process's `mmap_lock` wait — the mechanism behind the field
/// measurement in issue #862, where a sample went from ~49 ms idle to 19-20 s
/// under a build storm.
///
/// The argv is still needed, for the [`ShellToolShape`] cross-check — but only
/// for a descendant of one of the sample's roots that sits at a **session
/// boundary**: zero of those on an idle deck, and one per `setsid`-ed shell-tool
/// call on a busy one. So it is read in a second phase, for exactly the pids
/// [`shell_tool_candidates`] reports — a set that deliberately excludes the
/// *subtree below* that boundary, which is where a build's `rustc` and `ld`
/// processes live. This enum exists so the difference between "nothing needed
/// it" and "we wanted it and could not get it" survives into the classifier
/// instead of collapsing into an empty string that silently matches no shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLine {
    /// The sample read this process's command line. Callers must substring-match
    /// inside it and must never tokenise it: Claude Code's Bash-tool child has
    /// `argc == 3`, with the whole prologue plus the user's command in a single
    /// argv element.
    Read(String),
    /// The sample deliberately did not read it: this process is not a detached
    /// descendant of any root the sample was given, so no cross-check can ever
    /// consult it. The overwhelmingly common case — every process on the machine
    /// except our own panes' detached descendants.
    NotSampled,
    /// The sample tried to read it and could not — the ordinary cause being that
    /// the process exited between the table sample and the argv sample.
    Unavailable,
}

impl CommandLine {
    /// The command line if this sample read it, else `None`.
    pub fn read(&self) -> Option<&str> {
        match self {
            Self::Read(argv) => Some(argv),
            Self::NotSampled | Self::Unavailable => None,
        }
    }
}

/// A measured shell-tool argv fingerprint for one agent kind — the **secondary
/// cross-check**, never the primary test (PRD #386, Open Question 2).
///
/// It exists because the structural test and the argv test fail on *disjoint*
/// sets: the structural test dies if Claude Code stops `setsid`-ing its Bash
/// child and false-positives on an MCP server that detaches itself, neither of
/// which touches the argv; the argv test dies on prologue rewording,
/// `CLAUDE_CODE_SHELL_PREFIX`, sandbox mode, and the missing-snapshot variant,
/// none of which touches the session id.
///
/// It is **data, not an inlined literal**, so an agent whose shell-tool shape
/// has never been measured simply gets no cross-check (callers pass an empty
/// slice) rather than a fingerprint invented for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellToolShape {
    /// The agent kind this shape was measured against, for diagnostics.
    pub agent: &'static str,
    /// Alternative fingerprints. The argv matches this shape when **any**
    /// alternative matches, and an alternative matches when **every** substring
    /// in it occurs in the command line.
    pub alternatives: &'static [&'static [&'static str]],
}

impl ShellToolShape {
    /// Whether `argv` (a whole command line, never tokenised) carries this
    /// shape. An alternative with no substrings never matches — an empty
    /// fingerprint would match every process on the machine.
    pub fn matches(&self, argv: &str) -> bool {
        self.alternatives
            .iter()
            .any(|alt| !alt.is_empty() && alt.iter().all(|needle| argv.contains(needle)))
    }
}

/// Claude Code's Bash-tool command line, in the narrowest form that survived
/// the 2026-08-06 measurement against Claude Code 2.1.220.
///
/// Two alternatives, because the snapshot `source` segment is absent from the
/// no-snapshot variant entirely:
/// - the usual shape — `shell-snapshots/snapshot-` **and** `&& eval `;
/// - the unalias prologue, which survives when the snapshot segment does not.
///
/// Predicates that were measured and rejected are recorded in the PRD, not
/// here: `argv[0] == "/bin/zsh"` (the interpreter follows `CLAUDE_CODE_SHELL` →
/// `$SHELL`), `setopt NO_EXTENDED_GLOB` (zsh-only), `.claude/shell-snapshots`
/// (breaks under `CLAUDE_CONFIG_DIR`), and any form of argv tokenising.
pub const CLAUDE_BASH_TOOL_SHAPE: ShellToolShape = ShellToolShape {
    agent: "claude",
    alternatives: &[
        &["shell-snapshots/snapshot-", "&& eval "],
        &[r"\builtin unalias -- 'unsetenv'"],
    ],
};

/// Every shell-tool argv shape this project has actually **measured**, and
/// nothing else (PRD #386 M3, Open Question 2).
///
/// This is the catalog the daemon's poll loop hands to
/// [`crate::agent_pty::AgentPtyRegistry::shell_foreground_busy_snapshot`], which
/// selects from it **per pane** by agent kind. An agent whose shell-tool shape
/// has never been measured must select nothing from it and fall back to the
/// structural test alone — handing it a fingerprint measured against a
/// *different* product would let the cross-check veto a genuinely detached
/// descendant, and the resulting false negative is silent (the PRD's "the
/// failure mode to watch for is silence, not noise").
///
/// Adding an entry here is therefore a claim that the shape was measured
/// against a live agent of that kind, not that it looks plausible.
pub const MEASURED_SHELL_TOOL_SHAPES: &[ShellToolShape] = &[CLAUDE_BASH_TOOL_SHAPE];

/// Whether `argv` (a whole command line, never tokenised) is a genuine
/// `dot-agent-deck wrap` invocation. Delegates to [`crate::wrap::is_wrap_invocation`]
/// rather than duplicating its token check, so there is exactly one place
/// that decides this — round 1 of issue #644 kept a laxer local copy that
/// checked only "is the second whitespace token literally `wrap`", with no
/// validation of the first token at all, which any pane command shaped
/// `<anything> wrap …` (`git wrap …`, `npm wrap …`) satisfied for reasons
/// having nothing to do with this deck (round 2, reviewer F3 / auditor F2).
/// `wrap.rs`'s own check validates that the first token's basename is
/// actually this deck's binary — either the literal
/// [`crate::platform::paths::DEFAULT_BINARY_NAME`] or whatever
/// `deck_binary_for_wrap()` resolves this build's own binary to, which is
/// what keeps this matching a fork's renamed install (`worker-agent-deck`,
/// CLAUDE.md rule 21) as well as an unrenamed one.
fn argv_is_wrap_invocation(argv: &str) -> bool {
    crate::wrap::is_wrap_invocation(argv)
}

/// `row`'s sampled command line as a `&str`, or `""` when the two-phase
/// sampler (issue #862) never read one for this row — the overwhelming
/// majority of rows on any tick, since only [`shell_tool_candidates`] gets a
/// second-phase `ps` at all. An empty string never matches
/// [`argv_is_wrap_invocation`] or [`ShellToolShape::matches`], so an
/// unsampled row degrades to "not wrap" / "no shape match" rather than
/// panicking or guessing — the same fail-safe direction this module already
/// takes for an unreadable session id.
fn sampled_argv(row: &ProcessInfo) -> &str {
    match &row.command_line {
        CommandLine::Read(argv) => argv.as_str(),
        CommandLine::NotSampled | CommandLine::Unavailable => "",
    }
}

/// Every transitive descendant of `root_pid` in `table`, each reported exactly
/// once and never including `root_pid` itself.
///
/// The walk carries a visited set because it **must terminate on a cycle**: a
/// `ppid` table sampled non-atomically can loop back into a branch it just
/// descended after PID reuse, and a naive walk would spin forever inside the
/// daemon's poll task.
pub fn descendants(table: &[ProcessInfo], root_pid: i32) -> Vec<&ProcessInfo> {
    let mut children: HashMap<i32, Vec<&ProcessInfo>> = HashMap::new();
    for row in table {
        children.entry(row.ppid).or_default().push(row);
    }

    let mut visited: HashSet<i32> = HashSet::new();
    visited.insert(root_pid);

    let mut queue: VecDeque<i32> = VecDeque::new();
    queue.push_back(root_pid);

    let mut out: Vec<&ProcessInfo> = Vec::new();
    while let Some(pid) = queue.pop_front() {
        let Some(kids) = children.get(&pid) else {
            continue;
        };
        for kid in kids {
            if !visited.insert(kid.pid) {
                continue;
            }
            out.push(kid);
            queue.push_back(kid.pid);
        }
    }
    out
}

/// Classify a pane as busy (`Some(true)`) or idle (`Some(false)`) from a
/// sampled process table, or `None` when the table cannot answer.
///
/// A pane is **busy** iff `root_pid` has a transitive descendant that is in a
/// different POSIX session than the session it inherited — the load-bearing
/// condition, one `getsid` comparison per candidate, immune to any change in
/// what an agent puts on its command line.
///
/// One structural exemption (issue #644): when some node on the path from
/// `root_pid` down to a candidate is itself a `dot-agent-deck wrap`
/// invocation (verified via [`argv_is_wrap_invocation`]) and is still
/// running in the baseline session at the point that node's own direct
/// child diverges, that child — the interactive process `wrap` spawned — is
/// not evidence of a detached command merely for leading a session of its
/// own. `wrap` always allocates a fresh PTY for the interactive child it
/// launches, and that child becomes the leader of *that* PTY's session as an
/// ordinary consequence of spawning it, not because it backgrounded itself
/// (true of every agent `wrap` launches interactively, not just Codex).
///
/// **This is not restricted to `root_pid` itself being `wrap` (round 1's
/// original shape) or to depth 1** (round 2, reviewer/auditor finding F1):
/// `wrap_launch_command`'s output is always multi-word, so
/// `agent_pty::spawn` always routes it through `$SHELL -c '<wrap
/// invocation>'`, never execs `wrap` directly. When that shell exec-replaces
/// itself (bash/zsh, most of the time), `root_pid` ends up being `wrap`
/// itself and the exemption fires at depth 1, same as round 1. When the
/// shell *survives* instead — dash never exec-optimizes, and any shell
/// leaves a surviving process for a `-c` string ending in a pipeline, list,
/// or redirection, and `src/spawn.rs` pins `SHELL=/bin/sh` for every
/// multi-word scheduler/issue-dispatch/orchestration-role command, so this
/// is not a hypothetical topology — `root_pid` is the shell, `wrap` is its
/// direct child (still in the shell's session, since a plain non-exec'd
/// `-c` invocation allocates no new session of its own), and the agent's
/// primary child sits one hop further down, in the fresh session `wrap`
/// allocated for it. The walk therefore tracks, per queued node, whether
/// *that* node's own process is a `wrap` invocation, and reads it from
/// whichever node is the immediate parent of the diverging candidate — not
/// only from `root_pid`.
///
/// This has to be conditioned on the parent actually being `wrap` and
/// cannot be a bare "direct child, self-led session" rule: that exact shape
/// is also what Claude Code's own genuinely-detached Bash-tool child looks
/// like — its `root_pid` is the `claude` process itself (Claude Code is not
/// a `wrap`-launched, [`IntegrationStrategy::Wrapper`](crate::agent_registry::IntegrationStrategy::Wrapper)
/// agent, so neither `claude` nor its `$SHELL -c` wrapper, if any, is ever a
/// `wrap` invocation), and its Bash-tool child is a **direct** child of it,
/// `setsid`-ed into a session it leads itself — structurally identical to
/// `wrap`'s primary child, and it is exactly the case
/// [`CLAUDE_BASH_TOOL_SHAPE`]'s argv cross-check exists to confirm as busy.
/// A blanket structural exemption at that shape would have silently
/// re-introduced PRD #370's original defect for the one agent this signal
/// was measured against.
///
/// Once exempted, `wrap`'s child's own session becomes the new baseline its
/// descendants are compared against, so the rest of the agent's ordinary
/// process tree (still living in the freshly allocated PTY session) is not
/// penalized for the same, already-excused, hop — but a *further* divergence
/// anywhere below that (a genuinely detached descendant one or more session
/// changes deeper) still reads as busy. The exemption is also capped at
/// **one rebase per root-to-leaf path**, tracked per queued node, so a
/// pathological multi-hop `wrap`-launching-`wrap` chain cannot exempt more
/// than the first hop on any single path.
///
/// `shapes` is the optional argv cross-check. When it is empty the structural
/// test stands alone (which is what the measurement says already excludes every
/// observed confounder); when it is non-empty a candidate must additionally
/// carry one of the shapes, so a caller can require confirmation for the one
/// agent whose shell-tool shape has actually been measured while leaving the
/// signal purely structural for the rest.
///
/// **When `shapes` is non-empty, `table` must have been sampled with `root_pid`
/// among its roots** (issue #862) — that is what makes the candidates'
/// [`CommandLine`]s present, since the sampler reads a command line for exactly
/// the pids [`shell_tool_candidates`] reports for its roots, which is also the
/// set the cross-check below iterates. A candidate that reaches the cross-check
/// as [`CommandLine::NotSampled`] means those two sets disagreed; it is read as
/// "not a match" and logged at `warn`, because inventing a match from a command
/// line nobody read would pin the pane at `Working` forever. With an empty
/// `shapes` no command line is read at all, so the roots do not matter.
///
/// `None` means "no answer available": `root_pid` is not in the table (it
/// exited, or the table was sampled from another PID namespace), its own
/// session id could not be read, or — the per-row fail-safe, fork issue #160
/// — at least one candidate descendant's session id could not be read and no
/// other candidate resolved a confirmed `Some(true)`. `None` is deliberately
/// not folded into `Some(false)` — the caller must be able to leave a pane's
/// status alone rather than assert it is idle. A single unreadable row used to
/// be `continue`d identically to a genuinely same-session row, so a candidate
/// set where *every* row was unreadable fell through to a confident
/// `Some(false)` — a total false negative, the worse failure direction (see
/// `docs/develop/shell-activity-signal.md`). A confirmed-busy candidate still
/// short-circuits `Some(true)` immediately, unreadable rows elsewhere in the
/// set notwithstanding.
///
/// Note what this does **not** consult: [`ProcessInfo::has_controlling_tty`].
/// A bare no-controlling-terminal test collapses in a container, where the
/// agent has no terminal either; see this module's docs.
pub fn descendant_shell_activity(
    table: &[ProcessInfo],
    root_pid: i32,
    shapes: &[ShellToolShape],
) -> Option<bool> {
    let detached = detached_descendants(table, root_pid)?;
    // Fork issue #160's per-row fail-safe: a descendant whose own session id
    // could not be read is unclassifiable, not "same session as root" —
    // `detached_descendants`'s filter folds it into "not different" the same
    // way it would fold a genuinely same-session row, so a candidate set where
    // EVERY row is unreadable used to fall through to a confident `Some(false)`
    // — a total false negative, the worse failure direction. Tracked
    // separately here so that case reports `None` instead; a confirmed-busy
    // candidate anywhere in the set still short-circuits `Some(true)` below,
    // unreadable rows elsewhere notwithstanding.
    let had_unreadable_candidate = descendants(table, root_pid)
        .into_iter()
        .any(|row| row.session_id <= 0);
    if detached.is_empty() {
        return if had_unreadable_candidate {
            None
        } else {
            Some(false)
        };
    }
    // `detached_descendants` above already confirmed `root_pid` resolves to a
    // row with a readable session id; re-resolve it here for its argv and
    // session id, which that helper's own `Vec<i32>` return does not carry.
    let root = table
        .iter()
        .find(|row| row.pid == root_pid)
        .expect("detached_descendants already confirmed root_pid resolves");
    let root_is_wrap = argv_is_wrap_invocation(sampled_argv(root));

    let mut children: HashMap<i32, Vec<&ProcessInfo>> = HashMap::new();
    for row in table {
        children.entry(row.ppid).or_default().push(row);
    }

    let mut had_unreadable_candidate = false;
    let mut visited: HashSet<i32> = HashSet::new();
    visited.insert(root_pid);

    // Each queued node carries: the session id its own children must be
    // compared against; whether the process AT this node is a `wrap`
    // invocation still running in that same baseline session (so its own
    // direct child re-leading a session is the #644 shape, wherever in the
    // tree this node sits — round 2, F1); and whether the #644 exemption has
    // already fired once on the path leading here, so a pathological
    // multi-hop `wrap`-launching-`wrap` chain cannot rebase more than once
    // per root-to-leaf path.
    let mut queue: VecDeque<(i32, i32, bool, bool)> = VecDeque::new();
    queue.push_back((root_pid, root.session_id, root_is_wrap, false));

    while let Some((parent_pid, baseline_session_id, parent_is_wrap, already_rebased)) =
        queue.pop_front()
    {
        let Some(kids) = children.get(&parent_pid) else {
            continue;
        };
        for candidate in kids {
            if !visited.insert(candidate.pid) {
                continue;
            }
            // Only worth checking a candidate's own argv for being a further
            // `wrap` hop when it could actually matter: either it is a
            // direct child of `root_pid` (root itself may not be `wrap` —
            // the surviving-shell shape, F1 — so every direct child has to
            // be checked regardless of `root_is_wrap`), or its parent is
            // already a confirmed `wrap` node (a multi-hop chain). Every
            // other branch of the tree — Claude's long-lived MCP children,
            // an agent's ordinary descendants — never pays for this check.
            let candidate_is_wrap = if parent_pid == root_pid || parent_is_wrap {
                argv_is_wrap_invocation(sampled_argv(candidate))
            } else {
                false
            };
            // A row whose session id could not be read is unclassifiable,
            // not "different" — counting it as different would turn an exit
            // racing the sample into a false `Working`. It is tracked
            // separately from "same session" so a set where every candidate
            // is unreadable reports unknown rather than a confident idle
            // (fork issue #160's per-row fail-safe) — a confirmed-busy
            // candidate elsewhere in the set still wins immediately via the
            // `return Some(true)` below, untouched. Its own (unreadable)
            // session id cannot seed a baseline, so descendants keep
            // inheriting the parent's.
            if candidate.session_id <= 0 {
                had_unreadable_candidate = true;
                queue.push_back((
                    candidate.pid,
                    baseline_session_id,
                    candidate_is_wrap,
                    already_rebased,
                ));
                continue;
            }
            if candidate.session_id == baseline_session_id {
                queue.push_back((
                    candidate.pid,
                    baseline_session_id,
                    candidate_is_wrap,
                    already_rebased,
                ));
                continue;
            }
            if !already_rebased && parent_is_wrap && candidate.session_id == candidate.pid {
                // Issue #644: `wrap`'s own primary child re-leading the
                // fresh PTY session it was spawned into — not evidence of a
                // detached foreground command. Rebase its subtree onto its
                // own session rather than penalizing the same hop again for
                // every descendant that inherits it. Gated on `parent_is_wrap`
                // — see this function's doc comment for why a bare
                // "direct child, self-led session" match is not enough —
                // and on `!already_rebased` so this can fire at most once
                // per path.
                queue.push_back((candidate.pid, candidate.session_id, candidate_is_wrap, true));
                continue;
            }
            if !shapes.is_empty()
                && !shapes
                    .iter()
                    .any(|shape| shape.matches(sampled_argv(candidate)))
            {
                queue.push_back((
                    candidate.pid,
                    baseline_session_id,
                    candidate_is_wrap,
                    already_rebased,
                ));
                continue;
            }
            return Some(true);
        }
    }
    // The cross-check consults the SESSION-BOUNDARY subset, not every detached
    // descendant — see [`shell_tool_candidates`] for why that is both correct
    // for the measured shape and the difference between reading one command
    // line and reading a whole build tree's.
    for pid in shell_tool_candidates(table, root_pid)? {
        let Some(candidate) = table.iter().find(|row| row.pid == pid) else {
            continue;
        };
        match &candidate.command_line {
            CommandLine::Read(argv) => {
                if shapes.iter().any(|shape| shape.matches(argv)) {
                    return Some(true);
                }
            }
            // Wanted and not obtained. Ordinary and expected: the process
            // exited between the table sample and the argv sample, and a
            // command line that no longer exists is not running anything, so
            // "not a match" is the right reading.
            CommandLine::Unavailable => continue,
            // Should be unreachable, and it is the one case worth a log line.
            // The sampler fills the command line for exactly the pids
            // `shell_tool_candidates` reports (see `super::process_table`), and
            // that is the very function this loop iterates, so reaching here
            // means the table was sampled for different roots than it is being
            // classified for — which would suppress the signal *silently*, the
            // failure mode PRD #386 exists to end. Treated as "not a match"
            // rather than as a match, because inventing a busy reading from a
            // command line nobody read would pin the pane at `Working`.
            CommandLine::NotSampled => {
                tracing::warn!(
                    root_pid,
                    candidate_pid = pid,
                    "shell-activity: a detached descendant reached the argv cross-check with no \
                     command line sampled; the process-table sampler and the classifier disagree \
                     about which pids need one, so this pane's signal is suppressed"
                );
                continue;
            }
        }
    }
    if had_unreadable_candidate {
        return None;
    }
    Some(false)
}

/// The **structural half** of [`descendant_shell_activity`] on its own: the pids
/// of `root_pid`'s transitive descendants that sit in a different POSIX session
/// than `root_pid` itself, in walk order.
///
/// `None` has exactly [`descendant_shell_activity`]'s meaning — "no answer
/// available": `root_pid` is not in the table, or its own session id could not
/// be read. An empty `Vec` is the positive statement that the root has no
/// detached descendant, i.e. structurally idle.
///
/// This is the **structural test** and nothing else. Which processes a
/// cross-check could ever need the command line of is the narrower
/// [`shell_tool_candidates`] (issue #862), which both the two-phase sampler and
/// [`descendant_shell_activity`]'s cross-check call — sharing that function is
/// what keeps those two sets identical rather than merely intended to be.
pub fn detached_descendants(table: &[ProcessInfo], root_pid: i32) -> Option<Vec<i32>> {
    let root = table.iter().find(|row| row.pid == root_pid)?;
    if root.session_id <= 0 {
        return None;
    }
    Some(
        descendants(table, root_pid)
            .into_iter()
            // A row whose session id could not be read is unclassifiable, not
            // "different" — counting it as different would turn an exit racing
            // the sample into a false `Working`.
            .filter(|row| row.session_id > 0 && row.session_id != root.session_id)
            .map(|row| row.pid)
            .collect(),
    )
}

/// The detached descendants of `root_pid` that the argv cross-check can actually
/// be carried by: the ones at a **session boundary** — a descendant in a
/// different POSIX session than `root_pid` whose *own parent* is in a different
/// session than it is (issue #862).
///
/// **This is the narrowing that makes the two-phase sample worth having, and
/// leaving it out was measured to matter.** `setsid()` creates a session; every
/// process below the caller then *inherits* it. Verified directly: a
/// `setsid bash -c 'sleep 40 & sleep 40 & wait'` reports its own session id and
/// both `sleep`s report that same id, neither of them a leader. So for a Claude
/// pane running a Bash-tool `cargo build`, [`detached_descendants`] is the
/// **whole build tree** — `cargo`, every `rustc`, every `ld` — and reading all
/// of their command lines would put the poll straight back to reading the
/// `mmap_lock` of exactly the processes that stalled it in the first place
/// (issue #862's episode had 13 `ld` processes wedged in `D` state). The
/// boundary set is the one `setsid`-ed shell per detached session instead.
///
/// **Why that is the right set rather than merely the small one.** The measured
/// Claude Bash-tool argv (`shell-snapshots/snapshot-…` plus `&& eval `) belongs
/// to the process that `setsid`-ed — the session's leader, which is exactly a
/// boundary process; `status/shell-activity/001`/`004` both assert that the real
/// detached child is its own session leader. Its descendants carry their own
/// command lines (`cargo`, `ld`), which is not where a shell-tool signature
/// lives, so reading them was waste rather than evidence. A shape carried by a
/// *non*-boundary process would need this widened, and none of the measured ones
/// is.
///
/// A descendant whose parent is absent from the table is treated as a boundary:
/// the walk only reaches a process through its parent, so this should not arise,
/// and including it is the direction that cannot lose a candidate.
///
/// `None` has [`detached_descendants`]'s meaning. The two sets are non-empty
/// together for any real process tree — `setsid` never *joins* an existing
/// session, so the shallowest detached descendant's parent is necessarily still
/// in the root's session and that edge is a boundary. The structural test in
/// [`descendant_shell_activity`] keeps using the wider set anyway, which means a
/// pane with **no** shapes never consults this function at all; note that it is
/// not a blanket guarantee for a pane that *does* have shapes, where an empty
/// boundary set alongside a non-empty detached set would read idle, exactly as a
/// cross-check that matches nothing does.
pub fn shell_tool_candidates(table: &[ProcessInfo], root_pid: i32) -> Option<Vec<i32>> {
    let root = table.iter().find(|row| row.pid == root_pid)?;
    if root.session_id <= 0 {
        return None;
    }
    let session_of: HashMap<i32, i32> = table.iter().map(|row| (row.pid, row.session_id)).collect();
    Some(
        descendants(table, root_pid)
            .into_iter()
            .filter(|row| row.session_id > 0 && row.session_id != root.session_id)
            .filter(|row| {
                session_of
                    .get(&row.ppid)
                    .is_none_or(|parent_session| *parent_session != row.session_id)
            })
            .map(|row| row.pid)
            .collect(),
    )
}

/// Every pid whose command line the second sampling phase must read, for a
/// sample taken on behalf of `roots` (issue #862) — the union of
/// [`shell_tool_candidates`] over every root, deduplicated and sorted.
///
/// Sorted so the `ps -p <list>` invocation built from it is deterministic, which
/// is what makes the argv phase testable against a fixed expected command line.
///
/// A root the table cannot answer for contributes nothing rather than aborting
/// the whole set: one exited pane must not cost every other pane its argv.
pub fn command_line_targets(table: &[ProcessInfo], roots: &[i32]) -> Vec<i32> {
    let mut wanted: Vec<i32> = roots
        .iter()
        .filter_map(|root| shell_tool_candidates(table, *root))
        .flatten()
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    wanted
}

/// Record the second phase's answers onto the table: every pid in `wanted` gets
/// [`CommandLine::Read`] if `resolved` carries one and [`CommandLine::Unavailable`]
/// if it does not (issue #862).
///
/// Rows outside `wanted` are left at [`CommandLine::NotSampled`], which is the
/// honest statement about them — nothing asked for their command line, so
/// nothing read it.
pub fn fill_command_lines(
    table: &mut [ProcessInfo],
    wanted: &[i32],
    resolved: &HashMap<i32, String>,
) {
    let wanted: HashSet<i32> = wanted.iter().copied().collect();
    for row in table.iter_mut() {
        if !wanted.contains(&row.pid) {
            continue;
        }
        row.command_line = match resolved.get(&row.pid) {
            Some(argv) => CommandLine::Read(argv.clone()),
            None => CommandLine::Unavailable,
        };
    }
}

/// Whether a `ps` TTY column names a real terminal. macOS prints `??` for a
/// process with no controlling terminal, Linux's procps prints `?`, and `-`
/// turns up in some `ps` implementations; anything else is a terminal name.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn tty_field_names_a_terminal(tty: &str) -> bool {
    !matches!(tty, "" | "?" | "??" | "-")
}

/// Parse the output of `ps -A -w -w -o pid=,ppid=,tty=` into a table, resolving each
/// row's POSIX session id through `session_id_of`.
///
/// Lives here rather than in `unix.rs` because it is pure string work and is
/// worth unit-testing on every platform, `ps` being the one parsing surface
/// Route A trades a native dependency for. Unparseable lines are skipped rather
/// than failing the whole sample: one odd row must not blind the poll to the
/// rest of the machine.
///
/// `session_leader` is derived from the session id (`getsid(pid) == pid`)
/// rather than from `ps`'s STAT letters, which is both exact and one fewer
/// column of `ps` formatting to depend on.
///
/// **Every row comes back [`CommandLine::NotSampled`]** (issue #862): this phase
/// does not ask `ps` for the `args` column, so it has nothing to say about any
/// command line, and anything trailing the third column is ignored rather than
/// stored. The command lines the cross-check needs arrive from
/// [`parse_ps_command_lines`] via [`fill_command_lines`].
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn parse_ps_table(stdout: &str, session_id_of: &dyn Fn(i32) -> i32) -> Vec<ProcessInfo> {
    let mut rows = Vec::new();
    for line in stdout.lines() {
        let Some((pid, ppid, tty)) = split_ps_row(line) else {
            continue;
        };
        let session_id = session_id_of(pid);
        rows.push(ProcessInfo {
            pid,
            ppid,
            session_id,
            has_controlling_tty: tty_field_names_a_terminal(tty),
            session_leader: session_id > 0 && session_id == pid,
            command_line: CommandLine::NotSampled,
        });
    }
    rows
}

/// Parse the output of the argv phase's `ps -w -w -o pid=,args= -p <list>` into
/// `pid → command line` (issue #862).
///
/// The command line is **kept whole** — interior spaces, quotes and `&&`
/// included — because the [`ShellToolShape`] cross-check substring-matches
/// inside it and must never tokenise it. A pid the output does not mention is
/// simply absent from the map, which [`fill_command_lines`] records as
/// [`CommandLine::Unavailable`]; that is the normal way a process that exited
/// between the two phases shows up.
///
/// A row with a pid and no command line at all is kept as an empty string
/// rather than dropped: it is a genuine answer ("this process has no argv the
/// kernel will show us"), and it matches no shape, which is the correct reading.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn parse_ps_command_lines(stdout: &str) -> HashMap<i32, String> {
    let mut out = HashMap::new();
    for line in stdout.lines() {
        let Some((pid, rest)) = next_token(line) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        out.insert(pid, rest.trim_start().to_string());
    }
    out
}

/// Split one bulk-phase `ps` row into `(pid, ppid, tty)`. All three columns are
/// whitespace-free tokens; anything after them is ignored, since this phase
/// asks for no fourth column (issue #862).
fn split_ps_row(line: &str) -> Option<(i32, i32, &str)> {
    let (pid, rest) = next_token(line)?;
    let (ppid, rest) = next_token(rest)?;
    let (tty, _rest) = next_token(rest)?;
    Some((pid.parse().ok()?, ppid.parse().ok()?, tty))
}

/// Fork issue #30 — drop the `getsid` answer for every row whose identity did
/// not survive a second `ps` sample, taken *after* the session ids were read.
///
/// The sample is not atomic: the process table is captured at one instant and
/// [`parse_ps_table`]'s `getsid(2)` calls happen at a later one. A pid that
/// exits in between can be recycled by the kernel, and then `getsid` answers
/// about a **different process** than the row describes — which, for a
/// descendant of an agent, can invent a "different session" and flip an idle
/// pane to busy.
///
/// The invariant this restores: *a `getsid` answer is trusted only if the
/// process table describes the same process before and after the call.*
/// `confirm_stdout` is a second sample in the same `ps` format; a row is
/// confirmed when its `ppid` **and** whether it has a controlling tty are both
/// unchanged. An unconfirmed row keeps its `pid`/`ppid` edge — so the
/// descendant walk still reaches anything below it — but its session id is
/// reset to the same "could not be read" sentinel a failed `getsid` produces,
/// which [`descendant_shell_activity`] already treats as unclassifiable
/// rather than as evidence of a different session.
///
/// **Narrowed by issue #862** (re-verified during a later upstream sync):
/// this used to also compare the row's whole command line, which closed a
/// tighter residual window — but issue #862 moved the bulk-phase `ps`
/// invocation (`PS_TABLE_ARGS` in `platform::proc::unix`) to a 3-column
/// `pid=,ppid=,tty=` format that asks for no fourth column at all, so neither
/// sample this function ever sees carries a command line to compare.
/// [`ProcessInfo::has_controlling_tty`] is what is left. **This is a real
/// weakening, flagged rather than silently accepted**: a pid recycled into a
/// process with the *same* parent and *some* controlling tty — not
/// necessarily the identical terminal, since only presence/absence survives
/// today — now reads as confirmed where it previously would not have, if the
/// replacement's command line also differed. Closing that gap again would
/// need either the argv phase widened to the confirmation pass too (paying
/// back the exact per-tick fork/exec cost issue #862 removed) or a per-pid
/// start-time token the POSIX surface does not offer either way.
///
/// This narrows the window; it does not close it. Two identical observations
/// still cannot distinguish a pid recycled into a process with the *same*
/// parent and *the same tty presence* — and that residual is only closable
/// with an atomic snapshot or a per-pid start-time token the POSIX surface
/// does not offer.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn invalidate_unconfirmed_session_ids(rows: &mut [ProcessInfo], confirm_stdout: &str) {
    let mut identity: HashMap<i32, (i32, bool)> = HashMap::new();
    for line in confirm_stdout.lines() {
        if let Some((pid, ppid, tty)) = split_ps_row(line) {
            identity.insert(pid, (ppid, tty_field_names_a_terminal(tty)));
        }
    }
    for row in rows.iter_mut() {
        let confirmed = identity.get(&row.pid).is_some_and(|(ppid, has_tty)| {
            *ppid == row.ppid && row.has_controlling_tty == *has_tty
        });
        if !confirmed {
            row.session_id = -1;
            row.session_leader = false;
        }
    }
}

fn next_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    Some(match s.find(char::is_whitespace) {
        Some(end) => (&s[..end], &s[end..]),
        None => (s, ""),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::spec;

    fn row(pid: i32, ppid: i32, session_id: i32, argv: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            session_id,
            has_controlling_tty: true,
            session_leader: session_id == pid,
            command_line: CommandLine::Read(argv.to_string()),
        }
    }

    /// PRD #386 M2, Open Question 2 — the argv cross-check is a *secondary*
    /// confirmation, so a candidate that is structurally busy must still be
    /// vetoed when a shape is supplied and does not match, and admitted when it
    /// does. `tests/shell_activity.rs` (`status/shell-activity/003`) pins the
    /// structural half with the cross-check disabled; this pins the half that
    /// switches on.
    #[test]
    fn the_argv_cross_check_vetoes_a_structurally_busy_descendant_that_does_not_match() {
        let table = vec![
            row(100, 1, 100, "claude --model opus"),
            row(200, 100, 250, "some-unmeasured-detached-thing"),
        ];
        assert_eq!(
            descendant_shell_activity(&table, 100, &[]),
            Some(true),
            "with no shapes the structural test alone must classify the detached descendant as busy"
        );
        assert_eq!(
            descendant_shell_activity(&table, 100, &[CLAUDE_BASH_TOOL_SHAPE]),
            Some(false),
            "a supplied shape the descendant does not carry must veto the structural match"
        );
    }

    /// Scenario: issue #644 — a `wrap`-spawned agent whose primary child
    /// gets its own fresh PTY session must not be classified as "busy" by
    /// that fact alone, but a genuinely detached descendant further down
    /// the tree must still read busy. Mirrors the real `ps -e -o
    /// pid,ppid,sid,tty,args` capture in `.dot-agent-deck/644-diagnosis.md`
    /// against a live, healthy (never actually busy) Codex pane: `wrap` is
    /// its own session leader; its primary child — the `node`/`codex`
    /// process — leads the NEW pty session `wrap` allocated for it (an
    /// ordinary consequence of how `wrap` spawns any interactive child, not
    /// evidence of a detached foreground command); the codex binary child
    /// stays in that same session. No shape is supplied (`&[]`), matching
    /// production: Codex has no entry in `MEASURED_SHELL_TOOL_SHAPES`, so
    /// `shell_tool_shape_key` hands the daemon an empty shape list and the
    /// argv veto never engages for this agent kind.
    #[test]
    fn wrap_primary_child_session_change_is_not_busy_but_a_deeper_detached_descendant_still_is() {
        let healthy_codex_pane = vec![
            row(
                707387,
                1,
                707387,
                "dot-agent-deck wrap --agent codex -- codex --model gpt-5.6-terra",
            ),
            row(
                707563,
                707387,
                707563,
                "node /home/linuxbrew/.linuxbrew/bin/codex --model gpt-5.6-terra",
            ),
            row(
                707572,
                707563,
                707563,
                "codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex --model gpt-5.6-terra",
            ),
        ];
        assert_eq!(
            descendant_shell_activity(&healthy_codex_pane, 707387, &[]),
            Some(false),
            "wrap's own primary child re-leading the new pty session it was spawned into is not \
             evidence of a detached foreground command (issue #644) — this is the false-positive \
             that permanently stuck a healthy Codex pane at Working"
        );

        // Same tree, plus a genuinely detached descendant one session-id
        // divergence further down — what an actually backgrounded command
        // looks like. This must still read busy, or a fix would have blinded
        // the discriminator to every session-id mismatch rather than just
        // the primary-child one.
        let mut with_detached_grandchild = healthy_codex_pane;
        with_detached_grandchild.push(row(707600, 707572, 707600, "ping -c 200 127.0.0.1"));
        assert_eq!(
            descendant_shell_activity(&with_detached_grandchild, 707387, &[]),
            Some(true),
            "a genuinely detached descendant further down the tree must still be reported busy"
        );
    }

    /// Scenario: issue #644 round 2 (reviewer/auditor finding F1) — in
    /// production `wrap_launch_command`'s output is always multi-word, so
    /// `agent_pty::spawn` always routes it through `$SHELL -c '<wrap
    /// invocation>'` (`command_needs_shell_wrap`, `platform/shell.rs`), never
    /// the `wrap` binary directly. Whether that shell exec-replaces itself
    /// and hands `root_pid` the `wrap` argv, or survives as its own process
    /// with `wrap` one hop further down, depends on the shell and the exact
    /// command shape (dash never exec-optimizes; bash's behavior varies by
    /// command shape — both reviewer and auditor measured real cases where
    /// the shell survives). When the shell survives: `root_pid` is the
    /// shell itself (`-c` as its second whitespace token, so
    /// `argv_is_wrap_invocation` returns false), `wrap` is root's direct
    /// child and stays in root's session (a plain non-exec'd `-c`
    /// invocation doesn't allocate a new session — only `wrap` itself does
    /// that, for the child *it* spawns), and the agent's primary child sits
    /// two hops below root, in the fresh session `wrap` allocated for it.
    /// The #644 exemption is keyed on `parent_is_root`, so it never fires on
    /// this shape and the pane sticks at `Working` exactly as before the
    /// round-1 fix — silently. (`src/spawn.rs:1105` pins `SHELL=/bin/sh` for
    /// every multi-word scheduler/issue-dispatch/orchestration-role command,
    /// so this is not a hypothetical topology.)
    #[test]
    fn wrap_spawned_via_a_surviving_shell_grandchild_session_change_is_not_busy_but_a_deeper_detached_descendant_still_is()
     {
        let shell = 800100;
        let wrap = 800101;
        let node_codex = 800163;
        let codex_bin = 800172;

        let healthy_codex_pane_behind_a_surviving_shell = vec![
            row(
                shell,
                1,
                shell,
                "/bin/sh -c dot-agent-deck wrap --agent codex -- codex --model gpt-5.6-terra",
            ),
            row(
                wrap,
                shell,
                shell,
                "dot-agent-deck wrap --agent codex -- codex --model gpt-5.6-terra",
            ),
            row(
                node_codex,
                wrap,
                node_codex,
                "node /home/linuxbrew/.linuxbrew/bin/codex --model gpt-5.6-terra",
            ),
            row(
                codex_bin,
                node_codex,
                node_codex,
                "codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex --model gpt-5.6-terra",
            ),
        ];
        assert_eq!(
            descendant_shell_activity(&healthy_codex_pane_behind_a_surviving_shell, shell, &[]),
            Some(false),
            "wrap's primary child re-leading the new pty session it was spawned into is not \
             evidence of a detached foreground command even when the pane's `$SHELL -c` \
             invocation survives instead of exec-replacing itself — currently this reads busy \
             because the #644 exemption only ever checks root_pid's own argv, and root_pid here \
             is the shell, not wrap (issue #644 round 2, reviewer/auditor finding F1)"
        );

        // A genuinely detached descendant one session-id divergence further
        // down must still read busy, so a fix that relaxes the depth
        // restriction cannot simply exempt everything under a wrap-rooted
        // tree regardless of how deep it goes.
        let mut with_detached_grandchild = healthy_codex_pane_behind_a_surviving_shell;
        with_detached_grandchild.push(row(800200, codex_bin, 800200, "ping -c 200 127.0.0.1"));
        assert_eq!(
            descendant_shell_activity(&with_detached_grandchild, shell, &[]),
            Some(true),
            "a genuinely detached descendant further down the tree must still be reported busy \
             even once the shell-wrapped topology is exempted"
        );
    }

    /// Scenario: issue #644 round 2 (reviewer F3 / auditor F2) —
    /// `argv_is_wrap_invocation` only checks that a command line's second
    /// whitespace token is literally `wrap`; unlike `wrap.rs::is_wrap_invocation`
    /// it does not validate that the first token is actually this deck's own
    /// binary. A pane whose user-configured command happens to carry `wrap`
    /// as its second word for an unrelated reason must not get the #644
    /// exemption for its direct children.
    #[test]
    fn an_argv_that_merely_contains_wrap_as_its_second_token_must_not_exempt_a_detached_child() {
        let root = 800300;
        let detached_child = 800301;
        let lookalike_pane = vec![
            row(
                root,
                1,
                root,
                "git wrap --agent codex -- codex --model gpt-5.6-terra",
            ),
            row(
                detached_child,
                root,
                detached_child,
                "codex --model gpt-5.6-terra",
            ),
        ];
        assert_eq!(
            descendant_shell_activity(&lookalike_pane, root, &[]),
            Some(true),
            "root's argv is not actually a dot-agent-deck wrap invocation — it merely has the \
             literal token 'wrap' in the second position — so its direct child re-leading a new \
             session is genuine evidence of a detached foreground command, not the #644 \
             exemption's targeted shape (issue #644 round 2, reviewer F3 / auditor F2)"
        );
    }

    /// The two measured Claude Bash-tool variants: the usual one carrying the
    /// shell-snapshot `source` prologue, and the no-snapshot one where that
    /// segment is absent from the command string entirely.
    #[test]
    fn the_claude_shape_matches_both_measured_bash_tool_variants() {
        let with_snapshot = "/bin/zsh -c source /Users/x/.claude/shell-snapshots/snapshot-zsh-1785985362201-zal44i.sh 2>/dev/null || true && eval 'ping -c 200 127.0.0.1'";
        let without_snapshot = "/bin/zsh -c -l { \\builtin unalias -- 'unsetenv'; } >/dev/null 2>&1 || true && eval 'ping -c 200 127.0.0.1'";
        assert!(CLAUDE_BASH_TOOL_SHAPE.matches(with_snapshot));
        assert!(CLAUDE_BASH_TOOL_SHAPE.matches(without_snapshot));
        assert!(!CLAUDE_BASH_TOOL_SHAPE.matches("npm exec @upstash/context7-mcp"));
        assert!(
            !CLAUDE_BASH_TOOL_SHAPE.matches(""),
            "an empty command line must never match — the cross-check has to be evidence"
        );
    }

    /// `None` is not `Some(false)`: a pid the table does not contain, or one
    /// whose own session id could not be read, means "no answer", so the caller
    /// can leave the pane's status alone instead of asserting it is idle.
    #[test]
    fn an_unanswerable_table_reports_none_rather_than_idle() {
        let table = vec![row(100, 1, 100, "claude")];
        assert_eq!(descendant_shell_activity(&table, 999, &[]), None);

        let unreadable = vec![ProcessInfo {
            session_id: -1,
            ..row(100, 1, 100, "claude")
        }];
        assert_eq!(descendant_shell_activity(&unreadable, 100, &[]), None);
    }

    /// A descendant whose own session id could not be read is unclassifiable,
    /// not "in a different session" — otherwise a process exiting during the
    /// sample would read as a false `Working`. Fork issue #160's note on
    /// #216 (and #216 itself) named the follow-on failure this earlier
    /// version of the test missed: an unclassifiable candidate was folded
    /// into "not busy" rather than "unknown", so when it is the *only*
    /// candidate the function fell through to a confident `Some(false)` — a
    /// real `ShellIdle`, one level below the per-sample fail-safe PR #206
    /// already closed. The fail-safe has to be a **per-row** property, not
    /// only a per-sample one, or an unreadable row still gets silently
    /// counted as "confirmed idle".
    ///
    /// Scenario: two synthetic tables share a root process. In the first, the
    /// root's only candidate descendant has an unreadable session id, so the
    /// discriminator must report `None` (unknown) rather than asserting the
    /// pane is idle. In the second, every descendant has a validly-read
    /// session id that genuinely matches the root's own — the discriminating
    /// case that must still resolve to `Some(false)`, so a fix cannot pass by
    /// simply returning `None` whenever any row exists.
    #[spec("status/shell-activity/011")]
    #[test]
    fn shell_activity_011_an_unreadable_candidate_session_id_is_unknown_not_a_confident_idle() {
        let only_candidate_unreadable = vec![
            row(100, 1, 100, "claude"),
            ProcessInfo {
                session_id: -1,
                ..row(200, 100, 200, "gone-during-the-sample")
            },
        ];
        assert_eq!(
            descendant_shell_activity(&only_candidate_unreadable, 100, &[]),
            None,
            "the only candidate's session id could not be read, so the discriminator must \
             report unknown rather than asserting the pane is idle — a confident `Some(false)` \
             here is a false negative, the worse failure direction per \
             docs/develop/shell-activity-signal.md"
        );

        let every_candidate_confirmed_same_session = vec![
            row(100, 1, 100, "claude"),
            row(200, 100, 100, "a-well-behaved-mcp-server"),
            row(300, 200, 100, "another-well-behaved-child"),
        ];
        assert_eq!(
            descendant_shell_activity(&every_candidate_confirmed_same_session, 100, &[]),
            Some(false),
            "every descendant's session id was validly read and genuinely matches the root's \
             own, so this must still resolve to a confident idle — the discriminating case that \
             keeps the fix from passing by simply never answering"
        );
    }

    /// Route A's bulk parsing surface: three whitespace-free columns, with
    /// `??`/`?` recognised as "no controlling terminal" on macOS/Linux
    /// respectively and `session_leader` derived from the session id. Issue #862
    /// removed the fourth column, so anything trailing the tty is ignored rather
    /// than stored, and every row reports `NotSampled`.
    #[test]
    fn parse_ps_table_reads_the_three_columns_and_samples_no_command_line() {
        let stdout = concat!(
            "  100     1 ttys014\n",
            " 200   100 ??\n",
            "  300     1 ?\n",
            // A stray trailing column must not become a command line: the bulk
            // phase asked for none, so recording one would let a `Read` value
            // appear for a process nothing wanted an argv for.
            "  400     1 ttys014  claude --model opus\n",
            "not a process row\n",
            "\n",
        );
        let rows = parse_ps_table(stdout, &|pid| if pid == 200 { 200 } else { 100 });
        assert_eq!(rows.len(), 4, "{rows:#?}");

        assert_eq!(rows[0].pid, 100);
        assert_eq!(rows[0].ppid, 1);
        assert!(rows[0].has_controlling_tty);
        assert!(rows[0].session_leader, "getsid(100) == 100");

        assert!(!rows[1].has_controlling_tty, "macOS prints ?? for no ctty");
        assert!(rows[1].session_leader, "getsid(200) == 200");

        assert!(!rows[2].has_controlling_tty, "Linux prints ? for no ctty");
        assert!(!rows[2].session_leader, "getsid(300) == 100 != 300");

        assert_eq!(rows[3].pid, 400);
        for r in &rows {
            assert_eq!(
                r.command_line,
                CommandLine::NotSampled,
                "the bulk phase reads no command line at all: {r:#?}"
            );
        }
    }

    /// Fork issue #30 — the PID-reuse invariant: a `getsid` answer survives only
    /// when the second `ps` sample still describes the same process. A row whose
    /// `ppid` moved, whose controlling-tty presence changed, or that vanished
    /// entirely has its session id reset to the unreadable sentinel, while an
    /// untouched row keeps the session id that was read for it. (Narrowed by
    /// issue #862 from a whole-command-line comparison to `ppid` + tty
    /// presence — see this function's own doc comment.)
    #[test]
    fn an_unconfirmed_row_loses_its_session_id_but_keeps_its_edge() {
        let mut rows = vec![
            row(100, 1, 100, "claude --model opus"),
            row(200, 100, 250, "recycled-into-a-different-parent"),
            row(300, 100, 350, "argv-rewritten-under-us"),
            row(400, 100, 450, "exited-between-the-two-samples"),
        ];
        let confirm = concat!(
            "100     1 ttys014  claude --model opus\n",
            // pid 200 reappears under a different parent — recycled.
            "200    999 ttys014  recycled-into-a-different-parent\n",
            // pid 300 kept its parent but lost its controlling tty — a
            // different process, per this function's narrowed (post-#862)
            // signal.
            "300    100 ??  something-completely-different\n",
            // pid 400 is simply gone.
        );
        invalidate_unconfirmed_session_ids(&mut rows, confirm);

        assert_eq!(
            rows[0].session_id, 100,
            "an unchanged row must keep its sid"
        );
        assert!(rows[0].session_leader);
        for unconfirmed in &rows[1..] {
            assert_eq!(
                unconfirmed.session_id, -1,
                "an unconfirmed row must lose its getsid answer: {unconfirmed:?}"
            );
            assert!(!unconfirmed.session_leader, "{unconfirmed:?}");
            assert_eq!(
                unconfirmed.ppid, 100,
                "the pid/ppid edge must survive so the descendant walk still reaches below it"
            );
        }
    }

    /// The consequence that matters: a descendant whose identity could not be
    /// confirmed must not be able to invent a busy reading. Before the
    /// confirmation pass the same table classifies the pane as busy; after it,
    /// the pane must report unknown rather than trusting a `getsid` answer
    /// that may describe an unrelated, recycled process.
    ///
    /// Fork issue #160's per-row fail-safe (`status/shell-activity/011`)
    /// changed this test's own expected outcome: `invalidate_unconfirmed_session_ids`
    /// resets the unconfirmed row's session id to the same `-1` sentinel a
    /// failed `getsid` produces, and pid 200 is the pane's *only* candidate —
    /// so this is exactly the per-row fail-safe's scenario, reached via the
    /// confirmation pass instead of a raw unreadable `getsid`. It used to
    /// assert `Some(false)`, which was the bug: a confident idle reading built
    /// entirely on a descendant nobody could confirm. The corrected contract
    /// is `None` — "no answer available" — never `Some(false)`.
    #[test]
    fn an_unconfirmed_descendant_cannot_flip_a_pane_to_busy() {
        let mut rows = vec![
            row(100, 1, 100, "claude --model opus"),
            row(200, 100, 250, "detached-or-recycled"),
        ];
        assert_eq!(
            descendant_shell_activity(&rows, 100, &[]),
            Some(true),
            "the unfiltered table reads busy — this is what the confirmation pass has to override"
        );

        let confirm = "100     1 ttys014  claude --model opus\n";
        invalidate_unconfirmed_session_ids(&mut rows, confirm);
        assert_eq!(
            descendant_shell_activity(&rows, 100, &[]),
            None,
            "a descendant the second sample could not confirm must not count as busy evidence \
             — and, being the pane's only candidate, must not be read as confirmed idle either"
        );
    }

    /// A row with no command line at all must still be kept: the descendant
    /// walk needs its `pid`/`ppid` edge even when there is no argv to read.
    #[test]
    fn parse_ps_table_keeps_a_row_with_nothing_after_the_tty_column() {
        let rows = parse_ps_table("  42     1 ??\n", &|_| 42);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 42);
        assert_eq!(rows[0].ppid, 1);
        assert_eq!(rows[0].command_line, CommandLine::NotSampled);
    }

    /// Issue #862, the argv phase's parsing surface: `pid` then the whole
    /// command line, interior double spaces and quotes intact, with a pid the
    /// output never mentions simply absent from the map.
    #[test]
    fn parse_ps_command_lines_keeps_each_command_line_whole() {
        let stdout = concat!(
            " 200 /bin/zsh -c source a/shell-snapshots/snapshot-zsh-1.sh && eval 'x  y'\n",
            "  42\n",
            "not a row\n",
            "\n",
        );
        let map = parse_ps_command_lines(stdout);
        assert_eq!(map.len(), 2, "{map:#?}");
        assert_eq!(
            map[&200], "/bin/zsh -c source a/shell-snapshots/snapshot-zsh-1.sh && eval 'x  y'",
            "the command line must survive whole, interior double spaces included"
        );
        assert!(CLAUDE_BASH_TOOL_SHAPE.matches(&map[&200]));
        assert_eq!(
            map[&42], "",
            "a pid with no argv is a real answer, not a missing one"
        );
        assert!(!map.contains_key(&999));
    }

    /// Issue #862 — the invariant the two-phase sample rests on: the set of pids
    /// whose command line the sampler reads is EXACTLY the set the classifier
    /// consults, because both are `shell_tool_candidates`. Asserted directly, so
    /// a future change that widens one without the other fails here rather than
    /// silently suppressing a pane's signal.
    #[test]
    fn command_line_targets_are_exactly_the_shell_tool_candidates() {
        let table = vec![
            // Two panes' shells, each with an in-session child (an MCP server
            // shape) and a detached one (a Bash-tool shape).
            row(100, 1, 100, "claude --model opus"),
            row(101, 100, 100, "npm exec @upstash/context7-mcp"),
            row(102, 100, 5102, "detached under pane one"),
            row(200, 1, 200, "claude --model opus"),
            row(201, 200, 200, "caffeinate -i -t 300"),
            row(202, 200, 5202, "detached under pane two"),
            // An unrelated process on the machine, in its own session. Not a
            // descendant of either root, so no phase may ever read its argv —
            // this is the whole point of the change.
            row(900, 1, 900, "some unrelated wedged linker"),
        ];
        assert_eq!(shell_tool_candidates(&table, 100).unwrap(), vec![102]);
        assert_eq!(shell_tool_candidates(&table, 200).unwrap(), vec![202]);
        assert_eq!(command_line_targets(&table, &[100, 200]), vec![102, 202]);
        assert_eq!(
            command_line_targets(&table, &[]),
            Vec::<i32>::new(),
            "no roots means no argv read at all"
        );
        // A root the table cannot answer for costs the others nothing.
        assert_eq!(command_line_targets(&table, &[100, 4242]), vec![102]);
    }

    /// Issue #862 — the narrowing that makes the two-phase sample worth having.
    /// `setsid()` creates a session and everything below the caller inherits it,
    /// so a Bash-tool call that runs a build has the WHOLE build tree in a
    /// different session than the agent. `detached_descendants` reports all of
    /// it (the structural test wants that — any one of them means busy), but the
    /// argv cross-check must only ever read the one process at the session
    /// boundary, because reading the rest would put the poll back to touching
    /// the `mmap_lock` of exactly the `ld` processes issue #862 is about.
    #[test]
    fn shell_tool_candidates_stop_at_the_session_boundary() {
        const AGENT: i32 = 100;
        const AGENT_SID: i32 = 100;
        const TOOL_SID: i32 = 5000;
        let table = vec![
            row(AGENT, 1, AGENT_SID, "claude --model opus"),
            // An in-session child, as every measured MCP server was.
            row(101, AGENT, AGENT_SID, "npm exec @upstash/context7-mcp"),
            // The `setsid`-ed Bash-tool shell: the boundary, and the only
            // process that carries the measured shape.
            row(
                200,
                AGENT,
                TOOL_SID,
                "/bin/zsh -c source x/shell-snapshots/snapshot-z.sh && eval 'cargo build'",
            ),
            // Its build tree. All in TOOL_SID — inherited, not created — so all
            // are detached relative to the agent and NONE is a boundary.
            row(201, 200, TOOL_SID, "cargo build"),
            row(202, 201, TOOL_SID, "rustc --crate-name a"),
            row(203, 202, TOOL_SID, "ld -z relro -o a"),
        ];

        assert_eq!(
            detached_descendants(&table, AGENT).unwrap(),
            vec![200, 201, 202, 203],
            "the structural test sees the whole detached subtree, and should: any one \
             of these existing means the pane is busy"
        );
        assert_eq!(
            shell_tool_candidates(&table, AGENT).unwrap(),
            vec![200],
            "but only the process at the session boundary can carry a shell-tool shape, \
             so it is the only one whose command line is ever read — reading `cargo`, \
             `rustc` and `ld` would be both useless and the whole cost issue #862 removed"
        );
        assert_eq!(
            command_line_targets(&table, &[AGENT]),
            vec![200],
            "and the sampler asks for exactly that one"
        );
        // The outcome is unchanged by the narrowing: the shape is on the
        // boundary process, so the cross-check still finds it.
        assert_eq!(
            descendant_shell_activity(&table, AGENT, &[CLAUDE_BASH_TOOL_SHAPE]),
            Some(true)
        );

        // A process that `setsid`s itself DEEP inside the tree is a boundary of
        // its own and stays a candidate — that is PRD #386's false-positive
        // risk, and this narrowing must not hide it.
        let mut nested = table.clone();
        nested.push(row(300, 202, 9000, "something that detached itself"));
        assert_eq!(
            shell_tool_candidates(&nested, AGENT).unwrap(),
            vec![200, 300],
            "a process that starts its own session deep inside the tree is a boundary of \
             its own, so the narrowing cannot hide PRD #386's false-positive risk"
        );
    }

    /// Issue #862 — a detached descendant whose command line was never sampled
    /// must NOT be read as busy. The cross-check is evidence; inventing a match
    /// from a command line nobody read would pin the pane at `Working` forever,
    /// which the PRD calls worse than the stale `Idle` it replaces.
    #[test]
    fn a_detached_descendant_with_no_sampled_command_line_is_not_busy() {
        let mut table = vec![
            row(100, 1, 100, "claude --model opus"),
            row(200, 100, 250, "the bash-tool child"),
        ];
        table[1].command_line = CommandLine::NotSampled;
        assert_eq!(
            descendant_shell_activity(&table, 100, &[CLAUDE_BASH_TOOL_SHAPE]),
            Some(false)
        );
        table[1].command_line = CommandLine::Unavailable;
        assert_eq!(
            descendant_shell_activity(&table, 100, &[CLAUDE_BASH_TOOL_SHAPE]),
            Some(false),
            "a process that exited between the two phases is not running anything"
        );
        // The structural test alone is unaffected: it reads no command line, so
        // a pane whose agent kind was never measured costs no argv read and is
        // still classified.
        assert_eq!(descendant_shell_activity(&table, 100, &[]), Some(true));
    }

    /// Issue #862 — `fill_command_lines` records the argv phase's answers on the
    /// wanted pids only, distinguishing "read it" from "wanted it and it was
    /// gone", and leaves every other row's `NotSampled` untouched.
    #[test]
    fn fill_command_lines_marks_wanted_rows_and_leaves_the_rest_not_sampled() {
        let mut table = vec![
            row(100, 1, 100, "claude"),
            row(102, 100, 5102, "placeholder"),
            row(900, 1, 900, "unrelated"),
        ];
        for r in table.iter_mut() {
            r.command_line = CommandLine::NotSampled;
        }
        let resolved = HashMap::from([(102, "the real bash child".to_string())]);
        fill_command_lines(&mut table, &[102, 103], &resolved);
        assert_eq!(
            table[1].command_line,
            CommandLine::Read("the real bash child".to_string())
        );
        assert_eq!(table[0].command_line, CommandLine::NotSampled);
        assert_eq!(
            table[2].command_line,
            CommandLine::NotSampled,
            "an unrelated process's command line is never read, at either phase"
        );

        // A wanted pid the argv phase could not answer for.
        let mut table = vec![row(102, 100, 5102, "placeholder")];
        table[0].command_line = CommandLine::NotSampled;
        fill_command_lines(&mut table, &[102], &HashMap::new());
        assert_eq!(table[0].command_line, CommandLine::Unavailable);
    }
}
