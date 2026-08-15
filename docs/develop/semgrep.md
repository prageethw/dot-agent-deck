# Semgrep CE static analysis

CI runs a `semgrep:` job (`.github/workflows/ci.yml`, added in PR #85) alongside `sonarqube:`. Neither replaces the other: Semgrep covers security-pattern matching, Sonar covers bugs/maintainability/complexity with a real Rust ruleset. See [SonarQube Cloud analysis](docs/develop/sonarqube.md).

## Packs, and why `p/bash` isn't one of them

The job runs `semgrep scan --config p/rust --config p/github-actions --config p/secrets`. `auto` is deliberately not used — it is broad, slower, and phones home telemetry. `p/bash` was tried and dropped: it 404s against the current Semgrep registry, which has no general bash/shell ruleset pack, only an unrelated `reverse-shells` pack that happens to tag `bash` as a language.

## The job is blocking — `--error`, not the absence of `--no-error`

`--no-error` is `semgrep scan`'s **default** (verified against the upstream CLI reference: `--error/--no-error … [default: --no-error]`). This matters because the job's first version used `--no-error` explicitly as documentation of a first-rollout, non-blocking choice — and issue #146 found that the natural-sounding fix, "drop `--no-error`", is a no-op: removing a flag that was never doing anything changes nothing. Making the job blocking requires **adding `--error`**, which is what `ci.yml` now does.

This is safe on this repo specifically: `main` has no branch protection and nothing `needs:` the `semgrep` job (verified across all workflow files), so a failing scan colours the board without gating a merge or the direct pushes `release.yml`/`docs-publish.yml` make to `main`. The `upload-sarif` step carries `if: always()`, so findings still reach GitHub code scanning when the scan step exits non-zero.

**A green `semgrep` check before this change meant "the scan ran", not "the scan found nothing".** That distinction is now recorded in CLAUDE.md rule 8 alongside the same failure mode in the `e2e:` job and SonarQube's `SONAR_TOKEN` gate — every automated check in this repo has its own way of being silently empty, and the check's colour alone never proves which one you're looking at.

## The four `--exclude-rule` flags

`semgrep scan` also excludes four rules by full ID. Each is a deliberate call, not a default:

- **`rust.lang.security.unsafe-usage.unsafe-usage`** — fires on every `unsafe {}` block. This is a systems codebase doing openpty / process control / raw fd handling, so `unsafe` is expected rather than a defect. The rule produced 209 of the first run's 286 findings, drowning everything else. Excluding it is a readability decision, not a claim the `unsafe` code is audited.
- **`rust.lang.security.current-exe.current-exe`** — fires on every `std::env::current_exe()` call. The rule's threat model needs a privilege boundary (setuid/setgid) this non-setuid, same-uid program does not have. `current_exe()` is chosen over `$PATH` *because* it is the more trustworthy of the two here (`src/daemon_attach.rs:247`) — the `$PATH` alternative broke three times (`493248b`, `bbf2236`, `ea8c748`). Issue #146 triaged all 13 open findings: 0 true positives.
- **`rust.lang.security.args.args`** — fires on every `std::env::args()` call, but the rule is specifically about `argv[0]` being attacker-controllable via `execve`. No site in this repo reads `argv[0]`; four of the five flagged sites explicitly `.skip(1)`/`.nth(1)` past it. Issue #146 triaged all 5 open findings: 0 true positives.
- **`rust.lang.security.temp-dir.temp-dir`** — fires on every `std::env::temp_dir()` call. Unlike the three rules above, this one's threat model *can* apply here — a predictable name in a shared directory is a real hazard — so excluding it is a readability decision, not a claim the pattern is inapplicable. Issue #146 triaged all 6 open findings: all benign today (read-only use, or already behind `tempfile`'s secure-creation path), but one (`src/embedded_pane.rs:346`) has the rule's exact shape and is benign only because nothing currently connects to that path — unreached, not absent. Revisit if a future change makes that path load-bearing.

## Reading the findings

Excluded rules never reach Semgrep's own finding count, so `--error` cannot see them — but the alerts are also dismissed on the GitHub side (`state=dismissed`, `dismissed_reason=false positive`) so the audit trail says why rather than leaving a bare exclusion comment as the only record. Findings that aren't excluded live in GitHub code scanning, not in the check:

```
gh api 'repos/prageethw/dot-agent-deck/code-scanning/alerts?state=open' \
  --jq '.[] | "\(.rule.id)\t\(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"'
```

## Why block on this scanner at all

The alert history is the argument. Across 290 total alerts (24 open at the time of #146's triage, 266 closed), the closed set breaks down as 209 `unsafe-usage` (excluded, not fixed) plus **57 genuinely fixed findings, all from `p/github-actions`**: 52 mutable action tags now SHA-pinned, 4 shell-injection findings, 1 `secrets: inherit` finding. That is a clean signal about where this scanner's value is — supply-chain regressions in the workflows themselves, exactly the class of change Renovate can reintroduce automatically and a human reviewer's eye slides past. A non-blocking scan catches those only if someone remembers to open the code-scanning tab; a blocking one is unusually cheap to run here because there is no branch protection for a red job to interact badly with.

## Out of scope

`.semgrepignore` exists for path-based exclusion (generated/vendored paths), not rule-based — it is not a mechanism for dismissing a rule the way `--exclude-rule` is. `nosem` comments were considered and rejected as the primary mechanism for the four excluded rules: dozens of annotations across many files, on lines that are correct as written, is a permanent readability tax paid to a tool that got it wrong. `docs-publish.yml`, `release.yml`, and the `fork-only` branch stack are untouched.
