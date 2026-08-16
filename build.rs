use std::process::Command;

// Issue #250: the version/build-id resolution order lives in a shared file so
// a unit test can exercise it. `cargo test` cannot reach code inside a build
// script — this script is compiled as its own binary and the test crate cannot
// import from it — so the pure functions live next door and
// `tests/build_version.rs` declares the same file as a module via `#[path]`.
// Everything impure (the `git` subprocesses and the `cargo:` emission below)
// stays here. A `mod` rather than an `include!` because rustfmt does not follow
// `include!` and the shared file would then escape `cargo fmt --check`.
mod build_version_resolve;

use build_version_resolve::{
    VersionSource, escape_for_cargo_warning, is_single_line_directive_value, normalize_build_id,
    normalize_release_repo, normalize_version, resolve_build_id, resolve_release_repo,
    resolve_version,
};

/// The GitHub repo the upgrade nudge polls for its "latest release" feed
/// (issue #398) when `DAD_RELEASE_REPO` is not injected. This is upstream's
/// own repo on purpose — a source build with no fork-local config must keep
/// polling the lineage it was actually built from. See the "fork sets it to
/// prageethw/dot-agent-deck" note in `.cargo/config.toml`'s `[env]` table for
/// where this fork overrides it.
const DEFAULT_RELEASE_REPO: &str = "vfarcic/dot-agent-deck";

fn main() {
    // Both values are injectable from the build environment (issue #250), so
    // cargo has to watch them: without these, an injected value would be baked
    // in once and then silently go stale across rebuilds.
    println!("cargo:rerun-if-env-changed=DAD_VERSION");
    println!("cargo:rerun-if-env-changed=DAD_BUILD_ID");
    println!("cargo:rerun-if-env-changed=DAD_RELEASE_REPO");

    let injected_version = build_env("DAD_VERSION");
    let injected_build_id = build_env("DAD_BUILD_ID");
    let injected_release_repo = build_env("DAD_RELEASE_REPO");

    // Resolution order (issue #250): injected env -> git tag -> CARGO_PKG_VERSION.
    // Git tags look like "v0.7.1" -> "0.7.1", "v0.25.0-alpha.0" -> "0.25.0-alpha.0";
    // an injected DAD_VERSION is validated the same way and falls through when
    // invalid, because `src/version.rs` parses it with `semver` and `.expect()`s.
    // The rejected value is rendered with `escape_for_cargo_warning`, never
    // interpolated raw: it failed validation precisely because it can be
    // anything, and `cargo:warning=` is parsed by the same line-oriented
    // protocol as every other directive.
    if let Some(raw) = injected_version.as_deref()
        && normalize_version(raw).is_none()
    {
        println!(
            "cargo:warning=DAD_VERSION=\"{}\" is not a valid SemVer version and was ignored \
             (`semver::Version::parse` grammar: an X.Y.Z core, an optional `v` prefix, an \
             optional `-<prerelease>` suffix, an optional `+<build>` suffix). Falling back to \
             the git tag / CARGO_PKG_VERSION.",
            escape_for_cargo_warning(raw)
        );
    }
    let version = resolve_version(
        injected_version.as_deref(),
        git_tag().as_deref(),
        env!("CARGO_PKG_VERSION"),
    );
    if version.source == VersionSource::Placeholder {
        println!(
            "cargo:warning=No version available: neither DAD_VERSION nor a git tag resolved, so \
             this build reports the CARGO_PKG_VERSION placeholder `{}` (issue #250). Version \
             negotiation, the upgrade nudge and `remote add` will all misbehave against it. Set \
             DAD_VERSION=<x.y.z> in the build environment, or build from a checkout with tags.",
            version.value
        );
    }
    emit_rustc_env("DAD_VERSION", &version.value);

    // PRD #103 M1.0: emit a finer-grained build identifier alongside DAD_VERSION.
    // Shape: `<DAD_VERSION>-g<short-sha>[-dirty]`, falling back to
    // `<DAD_VERSION>-unknown` when git metadata is unavailable.
    let short_sha = git_short_sha();
    // Only ask git whether the tree is dirty when there is a sha to qualify;
    // without one the answer cannot reach the composed id anyway.
    let dirty = short_sha.is_some() && git_is_dirty();
    // An injected build id outside the bounded alphabet is ignored the same way
    // an invalid version is — it falls through to the composed / `-unknown`
    // value — and the warning escapes it, because an interior newline in this
    // value is exactly what used to inject a second `cargo:` directive.
    if let Some(raw) = injected_build_id.as_deref()
        && normalize_build_id(raw).is_none()
    {
        println!(
            "cargo:warning=DAD_BUILD_ID=\"{}\" was ignored: a build id must be a single line of \
             ASCII alphanumerics, `.`, `-`, `+` or `_` (the shape is \
             `<version>-g<short-sha>[-dirty]`). Falling back to the git-composed build id.",
            escape_for_cargo_warning(raw)
        );
    }
    let build_id = resolve_build_id(
        injected_build_id.as_deref(),
        &version.value,
        short_sha.as_deref(),
        dirty,
    );
    emit_rustc_env("DAD_BUILD_ID", &build_id);

    // Issue #398: the upgrade nudge's "latest release" feed is a build-time
    // value too, resolved the same injected-env-or-default way as the two
    // values above. Unlike DAD_VERSION/DAD_BUILD_ID there is no git-derived
    // step — the repo a checkout was cloned from doesn't tell you which
    // GitHub repo's releases to poll, so an unset/invalid value degrades
    // straight to DEFAULT_RELEASE_REPO.
    if let Some(raw) = injected_release_repo.as_deref()
        && normalize_release_repo(raw).is_none()
    {
        println!(
            "cargo:warning=DAD_RELEASE_REPO=\"{}\" is not a valid \"owner/name\" GitHub repo \
             slug and was ignored. Falling back to {DEFAULT_RELEASE_REPO}.",
            escape_for_cargo_warning(raw)
        );
    }
    let release_repo = resolve_release_repo(injected_release_repo.as_deref(), DEFAULT_RELEASE_REPO);
    emit_rustc_env("DAD_RELEASE_REPO", &release_repo);

    // Re-run if HEAD changes (new commit, branch switch, detached-HEAD move).
    // `.git/HEAD` alone is necessary but not sufficient on a normal branch —
    // the file contents `ref: refs/heads/<branch>` don't change when commits
    // land on that branch. PRD #103 M1.0 prescribes also watching the
    // resolved ref file, the index (dirty/clean transitions), and
    // packed-refs (post-`git gc` fallback).
    //
    // In a git worktree, `.git` is a *file* containing
    // `gitdir: <main-repo>/.git/worktrees/<name>`, not a directory. The
    // literal `.git/HEAD` / `.git/index` paths then point at nothing and
    // `cargo:rerun-if-changed` silently no-ops, which means cached
    // `DAD_BUILD_ID` doesn't invalidate on commit (violates PRD invariant
    // 6). Resolve each path through `git rev-parse --git-path <name>` so
    // we watch the real file under `.git/worktrees/<name>/...` (or the
    // shared `commondir` for packed-refs). Fall back to the literal path
    // only if `git rev-parse` fails — matches the existing degrade-to-
    // unknown discipline elsewhere in this file.
    emit_rerun_if_changed_git_path("HEAD");
    emit_rerun_if_changed_git_path("index");
    emit_rerun_if_changed_git_path("packed-refs");
    if let Some(ref_path) = parse_head_ref_path() {
        emit_rerun_if_changed_git_path(&ref_path);
    }
}

/// Emit `cargo:rustc-env=<name>=<value>` after a final line-protocol check.
///
/// Cargo reads build-script stdout line by line, so a value containing CR/LF
/// would let whoever supplied it append a second directive of their choosing
/// (`rustc-cfg`, `rustc-link-arg`, another `rustc-env`). Every value reaching
/// here has already been validated — `semver` for the version, the bounded
/// alphabet for the build id, Cargo itself for `CARGO_PKG_VERSION`, hex for the
/// short sha — so this is a backstop that only fires if a future edit adds an
/// unvalidated source. Failing the build loudly is the right outcome then:
/// emitting the value is the one thing we must not do.
fn emit_rustc_env(name: &str, value: &str) {
    assert!(
        is_single_line_directive_value(value),
        "refusing to emit cargo:rustc-env={name}: the value is not a single safe line (\"{}\")",
        escape_for_cargo_warning(value)
    );
    println!("cargo:rustc-env={name}={value}");
}

/// Emit `cargo:rerun-if-changed` for a git-internal path resolved via
/// `git rev-parse --git-path <relative>`. In a worktree this returns the
/// real path under `.git/worktrees/<name>/...` (for HEAD/index) or the
/// shared `commondir` for `packed-refs` and `refs/heads/<branch>`.
///
/// Falls back to the literal `.git/<relative>` path if `git rev-parse`
/// fails (no git, shallow tarball, ...). Cargo's `rerun-if-changed`
/// silently tolerates non-existent paths, so the fallback is harmless
/// outside a real repo.
fn emit_rerun_if_changed_git_path(relative: &str) {
    let resolved = Command::new("git")
        .args(["rev-parse", "--git-path", relative])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        // Same line-protocol rule as `emit_rustc_env`: a path is git-supplied,
        // so a CR/LF in it would open a second directive. Fall back to the
        // literal path rather than emitting it.
        .filter(|s| is_single_line_directive_value(s))
        .unwrap_or_else(|| format!(".git/{relative}"));
    if !is_single_line_directive_value(&resolved) {
        // Only reachable if `relative` itself is unsafe, which
        // `parse_head_ref_path` already rules out. Watching nothing is a
        // stale-build-id risk; emitting it is a directive-injection one.
        return;
    }
    println!("cargo:rerun-if-changed={resolved}");
}

/// Read `name` from the build environment, treating unset, empty and
/// all-whitespace alike as "not injected" so `DAD_VERSION=` in a CI script
/// behaves like no injection at all rather than emitting a blank version.
fn build_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The raw `git describe --tags --abbrev=0` output for the build checkout, or
/// `None` when git is unavailable / this is not a repo / no tags are reachable.
/// Validation and the `v` strip happen in [`normalize_version`], which the
/// injected value goes through too.
fn git_tag() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let tag = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if tag.is_empty() { None } else { Some(tag) }
}

/// The short HEAD sha, or `None` when git metadata is unavailable. Any git
/// failure degrades to `None` rather than aborting the build, so tarball /
/// shallow-clone builds still produce a usable `DAD_BUILD_ID` (the
/// `-unknown` sentinel composed by [`resolve_build_id`]).
fn git_short_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

fn git_is_dirty() -> bool {
    let Ok(output) = Command::new("git").args(["status", "--porcelain"]).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    !output.stdout.is_empty()
}

/// Resolve the symbolic ref path HEAD points at (e.g. `refs/heads/main`)
/// via `git symbolic-ref -q HEAD`. Returns `None` on detached HEAD (the
/// existing HEAD watch already covers that case) or when git isn't
/// available.
///
/// We can't read `.git/HEAD` directly: in a worktree, `.git` is a *file*
/// containing `gitdir: ...`, not a directory, so the literal path
/// points at nothing. `git symbolic-ref` handles both layouts uniformly
/// and prints just the ref path (or fails silently with a non-zero exit
/// when HEAD is detached).
fn parse_head_ref_path() -> Option<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ref_path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if ref_path.is_empty() || !is_single_line_directive_value(&ref_path) {
        None
    } else {
        Some(ref_path)
    }
}
