//! No source file under `src/` spells a label removal or a REST deletion
//! (PRD #421 M3.3(b)).
//!
//! Issue dispatch labels a dispatched issue `agent-deck/claimed` and writes a
//! claimant comment, and by decision nothing takes either away: not on tab
//! close, not on a re-claim. This guard is the syntactic half of holding that
//! decision: it scans the text of every file under `src/` for the spellings
//! below, so a removal written with one of them anywhere later fails here.
//!
//! It checks spellings, not behaviour. A removal built from parts at run time
//! (a method name formatted into an argv, a GraphQL document assembled from
//! fragments) carries none of these strings and passes it; the e2e tests that
//! read the GitHub stub's state after a re-claim and a tab close are what
//! observe the behaviour.

use std::path::{Path, PathBuf};

/// Spellings of a label removal through `gh`: the `issue edit` / `pr edit`
/// flag, and the GraphQL mutation `removeLabelsFromLabelable`.
const LABEL_REMOVAL: &[&str] = &["remove-label", "removeLabels"];

/// Fork-only carve-out. `worker-agent-deck issue release` (fork issue #326)
/// removes the FORK's own `in-progress` claim label through
/// `issue_edit_remove_label_argv`, defined in this file. It is a different
/// label from the `agent-deck/claimed` one this guard protects, so the guard
/// tolerates the removal flag in exactly this file's doc comments and in the
/// two argv-element lines of that builder and its shape test.
/// [`the_carve_out_covers_exactly_the_builder_and_its_shape_test`] holds the
/// file to exactly those two lines (a third spelling fails it), and
/// [`the_forks_label_removal_is_never_handed_the_claim_label`] holds the
/// other half: nothing may hand that builder the dispatch claim label.
const FORK_RELEASE_BUILDER_FILE: &str = "issue_dispatch.rs";

/// The builder the fork's release goes through.
const FORK_RELEASE_BUILDER: &str = "issue_edit_remove_label_argv";

/// The only two functions in [`FORK_RELEASE_BUILDER_FILE`] that may spell the
/// removal flag as an argv element, once each: the builder and its argv-shape
/// test. [`the_carve_out_covers_exactly_the_builder_and_its_shape_test`] holds
/// the file to this list, so a second spelling (a second builder, the flag
/// written twice, the shape test moved or deleted) fails until a human decides.
const FORK_RELEASE_EXEMPT_OWNERS: &[&str] = &[
    "issue_edit_remove_label_argv",
    "issue_edit_remove_label_argv_shape",
];

/// Whether `line` is the one argv element the builder (`"--remove-label"
/// .to_string(),`) and its shape test (`"--remove-label",`) each carry.
fn is_removal_argv_element(line: &str) -> bool {
    matches!(
        line.trim(),
        r#""--remove-label".to_string(),"# | r#""--remove-label","#
    )
}

/// Whether `line` is a comment line: prose cannot remove a label.
fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Whether `line` of `file` is one the fork-only carve-out covers. This is the
/// per-line half only: it admits the argv-element shape anywhere in the
/// builder's file, and how many of those lines there are, and in which
/// functions, is pinned by
/// [`the_carve_out_covers_exactly_the_builder_and_its_shape_test`].
fn is_fork_release_spelling(file: &Path, line: &str) -> bool {
    file.ends_with(FORK_RELEASE_BUILDER_FILE)
        && (is_comment_line(line) || is_removal_argv_element(line))
}

/// The name of the function each argv-element spelling in `text` sits in, in
/// source order: the nearest `fn <name>` above it (comment lines are skipped,
/// so prose that says "fn" cannot move the owner), or `<outside any fn>`.
fn removal_spelling_owners(text: &str) -> Vec<String> {
    let header = regex::Regex::new(r"\bfn\s+(\w+)").expect("valid pattern");
    let mut owner = String::from("<outside any fn>");
    let mut owners = Vec::new();
    for line in text.lines() {
        let declared = if is_comment_line(line) {
            None
        } else {
            header.captures(line)
        };
        if let Some(found) = declared {
            owner = found[1].to_string();
        }
        if is_removal_argv_element(line) {
            owners.push(owner.clone());
        }
    }
    owners
}

/// The index of the `)` that closes the `(` at `open` in `text`, counting
/// nested parentheses and stepping over `"..."` string literals (a `)` inside
/// one does not close anything); `None` when it never closes. Like the rest of
/// this guard it reads text, not Rust syntax: a char literal `'"'` or a raw
/// string holding a `"` is not understood.
fn matching_close(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[open..].char_indices() {
        if in_string {
            match (escaped, ch) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                // `text[open]` is the `(` that made `depth` 1, so this cannot
                // underflow.
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// One call of the fork's label-removal builder found in source text.
struct BuilderCall<'a> {
    /// The call as written (to the end of the text when it never closes).
    whole: &'a str,
    /// The text between its parentheses; `None` when they never balance.
    args: Option<&'a str>,
}

/// Every call of [`FORK_RELEASE_BUILDER`] in `text`, its definition excluded.
fn builder_calls(text: &str) -> Vec<BuilderCall<'_>> {
    let opener = format!("{FORK_RELEASE_BUILDER}(");
    text.match_indices(opener.as_str())
        // The builder's own definition, not a call of it.
        .filter(|(start, _)| !text[..*start].ends_with("fn "))
        .map(|(start, _)| {
            let open = start + opener.len() - 1;
            let close = matching_close(text, open);
            BuilderCall {
                whole: &text[start..close.map_or(text.len(), |close| close + 1)],
                args: close.map(|close| &text[open + 1..close]),
            }
        })
        .collect()
}

/// Whether a call's arguments hand the builder only the fork's own
/// `in-progress` label: it is named, and neither the dispatch claim label's
/// constant nor its literal appears. A call whose parentheses never balance
/// has no arguments to judge and is refused.
fn hands_only_the_in_progress_label(args: Option<&str>) -> bool {
    args.is_some_and(|args| {
        args.contains("IN_PROGRESS_LABEL")
            && !args.contains("CLAIM_LABEL")
            && !args.contains("agent-deck/claimed")
    })
}

/// Spellings of a REST deletion through `gh api` (or any HTTP client): the
/// method as a string literal — `"DELETE"`, which is how an argv element or a
/// method constant spells it — and the method flags written inline in a
/// string or a comment (`--method DELETE`, `--method=DELETE`, `-X DELETE`,
/// `-XDELETE`), in any letter case, since `gh` accepts the method in any case.
/// The literal is matched case-sensitively: `"delete"` and `"Delete"` are key
/// names elsewhere in `src/`.
const REST_DELETION: &[&str] = &[r#""DELETE""#, r#"(?i)(?:--method|-X)[\s=]*"?delete\b"#];

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Every file under `src/` with its text, after checking the scan reaches the
/// claim's own module (a moved tree would otherwise pass vacuously).
fn source_files() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    files_under(&root.join("src"), &mut files);
    assert!(
        files.iter().any(|f| f.ends_with("issue_dispatch_run.rs")),
        "the scan must reach the claim's own module"
    );
    files
        .into_iter()
        .filter_map(|file| {
            let text = std::fs::read_to_string(&file).ok()?;
            Some((file, text))
        })
        .collect()
}

#[test]
fn no_source_file_spells_a_label_removal() {
    let offenders: Vec<String> = source_files()
        .iter()
        .filter_map(|(file, text)| {
            let hits: Vec<&str> = LABEL_REMOVAL
                .iter()
                .copied()
                .filter(|needle| {
                    text.lines()
                        .any(|line| line.contains(needle) && !is_fork_release_spelling(file, line))
                })
                .collect();
            (!hits.is_empty()).then(|| format!("{}: {hits:?}", file.display()))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "no file under src/ may spell a label removal (PRD #421 M3.3(b)); found: {offenders:?}"
    );
}

/// Scenario: read `src/issue_dispatch.rs` and list the function each argv-element spelling of the label-removal flag sits in. Exactly two are allowed, one in the fork's `issue_edit_remove_label_argv` builder and one in its argv-shape test; a second spelling anywhere in that file, a second builder, or a missing shape test fails here, so the label guard's carve-out cannot quietly grow past the builder it was written for.
#[test]
fn the_carve_out_covers_exactly_the_builder_and_its_shape_test() {
    let builder_files: Vec<(PathBuf, String)> = source_files()
        .into_iter()
        .filter(|(file, _)| file.ends_with(FORK_RELEASE_BUILDER_FILE))
        .collect();
    assert_eq!(
        builder_files.len(),
        1,
        "exactly one file under src/ may be named {FORK_RELEASE_BUILDER_FILE}, the one the \
         carve-out names; found: {:?}",
        builder_files.iter().map(|(f, _)| f).collect::<Vec<_>>()
    );
    let mut owners = removal_spelling_owners(&builder_files[0].1);
    owners.sort();
    let mut expected: Vec<String> = FORK_RELEASE_EXEMPT_OWNERS
        .iter()
        .map(|owner| owner.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        owners, expected,
        "{FORK_RELEASE_BUILDER_FILE} may spell the label-removal flag as an argv element exactly \
         once in the builder and once in its shape test, and nowhere else (PRD #421 M3.3(b)); a \
         second spelling is a decision for a human, who then updates FORK_RELEASE_EXEMPT_OWNERS"
    );
}

/// Scenario: run the carve-out's owner finder over three small source texts, the builder with its shape test, the same plus a second builder that spells the flag too, and a builder that spells it twice. Only the first yields the two allowed owners, so the exact-match check above can tell a grown carve-out from the intended one.
#[test]
fn the_carve_out_counter_tells_a_second_spelling_from_the_two_allowed() {
    let builder = r#"
/// Prose that says fn spelled_in_a_comment must not become the owner.
pub fn issue_edit_remove_label_argv(repo: &str) -> Vec<String> {
    vec![
        "--remove-label".to_string(),
    ]
}
"#;
    let shape_test = r#"
#[test]
fn issue_edit_remove_label_argv_shape() {
    assert_eq!(x, vec![
        "--remove-label",
    ]);
}
"#;
    let second_builder = r#"
fn another_builder(repo: &str) -> Vec<String> {
    vec![
        "--remove-label".to_string(),
    ]
}
"#;
    let spelled_twice = r#"
pub fn issue_edit_remove_label_argv(repo: &str) -> Vec<String> {
    vec![
        "--remove-label".to_string(),
        "--remove-label".to_string(),
    ]
}
"#;
    assert_eq!(
        removal_spelling_owners(&format!("{builder}{shape_test}")),
        [
            "issue_edit_remove_label_argv",
            "issue_edit_remove_label_argv_shape"
        ],
        "the intended two"
    );
    assert_eq!(
        removal_spelling_owners(&format!("{builder}{shape_test}{second_builder}")),
        [
            "issue_edit_remove_label_argv",
            "issue_edit_remove_label_argv_shape",
            "another_builder"
        ],
        "a second builder is a third owner"
    );
    assert_eq!(
        removal_spelling_owners(&format!("{spelled_twice}{shape_test}")),
        [
            "issue_edit_remove_label_argv",
            "issue_edit_remove_label_argv",
            "issue_edit_remove_label_argv_shape"
        ],
        "a doubled spelling is a third owner"
    );
    assert_eq!(
        removal_spelling_owners("const FLAGS: &[&str] = &[\n    \"--remove-label\",\n];\n"),
        ["<outside any fn>"],
        "a spelling outside every function is its own owner"
    );
}

/// Scenario: scan every file under `src/` for a call of the fork's label-removal builder (the one `issue release` uses to drop its `in-progress` label). Every call must be handed that `in-progress` label and none may name the dispatch claim label `agent-deck/claimed`, however deep the arguments nest their own parentheses, so the fork-only carve-out in the label guard above cannot be used to take the claim away.
#[test]
fn the_forks_label_removal_is_never_handed_the_claim_label() {
    let mut calls = 0;
    let mut offenders = Vec::new();
    for (file, text) in source_files() {
        for call in builder_calls(&text) {
            calls += 1;
            if !hands_only_the_in_progress_label(call.args) {
                offenders.push(format!("{}: {}", file.display(), call.whole));
            }
        }
    }
    assert!(
        calls > 0,
        "the scan must find the fork's release calling its label-removal builder, or this guard \
         checks nothing"
    );
    assert!(
        offenders.is_empty(),
        "the fork's label removal may only be handed the fork's `in-progress` label, never the \
         dispatch claim label (PRD #421 M3.3(b)); found: {offenders:?}"
    );
}

/// Scenario: run the builder-call scan over small source texts whose arguments nest parentheses, hide a `)` inside a string literal, or never close, and over a function definition. A call that buries the claim label after a nested call or a string `)`, or never closes, must be refused; a call that only nests the `in-progress` label is accepted; a definition is not a call.
#[test]
fn the_call_scan_reads_through_nested_parentheses() {
    let verdicts = |source: &str| -> Vec<bool> {
        builder_calls(source)
            .iter()
            .map(|call| hands_only_the_in_progress_label(call.args))
            .collect()
    };
    assert_eq!(
        verdicts("issue_edit_remove_label_argv(repo, 7, label_of(IN_PROGRESS_LABEL))"),
        [true],
        "a nested call around the in-progress label is fine"
    );
    // The pattern this replaced, `[^)]*`, stopped at the first `)` and judged only
    // `repo, 7, pick(IN_PROGRESS_LABEL`, so the next two both passed it.
    assert_eq!(
        verdicts("issue_edit_remove_label_argv(repo, 7, pick(IN_PROGRESS_LABEL), CLAIM_LABEL)"),
        [false],
        "the claim label after a nested call is still seen"
    );
    assert_eq!(
        verdicts(
            r#"issue_edit_remove_label_argv(repo, 7, IN_PROGRESS_LABEL, ")", "agent-deck/claimed")"#
        ),
        [false],
        "a `)` inside a string literal does not end the call"
    );
    assert_eq!(
        verdicts("issue_edit_remove_label_argv(repo, 7, IN_PROGRESS_LABEL"),
        [false],
        "a call that never closes has nothing to judge and is refused"
    );
    assert_eq!(
        verdicts("issue_edit_remove_label_argv(repo, 7, CLAIM_LABEL)"),
        [false],
        "the claim label alone is refused"
    );
    assert!(
        builder_calls("pub fn issue_edit_remove_label_argv(repo: &str) -> Vec<String> { vec![] }")
            .is_empty(),
        "the builder's own definition is not a call"
    );
}

#[test]
fn no_source_file_spells_a_rest_deletion() {
    let patterns: Vec<regex::Regex> = REST_DELETION
        .iter()
        .map(|pattern| regex::Regex::new(pattern).expect("valid pattern"))
        .collect();
    // The patterns must catch every spelling they claim to.
    for spelling in [
        r#"["api", "--method", "DELETE", "repos/o/r/issues/comments/1"]"#,
        "gh api --method DELETE repos/o/r/issues/comments/1",
        "gh api --method=DELETE repos/o/r/issues/comments/1",
        "gh api --method delete repos/o/r/issues/comments/1",
        "gh api -X DELETE repos/o/r/labels/x",
        "gh api -XDELETE repos/o/r/labels/x",
        r#"vec!["-X", "DELETE"]"#,
    ] {
        assert!(
            patterns.iter().any(|p| p.is_match(spelling)),
            "the guard must catch {spelling:?}"
        );
    }
    let patterns = &patterns;
    let offenders: Vec<String> = source_files()
        .iter()
        .flat_map(|(file, text)| {
            text.lines()
                .enumerate()
                .filter(|(_, line)| patterns.iter().any(|p| p.is_match(line)))
                .map(move |(n, line)| format!("{}:{}: {}", file.display(), n + 1, line.trim()))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "no file under src/ may spell a REST DELETE: the claim comment and label are never \
         removed (PRD #421 M3.3(b)); found: {offenders:?}"
    );
}
