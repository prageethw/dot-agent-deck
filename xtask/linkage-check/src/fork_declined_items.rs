//! Check 16: the three upstream governance/infra changes the maintainer
//! reviewed and explicitly declined post-rebase during the 14th fork/upstream
//! sync must not silently resurface.
//!
//! `docs/develop/fork-sync-workflow.md`'s "Re-curation and rebase history"
//! section records the decision and its reasoning in full — read it there
//! before touching anything this check names. In short: (1) upstream's
//! automated PR-review bot
//! (`.github/workflows/pr-review-batch.yml`/`pr-review.lock.yml`/
//! `pr-review.md`, `.github/pr-review-rubric.md`, plus its implementation —
//! `.github/scripts/pr_review_common.py`, `pr_review_select.py`,
//! `pr_review_vote.py`, and the `xtask/linkage-check` test module gating
//! them, `pr_review_verdict.rs`, all missed by the original strip and
//! removed only in a fix-round commit after the 14th sync) contradicts
//! CLAUDE.md rule 8's fork policy that no bot reviews this fork's PRs; (2) the
//! orchestrator's step-1 human plan-approval gate — the literal sentence this
//! check pins in `.dot-agent-deck.toml` — is the standing gate CLAUDE.md rule
//! 27 relies on, and upstream's rebase carries a hunk that removes it in
//! favour of "keep the plan internal, do not wait for sign-off, go straight
//! to execution", bundled with a `release`-role hunk pointing at a new
//! `/pr-create` skill this fork declined in place of `/prd-done`; (3)
//! upstream's tag-release ruleset-bypass-avoidance automation
//! (`.github/workflows/tag-release.yml`, `.claude/skills/tag-release/`)
//! solves a problem this fork's own `main` does not have, since it carries no
//! branch-protection ruleset at all.
//!
//! The forcing reason this is a `linkage-check` rule and not only a doc note:
//! all three items came in on the sync's own rebase as clean, non-conflicting
//! adds — nothing forced a human to look at them, so nothing stops a *future*
//! sync's rebase from quietly re-adding them with no prompt to notice. A
//! mechanical, load-bearing local gate (CLAUDE.md rule 2) is the only thing
//! that reliably survives a sync a human did not read line by line.
//!
//! Scoped narrowly to exactly the paths and the one sentence named above —
//! this is a regression tripwire over a specific, recorded decision, not a
//! general policy engine. A future sync that widens or narrows the declined
//! set updates this list in the same commit that updates the doc section it
//! cross-references.

use std::path::{Path, PathBuf};

/// Files whose mere existence in the working tree means one of the three
/// declined items resurfaced.
const FORBIDDEN_FILES: &[&str] = &[
    ".github/workflows/pr-review-batch.yml",
    ".github/workflows/pr-review.lock.yml",
    ".github/workflows/pr-review.md",
    ".github/pr-review-rubric.md",
    ".github/workflows/tag-release.yml",
    ".github/scripts/pr_review_common.py",
    ".github/scripts/pr_review_select.py",
    ".github/scripts/pr_review_vote.py",
    "xtask/linkage-check/src/pr_review_verdict.rs",
];

/// Directories whose mere existence — of any file under them, not just the
/// bare directory — means one of the three declined items resurfaced.
const FORBIDDEN_DIRS: &[&str] = &[".claude/skills/tag-release", ".claude/skills/pr-create"];

/// The step-1 human plan-approval gate sentence (CLAUDE.md rule 27's standing
/// gate). Upstream's declined rebase hunk replaces this instruction with "keep
/// the plan internal, do not wait for sign-off, go straight to execution" —
/// so its absence from `.dot-agent-deck.toml` is itself a sign item (2)
/// resurfaced, even without the literal file being restored.
const GATE_SENTENCE: &str = "Surface the plan to the user as a Markdown table and STOP";

/// Repo-relative path of the config file the gate sentence lives in.
const CONFIG_PATH: &str = ".dot-agent-deck.toml";

/// Quoted in every failure so the reader lands on the reasoning immediately
/// instead of having to guess why this check exists.
const POINTER: &str = "see docs/develop/fork-sync-workflow.md's \"Re-curation and rebase history\" \
     section (the 14th-sync declined-items entry) for what this was, why it was declined, and why \
     this check exists";

/// Run the rule over the checkout at `root`. Returns rendered failures; an
/// empty vector is a pass.
pub fn run(root: &Path) -> Vec<String> {
    let mut failures = Vec::new();

    for rel in FORBIDDEN_FILES {
        if root.join(rel).is_file() {
            failures.push(format!("{rel} exists — {POINTER}"));
        }
    }

    for rel in FORBIDDEN_DIRS {
        if let Some(file) = first_file_under(&root.join(rel)) {
            let shown = file.strip_prefix(root).unwrap_or(&file).display();
            failures.push(format!("{rel} contains {shown} — {POINTER}"));
        }
    }

    let config_path = root.join(CONFIG_PATH);
    match std::fs::read_to_string(&config_path) {
        Ok(text) => {
            if !text.contains(GATE_SENTENCE) {
                failures.push(format!(
                    "{CONFIG_PATH} does not contain the literal sentence {GATE_SENTENCE:?} — the \
                     step-1 human plan-approval gate looks altered or removed — {POINTER}"
                ));
            }
        }
        Err(e) => failures.push(format!(
            "failed to read {CONFIG_PATH}: {e} — cannot verify the step-1 plan-approval gate \
             sentence is intact — {POINTER}"
        )),
    }

    failures
}

/// The first regular file found under `dir`, walked depth-first in a
/// deterministic (sorted per directory) order. `None` if `dir` does not exist
/// or contains no file at all — a bare, empty directory is not itself a
/// resurrection of the declined skill.
fn first_file_under(dir: &Path) -> Option<PathBuf> {
    if dir.is_file() {
        return Some(dir.to_path_buf());
    }
    if !dir.is_dir() {
        return None;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            if child.is_dir() {
                stack.push(child);
            } else {
                return Some(child);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rule 5's note: linkage-check's assertions are RUNTIME-only, so a rule
    /// gutted to `Vec::new()` needs a test that catches it going quiet.
    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn minimal_tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            CONFIG_PATH,
            "# fixture\n[orchestrator]\nstep_1 = \"Surface the plan to the user as a Markdown table and STOP.\"\n",
        );
        tmp
    }

    #[test]
    fn a_clean_tree_passes() {
        let tmp = minimal_tree();
        assert_eq!(run(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn a_resurrected_forbidden_file_fails() {
        let tmp = minimal_tree();
        write(
            tmp.path(),
            ".github/workflows/tag-release.yml",
            "name: tag-release\n",
        );
        let failures = run(tmp.path());
        assert_eq!(failures.len(), 1, "got {failures:?}");
        assert!(failures[0].contains(".github/workflows/tag-release.yml"));
        assert!(failures[0].contains("fork-sync-workflow.md"));
    }

    #[test]
    fn each_forbidden_file_is_individually_caught() {
        for rel in FORBIDDEN_FILES {
            let tmp = minimal_tree();
            write(tmp.path(), rel, "resurrected\n");
            let failures = run(tmp.path());
            assert!(
                failures.iter().any(|f| f.contains(rel)),
                "expected a failure naming {rel}, got {failures:?}"
            );
        }
    }

    #[test]
    fn a_resurrected_forbidden_directory_fails_only_once_it_has_a_file() {
        let tmp = minimal_tree();
        let dir = tmp.path().join(".claude/skills/pr-create");
        std::fs::create_dir_all(&dir).unwrap();
        // An empty directory alone is not a resurrection.
        assert_eq!(run(tmp.path()), Vec::<String>::new());

        write(
            tmp.path(),
            ".claude/skills/pr-create/SKILL.md",
            "name: pr-create\n",
        );
        let failures = run(tmp.path());
        assert_eq!(failures.len(), 1, "got {failures:?}");
        assert!(failures[0].contains(".claude/skills/pr-create"));
    }

    #[test]
    fn each_forbidden_directory_is_individually_caught() {
        for rel in FORBIDDEN_DIRS {
            let tmp = minimal_tree();
            write(tmp.path(), &format!("{rel}/SKILL.md"), "resurrected\n");
            let failures = run(tmp.path());
            assert!(
                failures.iter().any(|f| f.contains(rel)),
                "expected a failure naming {rel}, got {failures:?}"
            );
        }
    }

    #[test]
    fn a_removed_gate_sentence_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            CONFIG_PATH,
            "# fixture\n[orchestrator]\nstep_1 = \"keep the plan internal, do not wait for sign-off, go straight to execution\"\n",
        );
        let failures = run(tmp.path());
        assert_eq!(failures.len(), 1, "got {failures:?}");
        assert!(failures[0].contains(CONFIG_PATH));
        assert!(failures[0].contains("plan-approval gate"));
    }

    #[test]
    fn a_missing_config_file_fails_rather_than_silently_passing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let failures = run(tmp.path());
        assert_eq!(failures.len(), 1, "got {failures:?}");
        assert!(failures[0].contains("failed to read"));
    }
}
