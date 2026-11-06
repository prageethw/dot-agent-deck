//! Issue #1610: a skill that merges a PR first checks whether `main` gained
//! commits touching the PR's files since its CI ran.
//!
//! The ruleset does not require a branch to be up to date before it merges, so
//! two PRs that are each green merged on 2026-10-04 and again on 2026-10-05
//! and left `main` unable to compile its tests. The fix is a procedure, not
//! code: `.claude/skills/merge-pr/SKILL.md` defines "The overlap check before
//! a merge" once, and the skills that hand a merge to `merge-pr` go through it.
//! Prose has no compiler, so an edit that drops the section's commands, or the
//! pointer in a skill that merges through `merge-pr`, would lose the check
//! without anything going red. These tests are that red.
//!
//! **What this does NOT claim.** It reads text. It checks that the defining
//! section still carries its commands and that `code-cleanup`, `issue-queue`
//! and `land-prs` still hand their merge to `merge-pr`; it does not check that
//! an agent follows the commands, and it does not look at merges written some
//! other way than `gh pr merge`.
//!
//! **Fork difference.** Upstream also asserted that every skill running
//! `gh pr merge` without `--auto` names the section (`merge_overlap_check_003`)
//! and that CLAUDE.md's "Answer AND Resolve Review Findings" rule points a
//! person merging by hand at it (`merge_overlap_check_004`). This fork dropped
//! both (maintainer decision 2026-11-06, CLAUDE.md rule 24: the fork's way):
//! `close-pending-prs` merges under CLAUDE.md rule 25's checklist, which has
//! no overlap-check step, and this fork's rule 8 is a different rule.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use regex::Regex;

    /// The heading of the section every merging skill must point at.
    const SECTION: &str = "The overlap check before a merge";

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("xtask/linkage-check sits two levels below the workspace root")
            .to_path_buf()
    }

    /// Whether `text` tells an agent to merge a PR directly: a `gh pr merge`
    /// command, inside one inline code span or one line of a code block, that
    /// does not carry `--auto`.
    fn merges_directly(text: &str) -> bool {
        let command = Regex::new(r"gh pr merge[^`\n]*").expect("static regex");
        command
            .find_iter(text)
            .any(|m| !m.as_str().contains("--auto"))
    }

    #[test]
    fn merge_overlap_check_001_only_a_merge_without_auto_counts() {
        assert!(merges_directly("then `gh pr merge <n> --squash`"));
        assert!(merges_directly(
            "```bash\ngh pr merge <n> --squash --match-head-commit \"$head\"\n```"
        ));
        assert!(merges_directly(
            "`gh pr merge <n> --squash --admin --body-file r.md`"
        ));
        assert!(!merges_directly(
            "arm it: `gh pr merge <n> --auto --squash`"
        ));
        assert!(!merges_directly("(`gh pr merge --auto`)"));
        assert!(!merges_directly("a PR is merged by a person"));
        // One direct merge among auto-merge mentions still counts.
        assert!(merges_directly(
            "`gh pr merge <n> --auto --squash`, or later `gh pr merge <n> --squash`"
        ));
    }

    #[test]
    fn merge_overlap_check_002_the_section_carries_its_commands() {
        let path = repo_root().join(".claude/skills/merge-pr/SKILL.md");
        let text = fs::read_to_string(&path).expect("read merge-pr/SKILL.md");
        let heading = format!("\n## {SECTION}\n");
        let start = text
            .find(&heading)
            .unwrap_or_else(|| panic!("merge-pr/SKILL.md has no `## {SECTION}` heading"));
        let body = &text[start + heading.len()..];
        let body = &body[..body.find("\n## ").unwrap_or(body.len())];
        for needle in [
            // a failed read stops the check instead of reading as "no overlap"
            "set -euo pipefail",
            "STOP:",
            // the PR's files, all of them and renames' old paths too
            "pulls/$n/files\" --paginate",
            "previous_filename",
            // the list is newline-delimited, so a newline inside a path stops it
            "if test(\"\\n\") then error(",
            // past the endpoint's cap the file list is incomplete
            "-le 3000",
            // commits on main since the branch point, limited to those files
            "git merge-base origin/main \"origin/pr-$n\"",
            "..origin/main\" -- \"${files[@]}\"",
            // what to do when it lists anything
            "gh pr update-branch <n>",
            "--match-head-commit",
        ] {
            assert!(
                body.contains(needle),
                "\"{SECTION}\" in merge-pr/SKILL.md no longer contains `{needle}`"
            );
        }
        // macOS's system Bash 3.2 has no `mapfile`, and the check must run there.
        assert!(
            !body.contains("mapfile"),
            "\"{SECTION}\" in merge-pr/SKILL.md uses `mapfile`, which Bash 3.2 lacks"
        );
    }

    /// The skills that merge without running `gh pr merge` themselves hand the
    /// merge to merge-pr, which is what puts the overlap check and the watch of
    /// `main`'s run after it in front of their merges.
    #[test]
    fn merge_overlap_check_005_the_skills_that_merge_go_through_merge_pr() {
        for skill in ["code-cleanup", "issue-queue", "land-prs"] {
            let text =
                fs::read_to_string(repo_root().join(format!(".claude/skills/{skill}/SKILL.md")))
                    .unwrap_or_else(|e| panic!("read {skill}/SKILL.md: {e}"));
            assert!(
                text.contains("merge-pr"),
                "{skill}/SKILL.md merges PRs but no longer hands the merge to merge-pr"
            );
        }
    }
}
