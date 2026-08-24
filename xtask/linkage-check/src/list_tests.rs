//! PRD #77 Decision 31 — `cargo xtask list-tests`.
//!
//! Emits a Markdown report of every synthetic-test delta between the
//! current branch and `origin/main`. Used by the orchestrator before
//! delegating release (per Decision 31) so the user agrees with the
//! synthetic-test inventory before the merge, and by PR reviewers as
//! a one-command answer to "which tests changed in this branch?".
//!
//! The four sections always print, even when empty (`_(none)_`), so
//! the report's structure is stable for downstream consumers (the
//! orchestrator pastes it verbatim).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

use xtask_docs::{CatalogEntry, DocsConfig, parse_catalog};

/// One synthetic test as observed at a single git ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEntry {
    pub spec_id: String,
    pub fn_name: String,
    /// Path relative to the repo root, e.g. `tests/e2e_hook_delivery.rs`.
    pub file: String,
    /// `/// Scenario:` doc comment with paragraph breaks preserved.
    /// Empty string when the test has no Scenario comment (linkage-check
    /// rule 7 catches this separately).
    pub scenario: String,
    /// Stable fingerprint of the test function body — token stream
    /// serialized to text. Two functions with identical bodies produce
    /// the same fingerprint, so a Same-id-different-fingerprint pair
    /// flags a body modification.
    pub body_fingerprint: String,
}

/// What changed about an existing `#[spec]` test between merge-base
/// and HEAD. At least one of `scenario_changed` or `body_changed` is
/// always true for modified entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifiedRow {
    pub spec_id: String,
    pub fn_name: String,
    pub file: String,
    pub scenario_changed: bool,
    pub body_changed: bool,
}

/// One catalog entry whose prose body changed between refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProseRow {
    pub spec_id: String,
    /// Human-readable summary of which catalog fields changed
    /// (`headline`, `Asserts`, etc.).
    pub what_changed: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowlistChange {
    Added,
    Removed,
}

/// One allowlist line that was added or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowlistRow {
    pub spec_id: String,
    pub change: AllowlistChange,
    /// Inline comment on the allowlist line, if any (the `# foo` tail).
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Public entrypoint
// ---------------------------------------------------------------------------

/// Build the synthetic-test inventory report against `origin/main`.
/// `workspace_root` is the repo root (where the workspace `Cargo.toml`
/// lives). Returns the rendered Markdown.
pub fn run(workspace_root: &Path) -> Result<String, String> {
    let merge_base = git_merge_base(workspace_root)?;

    let base_tests = collect_tests_at_ref(workspace_root, &merge_base)?;
    let head_tests = collect_tests_on_disk(workspace_root)?;

    let base_catalog = parse_catalog_at_ref(workspace_root, &merge_base)?;
    let head_catalog = parse_catalog_on_disk(workspace_root)?;

    let base_allowlist = read_allowlist_at_ref(workspace_root, &merge_base)?;
    let head_allowlist = read_allowlist_on_disk(workspace_root)?;

    let created = compute_created(&base_tests, &head_tests);
    let modified = compute_modified(&base_tests, &head_tests);
    let catalog_delta = compute_catalog_prose_delta(&base_catalog, &head_catalog);
    let allowlist_delta = compute_allowlist_delta(&base_allowlist, &head_allowlist);

    Ok(render_markdown(
        &created,
        &modified,
        &catalog_delta,
        &allowlist_delta,
        &head_catalog,
    ))
}

/// Outcome of `cargo xtask list-tests --compare <ref-a> <ref-b>` (issue
/// #344 item 3): the rendered Markdown plus whether any `#[spec]` test
/// present at `ref_a` is missing at `ref_b`. A non-empty removal set is
/// exactly the shape that let PRD fork#197's tests disappear silently
/// when the commit carrying them was dropped as "superseded" during the
/// 2026-08-15 sync — CI stayed green because the tier just got smaller.
/// This makes that shape visible; it does not judge whether a given
/// removal was justified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareOutcome {
    pub markdown: String,
    pub has_removals: bool,
}

/// Compare the `#[spec]` test population between two arbitrary refs
/// (issue #344 item 3). Unlike [`run`], which always diffs the working
/// tree against `origin/main`'s merge-base for the current branch, this
/// takes two explicit refs and is meant to be run BY HAND across a sync
/// boundary (`docs/develop/fork-sync-workflow.md`'s own procedure) —
/// deliberately NOT wired into the automatic per-PR `linkage-check`
/// suite. On a real `pull_request` CI event `HEAD` is already a merge
/// commit against `origin/main`'s tip, which makes a merge-base
/// comparison structurally vacuous, and a `fork-only`/`sync/*` branch
/// mid-rebase produces false positives because its merge-base briefly
/// points at a stale pre-rebase commit (both failure classes hit by the
/// sibling per-PR checks for issues #259/#281). A manual, explicit-refs
/// comparison sidesteps both.
///
/// "Removed" means a catalog id backed by a `#[spec]` test at `ref_a`
/// that has no backer of the same id at `ref_b` — computed by reusing
/// [`compute_created`] with the two test populations swapped, since "new
/// in head vs base" and "missing from head vs base" are the same set
/// operation run in opposite directions. A same-id test that was merely
/// edited or moved (a genuine rename, a reworded Scenario, a body tweak)
/// is NOT a removal; it shows up in [`compute_modified`] instead, reusing
/// that function's existing body/Scenario fingerprint comparison so a
/// rename/edit is never confused with a drop.
pub fn run_compare(repo_dir: &Path, ref_a: &str, ref_b: &str) -> Result<CompareOutcome, String> {
    let sha_a = resolve_ref(repo_dir, ref_a)?;
    let sha_b = resolve_ref(repo_dir, ref_b)?;

    let tests_a = collect_tests_at_ref(repo_dir, &sha_a)?;
    let tests_b = collect_tests_at_ref(repo_dir, &sha_b)?;

    let added = compute_created(&tests_a, &tests_b);
    let removed = compute_created(&tests_b, &tests_a);
    let modified = compute_modified(&tests_a, &tests_b);

    let has_removals = !removed.is_empty();
    let markdown = render_compare_markdown(ref_a, ref_b, &added, &removed, &modified);
    Ok(CompareOutcome {
        markdown,
        has_removals,
    })
}

/// Resolve `reference` to a commit SHA in `repo_dir`, failing with a
/// named error rather than letting an unresolvable ref silently produce
/// an empty tree further down in [`collect_tests_at_ref`] (`git ls-tree`
/// against a bad ref that happens to still parse as a pathspec would
/// otherwise report "no tests" instead of "no such ref"). Mirrors
/// `work_type::resolve_base`'s never-silent-success error handling
/// without depending on its `WorkTypeError` type, which is specific to
/// work-type derivation rather than this command.
fn resolve_ref(repo_dir: &Path, reference: &str) -> Result<String, String> {
    let out = git_command(repo_dir)
        .args(["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
        .output()
        .map_err(|e| format!("invoke git rev-parse --verify {reference}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ref {reference:?} does not resolve to a commit: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(format!(
            "git rev-parse --verify {reference} returned empty output"
        ));
    }
    Ok(sha)
}

/// Render the `--compare` report. `ref_a`/`ref_b` are printed as given
/// (the caller's original ref strings, not the resolved SHAs) so the
/// report reads naturally when a human passed branch names or tags.
fn render_compare_markdown(
    ref_a: &str,
    ref_b: &str,
    added: &[TestEntry],
    removed: &[TestEntry],
    modified: &[ModifiedRow],
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# Test-population comparison: `{ref_a}` -> `{ref_b}`\n\n"
    ));

    s.push_str("## Removed (present at ref-a, missing at ref-b)\n\n");
    if removed.is_empty() {
        s.push_str("_(none)_\n\n");
    } else {
        s.push_str(&format!(
            "**{} removed.** Every one of these needs an explanation in the \
             sync's own re-curation write-up (issue #344): either upstream's \
             reimplementation genuinely covers the same contract, or this is \
             a silent drop.\n\n",
            removed.len()
        ));
        s.push_str("| Catalog ID | Function | File |\n");
        s.push_str("|---|---|---|\n");
        for t in removed {
            s.push_str(&format!(
                "| {} | `{}` | `{}` |\n",
                t.spec_id, t.fn_name, t.file
            ));
        }
        s.push('\n');
    }

    s.push_str("## Added (present at ref-b, missing at ref-a)\n\n");
    if added.is_empty() {
        s.push_str("_(none)_\n\n");
    } else {
        s.push_str("| Catalog ID | Function | File |\n");
        s.push_str("|---|---|---|\n");
        for t in added {
            s.push_str(&format!(
                "| {} | `{}` | `{}` |\n",
                t.spec_id, t.fn_name, t.file
            ));
        }
        s.push('\n');
    }

    s.push_str(&render_modified_table(
        "## Modified (present at both, changed)\n\n",
        modified,
        false,
    ));

    s
}

/// Shared by `render_compare_markdown` and `render_markdown`: renders the
/// "Modified" table section, identical in both reports apart from the
/// heading text and whether a blank line trails the section (`render_markdown`
/// has further sections after it; `render_compare_markdown`'s Modified
/// section is the last thing printed).
fn render_modified_table(heading: &str, modified: &[ModifiedRow], trailing_blank: bool) -> String {
    let mut s = String::new();
    s.push_str(heading);
    if modified.is_empty() {
        s.push_str("_(none)_\n");
        if trailing_blank {
            s.push('\n');
        }
    } else {
        s.push_str("| Catalog ID | Function | File | What changed |\n");
        s.push_str("|---|---|---|---|\n");
        for m in modified {
            let mut what: Vec<&str> = Vec::new();
            if m.scenario_changed {
                what.push("Scenario");
            }
            if m.body_changed {
                what.push("body");
            }
            s.push_str(&format!(
                "| {} | `{}` | `{}` | {} |\n",
                m.spec_id,
                m.fn_name,
                m.file,
                what.join(", "),
            ));
        }
        if trailing_blank {
            s.push('\n');
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Pure helpers — covered by unit tests in this file
// ---------------------------------------------------------------------------

/// IDs in `head` not in `base`. Sorted by spec_id.
pub fn compute_created(
    base: &BTreeMap<String, TestEntry>,
    head: &BTreeMap<String, TestEntry>,
) -> Vec<TestEntry> {
    let mut out: Vec<TestEntry> = head
        .iter()
        .filter(|(id, _)| !base.contains_key(*id))
        .map(|(_, t)| t.clone())
        .collect();
    out.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
    out
}

/// IDs in BOTH `base` and `head` where either the function body or the
/// Scenario doc comment differs. Sorted by spec_id.
pub fn compute_modified(
    base: &BTreeMap<String, TestEntry>,
    head: &BTreeMap<String, TestEntry>,
) -> Vec<ModifiedRow> {
    let mut out: Vec<ModifiedRow> = Vec::new();
    for (id, head_entry) in head {
        let Some(base_entry) = base.get(id) else {
            continue;
        };
        let scenario_changed = base_entry.scenario != head_entry.scenario;
        let body_changed = base_entry.body_fingerprint != head_entry.body_fingerprint;
        if scenario_changed || body_changed {
            out.push(ModifiedRow {
                spec_id: id.clone(),
                fn_name: head_entry.fn_name.clone(),
                file: head_entry.file.clone(),
                scenario_changed,
                body_changed,
            });
        }
    }
    out.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
    out
}

/// Catalog entries present in both refs whose prose body (any non-id
/// field) changed. Entries added or removed are NOT surfaced here —
/// those are captured by the test-side Created section (catalog adds
/// without a test trip the "catalog ID without test" rule 1 anyway).
pub fn compute_catalog_prose_delta(
    base: &BTreeMap<String, CatalogEntry>,
    head: &BTreeMap<String, CatalogEntry>,
) -> Vec<CatalogProseRow> {
    let mut out: Vec<CatalogProseRow> = Vec::new();
    for (id, head_entry) in head {
        let Some(base_entry) = base.get(id) else {
            continue;
        };
        let mut diffs: Vec<&str> = Vec::new();
        if base_entry.headline != head_entry.headline {
            diffs.push("headline");
        }
        if base_entry.layer != head_entry.layer {
            diffs.push("Layer");
        }
        if base_entry.agent != head_entry.agent {
            diffs.push("Agent");
        }
        if base_entry.asserts != head_entry.asserts {
            diffs.push("Asserts");
        }
        if base_entry.does_not_assert != head_entry.does_not_assert {
            diffs.push("Does not assert");
        }
        if base_entry.platform_coverage != head_entry.platform_coverage {
            diffs.push("Platform coverage");
        }
        if base_entry.cost_note != head_entry.cost_note {
            diffs.push("Cost note");
        }
        if diffs.is_empty() {
            continue;
        }
        out.push(CatalogProseRow {
            spec_id: id.clone(),
            what_changed: diffs.join(", "),
        });
    }
    out.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
    out
}

/// Lines added or removed in `xtask/linkage-check/m2.allowlist`. A
/// catalog ID promoted off the allowlist (because a test landed in
/// the branch) shows up as Removed; a new allowlist entry shows up as
/// Added.
pub fn compute_allowlist_delta(base: &str, head: &str) -> Vec<AllowlistRow> {
    let base_set: BTreeMap<String, Option<String>> = parse_allowlist(base);
    let head_set: BTreeMap<String, Option<String>> = parse_allowlist(head);

    let mut out: Vec<AllowlistRow> = Vec::new();
    for (id, reason) in &head_set {
        if !base_set.contains_key(id) {
            out.push(AllowlistRow {
                spec_id: id.clone(),
                change: AllowlistChange::Added,
                reason: reason.clone(),
            });
        }
    }
    for (id, reason) in &base_set {
        if !head_set.contains_key(id) {
            out.push(AllowlistRow {
                spec_id: id.clone(),
                change: AllowlistChange::Removed,
                reason: reason.clone(),
            });
        }
    }
    out.sort_by(|a, b| match a.spec_id.cmp(&b.spec_id) {
        std::cmp::Ordering::Equal => a.change.cmp(&b.change),
        other => other,
    });
    out
}

/// Parse an allowlist text body into `id -> Option<reason-comment>`.
/// Lines may have an inline `# foo` comment after the id; both halves
/// are preserved. Blank lines and full-line comments are ignored.
fn parse_allowlist(text: &str) -> BTreeMap<String, Option<String>> {
    let mut out: BTreeMap<String, Option<String>> = BTreeMap::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((id_part, comment_part)) = trimmed.split_once('#') {
            let id = id_part.trim().to_string();
            let reason = comment_part.trim().to_string();
            if !id.is_empty() {
                out.insert(
                    id,
                    if reason.is_empty() {
                        None
                    } else {
                        Some(reason)
                    },
                );
            }
        } else {
            out.insert(trimmed.to_string(), None);
        }
    }
    out
}

impl PartialOrd for AllowlistChange {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AllowlistChange {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        fn ordinal(c: &AllowlistChange) -> u8 {
            match c {
                AllowlistChange::Added => 0,
                AllowlistChange::Removed => 1,
            }
        }
        ordinal(self).cmp(&ordinal(other))
    }
}

// ---------------------------------------------------------------------------
// syn-based source parsing (testable via collect_tests_from_sources)
// ---------------------------------------------------------------------------

/// Parse a set of `(file_path, source_text)` pairs into the per-id
/// inventory. The path is preserved verbatim so it can be displayed
/// in the rendered report.
///
/// Fork #156: a catalog id can legitimately be backed by more than one
/// `#[spec]` test across more than one file (e.g. `mouse/form/001`, real
/// five times over across two files) — a supported pattern, not a defect.
/// Collecting every backer per id (instead of overwriting on insert) and
/// then merging them in a traversal-order-independent way is what makes a
/// shared id's Modified/unmodified status depend only on the SET of
/// backing tests, never on the order `sources` happened to be supplied
/// in — which otherwise differs between the git-ref collector
/// (`collect_tests_at_ref`, alphabetical via `git ls-tree`) and the
/// on-disk collector (`collect_tests_on_disk`, unsorted via
/// `std::fs::read_dir`).
pub fn collect_tests_from_sources(
    sources: &[(String, String)],
) -> Result<BTreeMap<String, TestEntry>, String> {
    let mut raw: BTreeMap<String, Vec<TestEntry>> = BTreeMap::new();
    for (path, source) in sources {
        let parsed = match syn::parse_file(source) {
            Ok(p) => p,
            Err(e) => return Err(format!("parse {path}: {e}")),
        };
        // PRD #83: recurse into inline `mod` blocks so `#[spec]` tests
        // that live inside `#[cfg(test)] mod tests { … }` (e.g. the
        // `tabs/selection/*` unit tests in `src/tab.rs`) are found, not
        // just top-level test fns in `tests/`. Mirrors the docs
        // generator's `collect_spec_tests_from_items` walk.
        collect_entries_from_items(&parsed.items, path, &mut raw);
    }
    Ok(raw
        .into_iter()
        .map(|(id, entries)| (id, merge_entries(entries)))
        .collect())
}

/// Recurse through `items`, recording every `#[spec]`-annotated fn into
/// `out`. Items inside inline `Item::Mod { content: Some(_) }` are
/// walked the same way; external `mod foo;` declarations (no inline
/// body) are skipped — resolving them would need a separate file read.
///
/// Every backer for a given id is pushed, not just the last one seen —
/// see `collect_tests_from_sources` for why.
fn collect_entries_from_items(
    items: &[syn::Item],
    path: &str,
    out: &mut BTreeMap<String, Vec<TestEntry>>,
) {
    for item in items {
        match item {
            syn::Item::Fn(item_fn) => {
                if let Some(spec_id) = read_spec_attr(&item_fn.attrs) {
                    let fn_name = item_fn.sig.ident.to_string();
                    let scenario = read_scenario_doc(&item_fn.attrs).unwrap_or_default();
                    let body_fingerprint = fingerprint_block(&item_fn.block);
                    out.entry(spec_id.clone()).or_default().push(TestEntry {
                        spec_id,
                        fn_name,
                        file: path.to_string(),
                        scenario,
                        body_fingerprint,
                    });
                }
            }
            syn::Item::Mod(item_mod) => {
                if let Some((_, nested_items)) = &item_mod.content {
                    collect_entries_from_items(nested_items, path, out);
                }
            }
            _ => {}
        }
    }
}

/// Combine every `#[spec]` test backing one catalog id into a single
/// canonical `TestEntry` (fork #156). The single-backer case (the
/// overwhelming majority of ids) returns that one entry untouched, so
/// existing single-id inventory rows and their Scenario/body-fingerprint
/// values are unaffected.
///
/// For a shared id, backers are sorted by `(file, fn_name)` BEFORE
/// combining — a fixed, content-derived order, not the order `sources`
/// happened to list them in — so two collectors that visit the same
/// unchanged files in different orders (`git ls-tree` vs
/// `std::fs::read_dir`) produce byte-identical merged entries and no
/// phantom "Modified" row (the false-positive direction). The merged
/// `scenario` and `body_fingerprint` fold in EVERY backer, not just one
/// winner, so a real edit to any backer — even one that would have lost
/// a last-insert-wins race — still changes the merged fingerprint (the
/// false-negative direction: a tool that reports `_(none)_` while a real
/// change hides behind a shared id is exactly as untrustworthy as one
/// that invents rows).
fn merge_entries(mut entries: Vec<TestEntry>) -> TestEntry {
    if entries.len() == 1 {
        return entries.remove(0);
    }
    entries.sort_by(|a, b| (&a.file, &a.fn_name).cmp(&(&b.file, &b.fn_name)));
    let spec_id = entries[0].spec_id.clone();
    let fn_name = entries[0].fn_name.clone();
    let file = entries[0].file.clone();
    let scenario = entries
        .iter()
        .map(|e| format!("{}: {}", e.file, e.scenario))
        .collect::<Vec<_>>()
        .join(" | ");
    let body_fingerprint = entries
        .iter()
        .map(|e| format!("{}\u{0}{}", e.file, e.body_fingerprint))
        .collect::<Vec<_>>()
        .join("\u{1}");
    TestEntry {
        spec_id,
        fn_name,
        file,
        scenario,
        body_fingerprint,
    }
}

fn read_spec_attr(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("spec") {
            continue;
        }
        let parsed: Result<syn::LitStr, _> = attr.parse_args();
        if let Ok(lit) = parsed {
            return Some(lit.value());
        }
    }
    None
}

fn read_scenario_doc(attrs: &[syn::Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| {
            let mv: syn::MetaNameValue = a.meta.require_name_value().ok().cloned()?;
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = mv.value
            {
                Some(s.value())
            } else {
                None
            }
        })
        .collect();
    let scenario_marker = Regex::new(r"(?i)^\s*scenario(?:\s*:|\s+|\s*$)").expect("scenario regex");
    let start = lines.iter().position(|l| scenario_marker.is_match(l))?;
    let first_line = scenario_marker
        .replace(&lines[start], "")
        .trim()
        .to_string();
    let mut current: Vec<String> = Vec::new();
    if !first_line.is_empty() {
        current.push(first_line);
    }
    for line in lines.iter().skip(start + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if scenario_marker.is_match(line) {
            break;
        }
        current.push(trimmed.to_string());
    }
    if current.is_empty() {
        None
    } else {
        Some(current.join(" "))
    }
}

fn fingerprint_block(block: &syn::Block) -> String {
    use quote_compat::quote_to_string;
    quote_to_string(block)
}

/// Minimal `quote!`-free token-to-string helper for fingerprinting.
/// `syn`'s `Block` doesn't expose its source bytes directly; we walk
/// the underlying token stream and serialize via `Display`. Two
/// functionally-identical bodies stringify identically; whitespace
/// only differences vanish (token tree is whitespace-insensitive).
mod quote_compat {
    use proc_macro2::TokenStream;
    use syn::__private::ToTokens;

    pub fn quote_to_string<T: ToTokens>(value: &T) -> String {
        let mut ts = TokenStream::new();
        value.to_tokens(&mut ts);
        ts.to_string()
    }
}

/// First sentence of a scenario doc comment, for the Created-row
/// `Scenario` column. Falls back to the whole scenario if there's no
/// `.` boundary.
pub fn first_sentence(scenario: &str) -> String {
    if scenario.is_empty() {
        return "(missing /// Scenario:)".to_string();
    }
    match scenario.find(". ") {
        Some(idx) => scenario[..idx + 1].to_string(),
        None => scenario.to_string(),
    }
}

/// Derive a one-word layer label from the spec id + catalog entry.
///
/// The catalog `Layer:` field is free prose after the token, e.g.
/// `L2 (re-sequenced from L1: ...)`. We must read the layer TOKEN that
/// immediately follows the marker (the first whitespace-delimited word),
/// NOT match `L1`/`L2` anywhere in the line — otherwise the `L1` inside a
/// parenthetical explanation would wrongly win over the real `L2` token.
pub fn layer_label(spec_id: &str, catalog_entry: Option<&CatalogEntry>) -> String {
    if let Some(entry) = catalog_entry
        && let Some(layer) = entry.layer.as_deref()
        && layer_token_is_l1(layer)
    {
        return "L1".to_string();
    }
    if spec_id.starts_with("chain-smoke/") {
        return "chain-smoke".to_string();
    }
    "L2 synthetic".to_string()
}

/// Whether the FIRST token of a catalog `Layer:` value is `L1` (case- and
/// surrounding-punctuation-insensitive), e.g. `L1`, `L1 (ratatui).`, `L1.`.
/// `L2 (re-sequenced from L1: ...)` → `false` (the leading token is `L2`).
fn layer_token_is_l1(layer: &str) -> bool {
    layer
        .split_whitespace()
        .next()
        .map(|tok| tok.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .is_some_and(|tok| tok.eq_ignore_ascii_case("l1"))
}

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

pub fn render_markdown(
    created: &[TestEntry],
    modified: &[ModifiedRow],
    catalog_delta: &[CatalogProseRow],
    allowlist_delta: &[AllowlistRow],
    head_catalog: &BTreeMap<String, CatalogEntry>,
) -> String {
    let mut s = String::new();
    s.push_str("# Synthetic-test inventory\n\n");

    s.push_str("## Created in this branch\n\n");
    if created.is_empty() {
        s.push_str("_(none)_\n\n");
    } else {
        s.push_str("| Catalog ID | Layer | Function | File | Scenario |\n");
        s.push_str("|---|---|---|---|---|\n");
        for t in created {
            let layer = layer_label(&t.spec_id, head_catalog.get(&t.spec_id));
            s.push_str(&format!(
                "| {} | {} | `{}` | `{}` | {} |\n",
                t.spec_id,
                layer,
                t.fn_name,
                t.file,
                escape_table_cell(&first_sentence(&t.scenario)),
            ));
        }
        s.push('\n');
    }

    s.push_str(&render_modified_table(
        "## Modified in this branch\n\n",
        modified,
        true,
    ));

    s.push_str("## Catalog entries with prose changes\n\n");
    if catalog_delta.is_empty() {
        s.push_str("_(none)_\n\n");
    } else {
        s.push_str("| Catalog ID | What changed |\n");
        s.push_str("|---|---|\n");
        for c in catalog_delta {
            s.push_str(&format!(
                "| {} | {} |\n",
                c.spec_id,
                escape_table_cell(&c.what_changed),
            ));
        }
        s.push('\n');
    }

    s.push_str("## Linkage-allowlist deltas\n\n");
    if allowlist_delta.is_empty() {
        s.push_str("_(none)_\n");
    } else {
        s.push_str("| Catalog ID | Change | Reason |\n");
        s.push_str("|---|---|---|\n");
        for a in allowlist_delta {
            let change = match a.change {
                AllowlistChange::Added => "added",
                AllowlistChange::Removed => "removed",
            };
            let reason = a.reason.as_deref().unwrap_or("");
            s.push_str(&format!(
                "| {} | {} | {} |\n",
                a.spec_id,
                change,
                escape_table_cell(reason),
            ));
        }
    }

    s
}

fn escape_table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

// ---------------------------------------------------------------------------
// I/O — git + filesystem
// ---------------------------------------------------------------------------

/// Git location environment variables that must never leak into a `git`
/// invocation in this module (issue #344 auditor finding A2). An ambient
/// `GIT_DIR` pointed at some other repository makes `git ls-tree` return
/// empty output at exit 0 rather than erroring — read as "no tests" by
/// every caller here instead of "wrong repository" — so `--compare`
/// silently reports a confident all-clear while having read the wrong
/// tree. Clearing all of them, not just `GIT_DIR`, closes the same door
/// for its documented siblings (`git(1)` ENVIRONMENT VARIABLES).
const GIT_ENV_VARS_TO_CLEAR: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// Build a `git` [`Command`] rooted at `repo_dir` with every ambient git
/// location variable cleared, so ambient environment can never redirect
/// it to a different repository than the one named by `repo_dir`.
fn git_command(repo_dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_dir);
    for var in GIT_ENV_VARS_TO_CLEAR {
        cmd.env_remove(var);
    }
    cmd
}

fn git_merge_base(repo_dir: &Path) -> Result<String, String> {
    let out = git_command(repo_dir)
        .args(["merge-base", "HEAD", "origin/main"])
        .output()
        .map_err(|e| format!("invoke git merge-base: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git merge-base HEAD origin/main failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err("git merge-base returned empty output".to_string());
    }
    Ok(sha)
}

pub(crate) fn git_show(repo_dir: &Path, reference: &str, path: &str) -> Result<String, String> {
    let out = git_command(repo_dir)
        .args(["show", &format!("{reference}:{path}")])
        .output()
        .map_err(|e| format!("invoke git show {reference}:{path}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git show {reference}:{path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_ls_tree(repo_dir: &Path, reference: &str, path: &str) -> Result<Vec<String>, String> {
    let out = git_command(repo_dir)
        .args(["ls-tree", "-r", "--name-only", reference, path])
        .output()
        .map_err(|e| format!("invoke git ls-tree {reference} {path}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-tree {reference} {path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect())
}

pub(crate) fn collect_tests_at_ref(
    repo_dir: &Path,
    reference: &str,
) -> Result<BTreeMap<String, TestEntry>, String> {
    let mut sources: Vec<(String, String)> = Vec::new();
    for f in git_ls_tree(repo_dir, reference, "tests")? {
        if !f.ends_with(".rs") {
            continue;
        }
        // Skip the harness module itself — it carries no #[spec] but
        // is huge and slows the parse for no reason.
        if f == "tests/common/mod.rs" {
            continue;
        }
        let body = git_show(repo_dir, reference, &f)?;
        sources.push((f, body));
    }
    // src/ — PRD #83: the library crate can hold `#[spec]` tests too
    // (e.g. `src/tab.rs`). Only keep files that carry a `#[spec(` so a
    // ref's whole `src/` tree isn't syn-parsed, matching the on-disk and
    // docs-generator approach. (Scanning `src/` at BOTH refs keeps a
    // src-resident test that existed at merge-base out of the Created
    // section and into Modified, as expected.)
    for f in git_ls_tree(repo_dir, reference, "src")? {
        if !f.ends_with(".rs") {
            continue;
        }
        let body = git_show(repo_dir, reference, &f)?;
        if !body.contains("#[spec(") {
            continue;
        }
        sources.push((f, body));
    }
    collect_tests_from_sources(&sources)
}

fn collect_tests_on_disk(root: &Path) -> Result<BTreeMap<String, TestEntry>, String> {
    let mut sources: Vec<(String, String)> = Vec::new();
    // tests/ — parse every .rs (minus the huge spec-less harness module).
    let tests_dir = root.join("tests");
    walk_rs_files(&tests_dir, &mut |abs_path| {
        if abs_path.ends_with("common/mod.rs") {
            return Ok(());
        }
        let rel = rel_to_root(root, abs_path)?;
        let body = std::fs::read_to_string(abs_path)
            .map_err(|e| format!("read {}: {e}", abs_path.display()))?;
        sources.push((rel, body));
        Ok(())
    })?;
    // src/ — PRD #83: `#[spec]` tests also live in the library crate
    // (e.g. `src/tab.rs`). Pre-filter to files that actually carry a
    // `#[spec(` so we don't syn-parse the whole crate every run, matching
    // the docs generator's approach.
    let src_dir = root.join("src");
    walk_rs_files(&src_dir, &mut |abs_path| {
        let body = std::fs::read_to_string(abs_path)
            .map_err(|e| format!("read {}: {e}", abs_path.display()))?;
        if !body.contains("#[spec(") {
            return Ok(());
        }
        let rel = rel_to_root(root, abs_path)?;
        sources.push((rel, body));
        Ok(())
    })?;
    collect_tests_from_sources(&sources)
}

fn rel_to_root(root: &Path, abs_path: &Path) -> Result<String, String> {
    Ok(abs_path
        .strip_prefix(root)
        .map_err(|e| format!("strip prefix {}: {e}", abs_path.display()))?
        .to_string_lossy()
        .into_owned())
}

fn walk_rs_files(
    dir: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("read_dir {}: {e}", dir.display())),
    };
    for entry in rd {
        let entry = entry.map_err(|e| format!("read_dir entry: {e}"))?;
        let p = entry.path();
        let meta = std::fs::symlink_metadata(&p)
            .map_err(|e| format!("symlink_metadata {}: {e}", p.display()))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            walk_rs_files(&p, visit)?;
        } else if ft.is_file() && p.extension().and_then(|e| e.to_str()) == Some("rs") {
            visit(&p)?;
        }
    }
    Ok(())
}

fn parse_catalog_at_ref(
    workspace_root: &Path,
    reference: &str,
) -> Result<BTreeMap<String, CatalogEntry>, String> {
    // The catalog lives at `tests/CATALOG.md` (relocated out of the archived
    // PRD #77 file). On a base ref that predates the move the file is absent;
    // treat that like an empty catalog (mirrors `read_allowlist_at_ref`) so
    // the diff just shows the entries as added rather than failing.
    let catalog_rel = "tests/CATALOG.md";
    let body = match git_show(workspace_root, reference, catalog_rel) {
        Ok(s) => s,
        Err(_) => return Ok(BTreeMap::new()),
    };
    // parse_catalog wants a file path; stage the body in a tempfile.
    let tmp = tempfile_for_catalog(workspace_root, &body)?;
    let result = parse_catalog(&tmp);
    let _ = std::fs::remove_file(&tmp);
    result
}

fn parse_catalog_on_disk(workspace_root: &Path) -> Result<BTreeMap<String, CatalogEntry>, String> {
    let config = DocsConfig::from_workspace(workspace_root);
    parse_catalog(&config.catalog_path)
}

fn tempfile_for_catalog(workspace_root: &Path, body: &str) -> Result<PathBuf, String> {
    let dir = workspace_root.join("target");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!(
        "xtask-list-tests-catalog-{}-{nanos}.md",
        std::process::id()
    ));
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

fn read_allowlist_at_ref(repo_dir: &Path, reference: &str) -> Result<String, String> {
    let path = "xtask/linkage-check/m2.allowlist";
    // A branch where the allowlist was removed entirely would error
    // here, but the path is load-bearing so we treat the failure as
    // an empty allowlist rather than a fatal.
    match git_show(repo_dir, reference, path) {
        Ok(s) => Ok(s),
        Err(_) => Ok(String::new()),
    }
}

fn read_allowlist_on_disk(workspace_root: &Path) -> Result<String, String> {
    let path = workspace_root
        .join("xtask")
        .join("linkage-check")
        .join("m2.allowlist");
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(spec_id: &str, fn_name: &str, scenario: &str, body: &str) -> TestEntry {
        TestEntry {
            spec_id: spec_id.to_string(),
            fn_name: fn_name.to_string(),
            file: format!("tests/e2e_{}.rs", spec_id.replace('/', "_")),
            scenario: scenario.to_string(),
            body_fingerprint: body.to_string(),
        }
    }

    fn cat_entry(id: &str, headline: &str, asserts: &str) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            headline: headline.to_string(),
            layer: Some("L2.".to_string()),
            agent: None,
            asserts: Some(asserts.to_string()),
            does_not_assert: None,
            platform_coverage: None,
            cost_note: None,
        }
    }

    #[test]
    fn created_section_lists_only_ids_new_in_head() {
        let mut base: BTreeMap<String, TestEntry> = BTreeMap::new();
        base.insert(
            "hooks/delivery/001".to_string(),
            entry("hooks/delivery/001", "delivery_001_x", "x", "body-a"),
        );
        let mut head: BTreeMap<String, TestEntry> = base.clone();
        head.insert(
            "dashboard/pane/005".to_string(),
            entry(
                "dashboard/pane/005",
                "pane_005_y",
                "Render a card",
                "body-b",
            ),
        );
        head.insert(
            "chain-smoke/claude/002".to_string(),
            entry(
                "chain-smoke/claude/002",
                "claude_002_z",
                "Drive Claude end to end",
                "body-c",
            ),
        );

        let created = compute_created(&base, &head);
        // hooks/delivery/001 is in both → not Created.
        // Sorted by spec_id: chain-smoke/claude/002 comes before
        // dashboard/pane/005.
        assert_eq!(created.len(), 2);
        assert_eq!(created[0].spec_id, "chain-smoke/claude/002");
        assert_eq!(created[1].spec_id, "dashboard/pane/005");
    }

    #[test]
    fn modified_section_lists_ids_with_changed_body_or_scenario() {
        let mut base: BTreeMap<String, TestEntry> = BTreeMap::new();
        base.insert(
            "hooks/delivery/001".to_string(),
            entry(
                "hooks/delivery/001",
                "delivery_001_x",
                "Original.",
                "body-a",
            ),
        );
        base.insert(
            "hooks/delivery/002".to_string(),
            entry("hooks/delivery/002", "delivery_002_x", "Same.", "body-x"),
        );
        base.insert(
            "hooks/delivery/003".to_string(),
            entry("hooks/delivery/003", "delivery_003_x", "Same.", "body-y"),
        );

        let mut head: BTreeMap<String, TestEntry> = base.clone();
        // Scenario change only.
        head.get_mut("hooks/delivery/001").unwrap().scenario = "Updated narrative.".to_string();
        // Body change only.
        head.get_mut("hooks/delivery/002").unwrap().body_fingerprint = "body-x-changed".to_string();
        // delivery/003 unchanged.

        let modified = compute_modified(&base, &head);
        assert_eq!(modified.len(), 2);
        assert_eq!(modified[0].spec_id, "hooks/delivery/001");
        assert!(modified[0].scenario_changed);
        assert!(!modified[0].body_changed);
        assert_eq!(modified[1].spec_id, "hooks/delivery/002");
        assert!(!modified[1].scenario_changed);
        assert!(modified[1].body_changed);
    }

    /// Fork #156 — a catalog id legitimately shared by tests in more than
    /// one file (e.g. `mouse/form/001`, which real annotations five times
    /// across `tests/e2e_mouse_form.rs` and `tests/render_form_buttons.rs`)
    /// is silently collapsed to ONE `TestEntry` per id by
    /// `collect_tests_from_sources`'s `BTreeMap::insert`, so whichever file
    /// is processed LAST for that id wins and every earlier one is
    /// discarded without a trace. The git-ref collector orders files via
    /// `git ls-tree` (alphabetical); the on-disk collector orders them via
    /// `std::fs::read_dir` (filesystem-dependent, not sorted) — nothing
    /// guarantees the two agree. When they disagree, base and head keep
    /// DIFFERENT winners for the same shared id even though every file
    /// backing it is byte-identical between the two refs, and the id is
    /// reported "Modified" — this is the mechanism behind the false
    /// positives observed at PR #153's merge gate.
    #[test]
    fn compute_modified_does_not_flag_a_shared_spec_id_whose_every_file_is_unchanged() {
        let file_a = (
            "tests/e2e_a.rs".to_string(),
            r#"
                #[spec("shared/id/001")]
                #[test]
                /// Scenario: exercised from file A.
                fn shared_001_from_a() { let x = 1; }
            "#
            .to_string(),
        );
        let file_b = (
            "tests/e2e_b.rs".to_string(),
            r#"
                #[spec("shared/id/001")]
                #[test]
                /// Scenario: exercised from file B.
                fn shared_001_from_b() { let x = 2; }
            "#
            .to_string(),
        );

        // Neither file's bytes differ between "base" and "head" — only the
        // traversal order does, exactly as it legitimately can between
        // `git ls-tree` order and `std::fs::read_dir` order.
        let base = collect_tests_from_sources(&[file_a.clone(), file_b.clone()]).expect("parses");
        let head = collect_tests_from_sources(&[file_b, file_a]).expect("parses");

        let modified = compute_modified(&base, &head);
        assert!(
            modified.is_empty(),
            "shared/id/001 reported modified even though every file backing it \
             is byte-identical between base and head — only collector traversal \
             order differed: {modified:?}"
        );
    }

    /// Fork #156's symmetric direction, per the issue's own note: "a tool
    /// that invents rows is equally untrustworthy when it reports
    /// `_(none)_`". If the SAME file happens to win the shared-id collision
    /// on both sides (traversal order agrees, unlike the test above), a
    /// real edit to the OTHER file sharing that id is silently swallowed —
    /// the report says nothing changed when something did.
    #[test]
    fn compute_modified_can_miss_a_genuine_change_hidden_behind_a_shared_spec_id() {
        let file_a_before = (
            "tests/e2e_a.rs".to_string(),
            r#"
                #[spec("shared/id/002")]
                #[test]
                /// Scenario: exercised from file A, original.
                fn shared_002_from_a() { let x = 1; }
            "#
            .to_string(),
        );
        let file_a_after = (
            "tests/e2e_a.rs".to_string(),
            r#"
                #[spec("shared/id/002")]
                #[test]
                /// Scenario: exercised from file A, CHANGED.
                fn shared_002_from_a() { let x = 999; }
            "#
            .to_string(),
        );
        let file_b = (
            "tests/e2e_b.rs".to_string(),
            r#"
                #[spec("shared/id/002")]
                #[test]
                /// Scenario: exercised from file B, never touched.
                fn shared_002_from_b() { let x = 2; }
            "#
            .to_string(),
        );

        // Both base and head process A then B, so B wins the collision both
        // times — A's real edit never surfaces in either map.
        let base = collect_tests_from_sources(&[file_a_before, file_b.clone()]).expect("parses");
        let head = collect_tests_from_sources(&[file_a_after, file_b]).expect("parses");

        let modified = compute_modified(&base, &head);
        assert!(
            modified.iter().any(|m| m.spec_id == "shared/id/002"),
            "file A's body genuinely changed under shared/id/002, but the \
             shared-id collision hid it because file B won the slot on both \
             sides, so the report would say `_(none)_`: {modified:?}"
        );
    }

    #[test]
    fn catalog_prose_delta_flags_changed_fields() {
        let mut base: BTreeMap<String, CatalogEntry> = BTreeMap::new();
        base.insert(
            "hooks/delivery/001".to_string(),
            cat_entry(
                "hooks/delivery/001",
                "Old headline",
                "Asserts the old behavior",
            ),
        );
        base.insert(
            "dashboard/pane/004".to_string(),
            cat_entry("dashboard/pane/004", "Card title row", "Renders cleanly"),
        );

        let mut head: BTreeMap<String, CatalogEntry> = base.clone();
        // Change headline AND asserts on the first entry.
        head.get_mut("hooks/delivery/001").unwrap().headline = "New headline".to_string();
        head.get_mut("hooks/delivery/001").unwrap().asserts =
            Some("Asserts the new behavior".to_string());
        // dashboard/pane/004 unchanged.

        let delta = compute_catalog_prose_delta(&base, &head);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].spec_id, "hooks/delivery/001");
        assert!(delta[0].what_changed.contains("headline"));
        assert!(delta[0].what_changed.contains("Asserts"));
    }

    #[test]
    fn allowlist_delta_lists_additions_and_removals() {
        let base = "\
            # comment line
            hooks/delivery/001
            hooks/delivery/002  # parked for M3
            dashboard/pane/005
        ";
        let head = "\
            # comment line
            hooks/delivery/002  # parked for M3
            dashboard/pane/006  # new entry M4
            chain-smoke/opencode/001  # blocked on deck plugin
        ";

        let delta = compute_allowlist_delta(base, head);
        let added: Vec<&AllowlistRow> = delta
            .iter()
            .filter(|r| matches!(r.change, AllowlistChange::Added))
            .collect();
        let removed: Vec<&AllowlistRow> = delta
            .iter()
            .filter(|r| matches!(r.change, AllowlistChange::Removed))
            .collect();
        // Added: chain-smoke/opencode/001, dashboard/pane/006 (sorted).
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].spec_id, "chain-smoke/opencode/001");
        assert_eq!(added[0].reason.as_deref(), Some("blocked on deck plugin"));
        assert_eq!(added[1].spec_id, "dashboard/pane/006");
        // Removed: dashboard/pane/005, hooks/delivery/001 (sorted).
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].spec_id, "dashboard/pane/005");
        assert_eq!(removed[1].spec_id, "hooks/delivery/001");
    }

    #[test]
    fn render_markdown_emits_none_for_empty_sections() {
        let empty_catalog: BTreeMap<String, CatalogEntry> = BTreeMap::new();
        let report = render_markdown(&[], &[], &[], &[], &empty_catalog);
        assert!(report.starts_with("# Synthetic-test inventory"));
        assert!(report.contains("## Created in this branch\n\n_(none)_"));
        assert!(report.contains("## Modified in this branch\n\n_(none)_"));
        assert!(report.contains("## Catalog entries with prose changes\n\n_(none)_"));
        assert!(report.contains("## Linkage-allowlist deltas\n\n_(none)_"));
    }

    #[test]
    fn render_markdown_populates_created_table_with_layer_and_scenario() {
        let mut head_catalog: BTreeMap<String, CatalogEntry> = BTreeMap::new();
        head_catalog.insert(
            "dashboard/pane/004".to_string(),
            CatalogEntry {
                id: "dashboard/pane/004".into(),
                headline: "Card title row".into(),
                layer: Some("L1 (ratatui TestBackend + insta).".into()),
                agent: None,
                asserts: None,
                does_not_assert: None,
                platform_coverage: None,
                cost_note: None,
            },
        );
        let created = vec![TestEntry {
            spec_id: "dashboard/pane/004".into(),
            fn_name: "pane_004_card_title_row".into(),
            file: "tests/render_dashboard.rs".into(),
            scenario: "Render a single dashboard card. Pin it.".into(),
            body_fingerprint: "fp".into(),
        }];
        let report = render_markdown(&created, &[], &[], &[], &head_catalog);
        assert!(report.contains("| dashboard/pane/004 | L1 |"));
        assert!(report.contains("`pane_004_card_title_row`"));
        assert!(report.contains("Render a single dashboard card."));
    }

    #[test]
    fn collect_tests_from_sources_extracts_spec_id_and_scenario() {
        let src = r#"
            #[spec("hooks/delivery/001")]
            #[test]
            /// Scenario: A short, in-process check.
            fn delivery_001_x() {
                let x = 1 + 1;
                assert_eq!(x, 2);
            }
        "#;
        let sources = vec![("tests/e2e_x.rs".to_string(), src.to_string())];
        let map = collect_tests_from_sources(&sources).expect("parses");
        assert_eq!(map.len(), 1);
        let t = &map["hooks/delivery/001"];
        assert_eq!(t.fn_name, "delivery_001_x");
        assert_eq!(t.scenario, "A short, in-process check.");
        assert!(!t.body_fingerprint.is_empty());
    }

    #[test]
    fn collect_tests_from_sources_recurses_into_inline_modules() {
        // PRD #83: src-resident `#[spec]` tests live inside
        // `#[cfg(test)] mod tests { … }`. The inventory walker must
        // recurse into inline modules, not just scan top-level items —
        // otherwise `cargo xtask list-tests` silently omits them.
        let src = r#"
            #[spec("dashboard/pane/004")]
            #[test]
            /// Scenario: top-level test.
            fn pane_004_top() { let _ = 1; }

            #[cfg(test)]
            mod tests {
                #[spec("tabs/selection/001")]
                #[test]
                /// Scenario: nested in a test module.
                fn selection_001_nested() { let _ = 2; }

                mod deeper {
                    #[spec("tabs/selection/002")]
                    #[test]
                    /// Scenario: doubly nested.
                    fn selection_002_deep() { let _ = 3; }
                }
            }
        "#;
        let sources = vec![("src/tab.rs".to_string(), src.to_string())];
        let map = collect_tests_from_sources(&sources).expect("parses");
        assert_eq!(map.len(), 3);
        assert_eq!(map["dashboard/pane/004"].fn_name, "pane_004_top");
        assert_eq!(map["tabs/selection/001"].fn_name, "selection_001_nested");
        assert_eq!(map["tabs/selection/001"].file, "src/tab.rs");
        assert_eq!(map["tabs/selection/002"].fn_name, "selection_002_deep");
    }

    #[test]
    fn layer_label_picks_l1_chain_smoke_or_l2_synthetic() {
        let mut catalog: BTreeMap<String, CatalogEntry> = BTreeMap::new();
        catalog.insert(
            "dashboard/pane/004".into(),
            CatalogEntry {
                id: "dashboard/pane/004".into(),
                headline: "x".into(),
                layer: Some("L1 (ratatui).".into()),
                agent: None,
                asserts: None,
                does_not_assert: None,
                platform_coverage: None,
                cost_note: None,
            },
        );
        assert_eq!(
            layer_label("dashboard/pane/004", catalog.get("dashboard/pane/004")),
            "L1"
        );
        assert_eq!(layer_label("chain-smoke/claude/001", None), "chain-smoke");
        assert_eq!(layer_label("hooks/delivery/001", None), "L2 synthetic");
    }

    #[test]
    fn layer_label_reads_first_token_not_parenthetical_l1() {
        // The leading token is `L2`; the `L1` inside the parenthetical prose
        // must NOT flip the label (regression for the substring-match bug).
        let entry = CatalogEntry {
            id: "prompt/new-pane/007".into(),
            headline: "x".into(),
            layer: Some(
                "L2 (re-sequenced from L1: no public L1 render seam, driven via PTY).".into(),
            ),
            agent: None,
            asserts: None,
            does_not_assert: None,
            platform_coverage: None,
            cost_note: None,
        };
        let mut catalog: BTreeMap<String, CatalogEntry> = BTreeMap::new();
        catalog.insert("prompt/new-pane/007".into(), entry);
        assert_eq!(
            layer_label("prompt/new-pane/007", catalog.get("prompt/new-pane/007")),
            "L2 synthetic"
        );

        // First-token detection is robust to trailing punctuation / parens.
        assert!(layer_token_is_l1("L1"));
        assert!(layer_token_is_l1("L1 (ratatui TestBackend + insta)."));
        assert!(layer_token_is_l1("L1."));
        assert!(!layer_token_is_l1("L2 (re-sequenced from L1: ...)"));
        assert!(!layer_token_is_l1("L2 synthetic"));
        assert!(!layer_token_is_l1(""));
    }

    /// Issue #344 R1 (reviewer recheck of A2): pins `git_command`'s env
    /// isolation directly, with no spawn — `Command::get_envs()` reports
    /// each `env_remove`'d variable as `(key, None)`, so this asserts all
    /// eight `GIT_ENV_VARS_TO_CLEAR` entries are explicitly removed on the
    /// `Command` `git_command` builds, rather than only exercising it
    /// indirectly via `real_git`'s decoy-env integration tests below.
    #[test]
    fn git_command_clears_all_git_location_env_vars() {
        let cmd = git_command(Path::new("/tmp/git-command-env-test-repo"));
        for var in GIT_ENV_VARS_TO_CLEAR {
            let removed = cmd
                .get_envs()
                .any(|(k, v)| k == std::ffi::OsStr::new(var) && v.is_none());
            assert!(
                removed,
                "expected {var} to be explicitly removed, got envs: {:?}",
                cmd.get_envs().collect::<Vec<_>>()
            );
        }
    }
}

/// Issue #344 item 3: `run_compare` shells out to real `git` against two
/// refs, so it needs a real repository history to exercise — the
/// `mod real_git` exception CLAUDE.md rule 5 carves out for exactly this
/// shape (fixtures under a `tempfile::tempdir()`, ambient git
/// configuration switched off, no network/sleep, nothing that can read or
/// write the checkout these tests run inside).
///
/// **Not gated to `unix`, unlike `repo_state.rs`'s `mod real_git`.** That
/// module's gate exists for two Unix-specific fixture constructs (a
/// `file://` URL spelled from a POSIX path, and a directory name containing
/// a literal newline Win32 rejects) that this module's fixtures do not use
/// — `git init`, `fs::write`/`fs::remove_file`, and `Path`/`PathBuf` joins
/// with forward slashes, all of which Windows accepts fine. Keeping these
/// four tests running on `build-windows` is what CLAUDE.md rule 5 added
/// `--workspace` for in the first place.
#[cfg(test)]
mod real_git {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A throwaway repository under a `tempfile::tempdir()`, isolated from
    /// the developer's/CI runner's ambient git configuration the same way
    /// `repo_state.rs`'s `mod real_git::Sandbox` is — see that module's
    /// doc comment for the full reasoning; duplicated here rather than
    /// shared because the two modules' fixture shapes (worktrees/clones
    /// there, a single linear history of test-file commits here) don't
    /// overlap enough to be worth a shared abstraction.
    struct Sandbox {
        _dir: TempDir,
        root: PathBuf,
    }

    impl Sandbox {
        fn new() -> Sandbox {
            let dir = TempDir::new().expect("tempdir");
            let root = dir.path().canonicalize().expect("canonicalize tempdir");
            fs::create_dir_all(root.join("home")).expect("mkdir home");
            fs::create_dir_all(root.join("empty-template")).expect("mkdir template");
            let sandbox = Sandbox { _dir: dir, root };
            sandbox.git(&["init", "-q", "-b", "main"]);
            sandbox
        }

        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .args(args)
                .current_dir(&self.root)
                .env("HOME", self.root.join("home"))
                .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
                .env("GIT_CONFIG_GLOBAL", self.root.join("no-such-gitconfig"))
                .env("GIT_CONFIG_SYSTEM", self.root.join("no-such-gitconfig"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TEMPLATE_DIR", self.root.join("empty-template"))
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_AUTHOR_NAME", "list-tests tests")
                .env("GIT_AUTHOR_EMAIL", "tests@example.invalid")
                .env("GIT_COMMITTER_NAME", "list-tests tests")
                .env("GIT_COMMITTER_EMAIL", "tests@example.invalid")
                .output()
                .unwrap_or_else(|e| panic!("failed to invoke `git {}`: {e}", args.join(" ")));
            assert!(
                out.status.success(),
                "fixture command `git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim(),
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        /// Writes (or overwrites) a file relative to the sandbox root,
        /// creating parent directories as needed.
        fn write(&self, rel: &str, contents: &str) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("mkdir parent");
            }
            fs::write(&path, contents).expect("write fixture file");
        }

        /// Stages everything and commits, returning the new commit SHA.
        fn commit(&self, message: &str) -> String {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", message]);
            self.git(&["rev-parse", "HEAD"])
        }
    }

    /// Item 3's first required case: `ref-b` only ADDS a test relative to
    /// `ref-a` — no removal should be reported.
    #[test]
    fn compare_reports_no_removal_when_ref_b_only_adds_a_test() {
        let sandbox = Sandbox::new();
        sandbox.write(
            "tests/e2e_a.rs",
            r#"
                #[spec("hooks/delivery/001")]
                #[test]
                /// Scenario: original test, present at both refs.
                fn delivery_001_x() { let _ = 1; }
            "#,
        );
        let sha_a = sandbox.commit("first");

        sandbox.write(
            "tests/e2e_b.rs",
            r#"
                #[spec("dashboard/pane/005")]
                #[test]
                /// Scenario: a brand new test, only at ref-b.
                fn pane_005_y() { let _ = 2; }
            "#,
        );
        let sha_b = sandbox.commit("second");

        let outcome = run_compare(&sandbox.root, &sha_a, &sha_b).expect("compare");
        assert!(
            !outcome.has_removals,
            "adding a test must not read as a removal: {}",
            outcome.markdown
        );
        assert!(outcome.markdown.contains("dashboard/pane/005"));
        assert!(
            outcome
                .markdown
                .contains("## Removed (present at ref-a, missing at ref-b)\n\n_(none)_")
        );
    }

    /// Item 3's second required case: `ref-b` is missing a test `ref-a`
    /// had — a removal must be reported, naming the dropped catalog id.
    /// This is the exact shape fork issue #344 is about: PRD fork#197's
    /// tests disappeared along with the implementation commit they were
    /// bundled into when a sync dropped that commit as "superseded".
    #[test]
    fn compare_reports_removal_when_ref_b_drops_a_test() {
        let sandbox = Sandbox::new();
        sandbox.write(
            "tests/e2e_a.rs",
            r#"
                #[spec("hooks/delivery/001")]
                #[test]
                /// Scenario: will be dropped at ref-b.
                fn delivery_001_x() { let _ = 1; }
            "#,
        );
        let sha_a = sandbox.commit("first");

        fs::remove_file(sandbox.root.join("tests/e2e_a.rs")).expect("remove dropped test file");
        sandbox.write(
            "tests/e2e_c.rs",
            r#"
                #[spec("dashboard/pane/006")]
                #[test]
                /// Scenario: unrelated survivor, present at both refs really.
                fn pane_006_z() { let _ = 3; }
            "#,
        );
        let sha_b = sandbox.commit("second");

        let outcome = run_compare(&sandbox.root, &sha_a, &sha_b).expect("compare");
        assert!(
            outcome.has_removals,
            "dropping hooks/delivery/001 must be reported as a removal: {}",
            outcome.markdown
        );
        assert!(outcome.markdown.contains("hooks/delivery/001"));
        assert!(outcome.markdown.contains("delivery_001_x"));
    }

    /// Item 3's third required case: the SAME catalog id exists at both
    /// refs but its Scenario/body changed — this must show up as
    /// Modified, never as a false-positive Removed, by reusing
    /// `compute_modified`'s existing fingerprint comparison exactly as
    /// the module doc for `run_compare` states.
    #[test]
    fn compare_treats_a_same_id_edit_as_modified_not_removed() {
        let sandbox = Sandbox::new();
        sandbox.write(
            "tests/e2e_a.rs",
            r#"
                #[spec("hooks/delivery/001")]
                #[test]
                /// Scenario: original wording.
                fn delivery_001_x() { let x = 1; let _ = x; }
            "#,
        );
        let sha_a = sandbox.commit("first");

        sandbox.write(
            "tests/e2e_a.rs",
            r#"
                #[spec("hooks/delivery/001")]
                #[test]
                /// Scenario: reworded, same contract, function renamed too.
                fn delivery_001_x_renamed() { let x = 1; let _ = x; }
            "#,
        );
        let sha_b = sandbox.commit("second");

        let outcome = run_compare(&sandbox.root, &sha_a, &sha_b).expect("compare");
        assert!(
            !outcome.has_removals,
            "a same-id edit must not read as a removal: {}",
            outcome.markdown
        );
        assert!(outcome.markdown.contains("## Modified"));
        assert!(outcome.markdown.contains("hooks/delivery/001"));
        assert!(outcome.markdown.contains("delivery_001_x_renamed"));
    }

    /// `resolve_ref` must fail loudly on a ref that does not exist,
    /// mirroring `work_type::resolve_base`'s never-silent-success
    /// handling — a bad ref must never be silently treated as "no tests
    /// there" by `git ls-tree` further down.
    #[test]
    fn compare_fails_clearly_on_an_unresolvable_ref() {
        let sandbox = Sandbox::new();
        sandbox.write(
            "tests/e2e_a.rs",
            r#"
                #[spec("hooks/delivery/001")]
                #[test]
                /// Scenario: x.
                fn delivery_001_x() { let _ = 1; }
            "#,
        );
        let sha_a = sandbox.commit("first");

        let err = run_compare(&sandbox.root, &sha_a, "not-a-real-ref")
            .expect_err("an unresolvable ref must fail, not silently succeed");
        assert!(err.contains("not-a-real-ref"), "{err}");
    }
}
