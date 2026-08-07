# SonarQube Cloud analysis

CI runs a `sonarqube:` job (`.github/workflows/ci.yml`) alongside the `semgrep:` job added in PR #85. Neither replaces the other: Sonar covers bugs, maintainability, and complexity with a real Rust ruleset (85 rules), while Semgrep covers security-pattern matching. Both are informational -- the Sonar quality gate is not wired as a blocking check.

## Why Cloud, not Community Build

SonarQube ships two self-hostable-adjacent options, and only one fits this repo. Community Build supports main-branch analysis only -- no pull request analysis, no non-main branches, and its own GitHub Actions guide recommends `on.push.branches` while telling you to avoid `on.pull_request` entirely. SonarQube Cloud's free plan supports both main and PR analysis, but only when the PR targets the main branch -- which is every PR here, since `ci.yml` only triggers `pull_request` against `main`. Cloud Free is free for public repositories, and `prageethw/dot-agent-deck` is public, so Cloud Free is strictly more capable here with no server to run or maintain.

## The `SONAR_TOKEN` secret

The job needs a `SONAR_TOKEN` repository secret (Settings -> Secrets and variables -> Actions) generated from the SonarCloud project's own token page. The `secrets` context is not reliably readable in a job-level `if:`, so the token is surfaced as a job-level `env: SONAR_TOKEN: ${{ secrets.SONAR_TOKEN }}` and each scan step is individually guarded with `if: env.SONAR_TOKEN != ''`. When the secret is absent -- most notably on a PR opened from a fork, since GitHub never exposes secrets to fork-originated workflow runs -- the job prints `SONAR_TOKEN not set - skipping SonarQube analysis` and every scan step is skipped rather than failing.

## Automatic Analysis must be off

SonarCloud enables "Automatic Analysis" by default on a newly imported project, and it conflicts with CI-driven analysis -- running both against the same project produces an error naming `autoscan` / automatic analysis. Turn it off in the SonarCloud project's Analysis Method settings before the CI job's first real run; this is a one-time change made in the SonarCloud UI, not in this repository.

## Correcting the org and project keys

`sonar-project.properties` at the repo root sets `sonar.organization=prageethw` and `sonar.projectKey=prageethw_dot-agent-deck` -- SonarCloud's conventional defaults for a GitHub-imported project (lowercased org login; `org_repo` for the key). These have not been confirmed against the actual SonarCloud project settings page. If CI fails with `Project not found` or `You're not authorized to run analysis`, open the project on SonarCloud, read the real organization and project key off its settings page, and update both values in `sonar-project.properties`.

## What gets scanned

`sonar.sources=.` with `sonar.exclusions` carving out generated and vendored paths: `target/`, `tests/snapshots/` (terminal-capture fixtures, not hand-written code), `site/build/` and `site/node_modules/` (the Docusaurus build output and its dependencies), and `.dot-agent-deck/` (per-clone dev state). This mirrors the scope reasoning in `.semgrepignore`.

The job also runs `cargo clippy --message-format=json > clippy-report.json` and imports it via `sonar.rust.clippyReport.reportPaths`, so Sonar's Rust analysis includes clippy's own findings rather than only its own static analysis. This invocation deliberately omits `-D warnings` (unlike CLAUDE.md rule 2's pre-commit gate) -- it exists to produce a report for Sonar to read, and a non-zero exit here would kill the job before the scan step runs.

## Out of scope (for now)

The Sonar quality gate is not blocking. Coverage reporting is not wired -- that needs a separate LCOV pipeline and is a later decision. `docs-publish.yml`, `release.yml`, and the `fork-only` branch stack are untouched; their own Semgrep follow-up is tracked separately.
