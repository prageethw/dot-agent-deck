# PRD fork#340: One work-type vocabulary, derived from the diff and checked by a gate rather than a prompt

**GitHub Issue**: [fork #340](https://github.com/prageethw/dot-agent-deck/issues/340)

**Priority**: Medium

**Status** *(2026-08-15)*: **In progress — M1–M5 unstarted.** Research and design complete and recorded here. Issue filed, worktree `../dot-agent-deck-worktype` created on branch `feat/340-work-type-vocabulary` from `5d2a9205`, issue claimed. Resume checklist steps 1–3 are done; execution begins at step 4 (M1 then M2, delegated to `coder`), after the orchestrator's test-plan gate.

**Sibling**: [fork #339](https://github.com/prageethw/dot-agent-deck/issues/339) — the other PRD parked in `12ccb6df`, merged as PR [#342](https://github.com/prageethw/dot-agent-deck/pull/342) (`5d2a9205`). Its file was renamed to `prds/fork-339-agent-type-badge-toggle.md` on that branch, so **M4's optional rename is already done** — it is no longer the 1-of-129 `prds/` file lacking an issue number, and R4 has nothing left to fix up there.

**Fork-only?** **Mixed, and the split is clean.** M1 (reconciling `assemble-changelog.sh` with `pyproject.toml`) and M3–M4 (the `xtask` gate) are **upstream-worthy** — they fix a latent release-blocker and add a general-purpose gate in shared code. M2's CLAUDE.md clause and the `docs/develop/work-types.md` vocabulary are **fork preference** (upstream's label set differs — it carries both `feature` and `enhancement`, and a live `PRD` label). Per rule 19: build and merge here, then file the upstream-offer issue at merge time for M1/M3/M4 only.

**Related**: `docs/develop/empty-gates.md` (the failure mode this PRD is entirely about) · `docs/develop/versioning.md` (names `pyproject.toml` as the changelog-type supplier) · CLAUDE.md rules 13 (sync-clobbered skills), 16 (named supplier), 5 (`--workspace` on both test aliases)

---

## Problem Statement

The request was *"add work-item tagging to the deck — `BUG` / `PRD` / `DOC`, persisted, displayed, filterable."* **This repo does not lack tagging.** It has **five** surfaces carrying work-type vocabulary, three of which disagree with each other, and two of which are already hard gates that can block a release.

| # | Surface | Vocabulary | Gated? |
|---|---|---|---|
| 1 | `pyproject.toml` `[[tool.towncrier.type]]` | `feature`, `bugfix`, `breaking`, `doc`, `misc` | **the declared supplier** — named by `docs/develop/versioning.md:20`; `dot-ai-changelog-fragment`'s Step 3 tells agents to read it |
| 2 | `scripts/assemble-changelog.sh:25` | **nine** — those five plus `added`, `changed`, `fixed`, `removed` | **yes**, hard-fails on anything else (`:31-51`) |
| 3 | `.claude/skills/dot-ai-tag-release/analyze.sh:54-68` | **five** — hard-fails on the other four | **yes, and runs first** |
| 4 | GitHub labels (22 live on the fork) | `bug`, `documentation`, `enhancement`, + area (`source`/`tests`/`config`/`ci-cd`/`dependencies`), priority/size (`TRIAGE_LABELS`), process (`in-progress`) | no |
| 5 | `prds/` (129 files) | the `PRD` label — **which does not exist on this fork** | no |

Adding `BUG|PRD|DOC` as requested would create a **sixth** vocabulary. The work worth doing is reconciling these onto one, and enforcing it by a mechanism that does not depend on which agent is running.

### Why the enforcement must be vendor-neutral

The original proposal routed the vocabulary through `.claude/templates/` and `CLAUDE.md`. That is unusable here as the primary mechanism: this fork's `reviewer` and `auditor` roles ran on **codex** until 2026-08-14 (commit `ad93333`), and the product ships adapters for `ClaudeCode`, `OpenCode`, `Pi`, `Codex` and `Devin` (`src/agent_registry.rs:178-284`). A checklist only Claude can find is one the fork's own review roles could not read for most of its life.

It is also the weaker mechanism regardless of vendor. **This repo has documented evidence that prompt-only enforcement decays**: CLAUDE.md rule 17 records the orchestrator writing `tests/` despite its role prompt forbidding it in as many words, concluding *"role adherence decays with session length — worst exactly when an orchestration is long and complex."* A checklist living only in a prompt is an empty gate by construction — it looks identical whether it was followed or never read.

**A gate that reads the diff is agent-agnostic for free**, and this repo already has the pattern: `assemble-changelog.sh:31-51` is exactly that, and `docs/develop/empty-gates.md` is a whole document about the failure mode it prevents.

### Three findings that constrain the design

**F1 — the changelog suffix vocabulary is FROZEN at five, and this is not negotiable.**
`.claude/skills/dot-ai-tag-release/analyze.sh:54-68` hard-errors (`ERROR=true`, `exit 1`) on any fragment suffix outside `feature|bugfix|breaking|doc|misc`, **and that file is in the `dot-ai-*` sync-clobbered zone** — CLAUDE.md rule 13 records `04a3641` being reverted byte-for-byte by `e94388d` five days later. So introducing a `.chore.md`, `.prd.md` or `.bug.md` suffix would break `/tag-release` at the first release, with a fix that **cannot be carried in-repo**. Every new work type must therefore **alias onto one of the five** — which is precisely the move `TYPE_HEADERS` (`assemble-changelog.sh:12-22`) already makes, collapsing nine names onto six headers (`added|feature → Added`, `fixed|bugfix → Fixed`, `changed|breaking → Changed`).

**F2 — migration is zero.** `changelog.d/` on disk contains exactly `.gitkeep`; `assemble-changelog.sh`'s final loop `rm -f`s every processed fragment, so the backlog is consumed at each release. (An earlier count of "712 fragments" was counting add+delete events in git history — 226 distinct tracked paths ever existed, none of them on disk now.) **Nothing needs renaming and `CHANGELOG.md` is never rewritten.**

**F3 — `breaking` is a severity axis, not a work type.** It feeds the version bump (`analyze.sh:84-99`; CLAUDE.md rule 12 — breaking → minor while `0.x`). A `bug` can be breaking and so can a `prd`. Keeping it out of the work-type vocabulary avoids forcing a wrong classification on the first breaking bugfix.

---

## Solution Overview

**One vocabulary — `bug | prd | doc | chore` — mapped by alias onto every existing surface, plus one gate that derives a change's type from the diff and enforces what is mechanically checkable.**

### Decisions taken (do not re-litigate on resume)

| # | Decision | Rationale |
|---|---|---|
| D1 | Vocabulary is `bug \| prd \| doc \| chore` | `chore` closes the largest homeless category (dep bumps, CI, refactors, formatting) and is **already this repo's spelling** in 3 branches and 16 of the last 400 commits. |
| D2 | `chore` aliases to the existing `.misc.md` suffix; **no new suffix is created** | F1. `misc`'s own `pyproject.toml` description already reads "Internal improvements, CI/CD, tooling, refactoring". |
| D3 | The live `enhancement` label **stays** and is the on-GitHub spelling of `prd` | No destructive migration against a GitHub-default label. Honours the original "no separate ENHANCEMENT *type*" intent without fighting GitHub's defaults. |
| D4 | `breaking` stays out of the vocabulary | F3. |
| D5 | Enforcement is a **diff-reading gate**, never a prompt instruction | Vendor-neutral by construction; rule 17's decay evidence. |
| D6 | Retire `added`/`changed`/`fixed`/`removed` from `assemble-changelog.sh` | Never used (`changed`/`fixed`/`removed` **zero times**; `added` once). Accepted by one gate and rejected by another — a latent release-blocker, not merely dead. |
| D7 | No user-facing `type:` token anywhere | It is taken four times over; see "Naming collision". |

### The vocabulary

| Work type | changelog suffix | GitHub label | in `prds/`? | branch prefix | commit prefix |
|---|---|---|---|---|---|
| **`bug`** | `.bugfix.md` | `bug` *(exists)* | no | `fix/` — 13 live | `fix:` — 103/400 |
| **`prd`** | `.feature.md`, or `.breaking.md` when it breaks the TUI↔daemon contract | **`enhancement`** *(exists)* | **yes** — `prds/<n>-<slug>.md` or `prds/fork-<n>-<slug>.md` | `feat/` — 4 | `feat:` — 19/400 |
| **`doc`** | `.doc.md` | *none — see C1* | no | `docs/` — 8 | `docs:` — 84/400 |
| **`chore`** | `.misc.md` | `chore` — *the one label to create* | no | `chore/` — 3 | `chore:`/`ci:`/`test:`/`refactor:` — 79/400 |
| *(severity, not a type)* | `.breaking.md` | — | — | — | — |

**Every branch and commit prefix above is already in use.** Nothing is invented.

### Conflicts, each verified

**C1 — `documentation` is an area label wearing a type label's name.** `.github/labeler.yml:1-6` auto-applies it on `pull_request_target` to any PR touching `docs/**` or a root `*.md`. Every `prd` PR that updates docs — which rule 11 and the `dot-ai-write-docs` skill both require — gets it automatically. **So it cannot mean "this is doc work."** It stays an area label; `doc` work is identified by the gate's supplier instead. Do not try to make this the `doc` type label — that fight is unwinnable and the result would be a silent empty gate.

**C2 — upstream carries both `feature` and `enhancement`; the fork carries only `enhancement`.** The fork has no problem today, but it syncs from upstream and inherits its habits. Record that on this fork `enhancement` is the sole `prd` label and `feature` is not to be created.

**C3 — the fork has no `PRD` label; upstream does** (`#0052CC`, "Product Requirements Document"). So `.claude/skills/dot-ai-prds-get` — *"Fetch all open GitHub issues that have the 'PRD' label"* — **returns nothing on this fork, silently.** A live empty gate in existing tooling. Under D3's no-destructive-migration rule the answer is **not** to create a `PRD` label (it would duplicate `enhancement`) but to record that the skill is inert here and `enhancement` + `prds/` is the fork's answer.

**C4 — `assemble-changelog.sh` and `pyproject.toml` disagree, and the stricter one runs first.** `/tag-release` invokes `analyze.sh` **before** `assemble-changelog.sh`, so the four extra names are worse than dead: a fragment named `5.added.md` would today **block the release** with `Unknown fragment type`, from a suffix `assemble-changelog.sh` swears is legal. This is exactly the two-sources-disagree pattern `empty-gates.md` documents. D6 fixes it.

**C5 — `pyproject.toml` declares `filename = "docs/CHANGELOG.md"` and an `issue_format` pointing at `vfarcic`, but towncrier is never invoked** — the shell script is. Harmless, but it means the file reads as config and behaves as a vocabulary declaration. Note it in a comment; do not "fix" it.

### Dead aliases — retire all four

Across the entire git history of `changelog.d/`: `bugfix` 115, `feature` 75, `misc` 12, `breaking` 10, `doc` 2 — and `added` **1**, `changed` **0**, `fixed` **0**, `removed` **0**.

Historical malformed names, all predating the current gate and all now caught by it: `.fix.md` ×7 (the v0.24.3 incident recorded at `assemble-changelog.sh:27-30` — those fragments were silently ignored, leaving the GitHub release body and `CHANGELOG.md` empty for that version), `.docs.md` ×1, and three fragments with **no type suffix at all**.

---

## The gate — `cargo xtask work-type-check`

### The supplier (CLAUDE.md rule 16)

**Two tiers, then failure.**

- **Tier 1 — the changelog fragment suffix added in this diff.** `changelog.d/<stem>.<suffix>.md`, mapped through the alias table above. It is **already the supplier**, declared in `pyproject.toml`, consumed by two gates, and produced by an existing skill. It is **in the diff**, so `git diff --name-only` reads it — no API call, no token, no permissions, no event ordering, and identical locally and in CI.
- **Tier 2 — the branch prefix**, for the majority of PRs that carry no fragment: `fix/`→`bug`, `feat/`→`prd`, `docs/`→`doc`, `chore/`→`chore`. All four are the de-facto convention already (28 prefixed branches live now). **Rule 16 requires this to have its own supplier, and it already does**: CLAUDE.md rule 1 obliges the orchestrator to state the branch name in every delegated task. The change is one clause — that branch name carries a work-type prefix. No new party, no new obligation shape.
- **Tier 3 — failure**, naming both ways to fix it. This is what stops the gate being vacuous by omission.

**If both tiers supply and disagree: fail, naming both.** A `fix/` branch with a `.feature.md` fragment is either a mislabelled branch or a feature about to ship as a patch release (`analyze.sh:99`) — both worth five seconds of a human's attention. Do not let the fragment silently win.

### Rejected suppliers, each for a fatal reason

**The PR label — rejected on four independent grounds, any one fatal:**
1. `ci.yml:4-6` triggers on `types: [opened, synchronize, reopened]`. **`labeled` is not among them.** Adding a label after a red gate never turns it green; removing one after a green gate never turns it red. The check's result becomes permanently decoupled from the value it claims to check.
2. `labeler.yml` is a **separate workflow on `pull_request_target`**; at `opened` the two race and CI can legitimately read zero labels.
3. On a `CONFLICTING` PR **no `pull_request` run is created at all** (fork #150), while the `pull_request_target` labeler still fires — so the label exists with no gate behind it.
4. Not in the diff, so it has no local form — and `CONTRIBUTING.md:55` step 4 is a local gate.

**Front-matter — rejected.** There is no `.github/ISSUE_TEMPLATE/`, no PR template (`.github/` holds only `CODEOWNERS` and `labeler.yml`), and `prds/*.md` have no front matter. Nothing supplies it on day one — the exact rule-16 trap.

**Commit-message prefix — rejected as primary.** Richer (8 prefixes in use) but a branch has many commits with many prefixes, needing a merge rule, and CONTRIBUTING explicitly blesses many commits per branch. Keep it as documented convention.

### Where the gate lives

**A new subcommand of the existing `xtask-linkage-check` binary — `cargo xtask work-type-check` — not rule 10 of `linkage-check`.**

- **Reuse:** `xtask/linkage-check/src/list_tests.rs` already resolves `git merge-base HEAD origin/main` (`:636-648`) and computes Created/Modified `#[spec]` sets with body fingerprints (`:22-47`). The `bug` rule is one call into that. The subcommand multiplexer at `main.rs:198` takes a new arm in three lines.
- **Not rule 10**, because `linkage-check` also runs on `push: [main]` (`ci.yml:216`), where no base ref is meaningful. Folding it in forces an internal skip — and a rule that silently skips inside a tool whose success line reads `linkage-check: ok (…, 9 rules)` (`main.rs:483`) is the empty gate this PRD exists to prevent.
- **Not `assemble-changelog.sh`**, which runs once per release, ~50 merges after the mistake, when the branch is gone. A gate that cannot block the PR that broke it is not a PR gate.
- **Not a CI-only job.** `CONTRIBUTING.md:55` has contributors run `cargo xtask linkage-check` locally; a CI-only gate would be the first repo gate with no local form and would break the green-locally-equals-green-in-CI property `ci.yml:186-190` spends twenty lines defending.

**CI wiring**, beside `ci.yml:216`:
```yaml
- run: cargo xtask work-type-check --self-test
- run: cargo xtask work-type-check
```

**One blocking implementation detail.** `ci.yml:132` uses `actions/checkout` with **no `fetch-depth`** (depth 1), and on a `pull_request` event there is no `origin/main` ref at all — so `git merge-base HEAD origin/main` fails. `list-tests` has never run in CI, so this has never mattered. Required: `fetch-depth: 0` on the `build` job, a `--base <ref>` flag defaulting to `origin/main`, and a **non-zero exit with a distinct code** when the base cannot be resolved. **Never exit 0 on an unresolvable base** — that is how this gate becomes empty on day one.

### The rules

Note that two of the four originally proposed were **rejected as wrong**, and the rejections are load-bearing — a gate that fires on legitimate work weekly trains everyone to route around it, and is then worse than nothing.

| Rule | Verdict and content |
|---|---|
| **R0** — declaration | **Accept; the spine.** Derive the type (fragment suffix, else branch prefix). Fail if neither supplies; if two added fragments map to different types; if the tiers disagree; or on a suffix outside the five. Every PR exercises it, so it cannot go vacuous. |
| **R1** — `doc` | ~~zero diff under `src/`~~ **REJECTED** — rustdoc lives in `src/` and is genuinely edited as doc work (`xtask/linkage-check/src/main.rs:1-60` is a 60-line module doc; branch `fix/242-crossterm-doc-contract` is a live case). **Refined:** *positive* — must touch `docs/**`, `site/**`, a root `*.md`, or `prds/**`; *narrow negative* — must **not add a `#[spec(`** and must not change `tests/**`. Catches a feature shipped as `doc`, which produces a `.doc.md` fragment and therefore **no version bump at all** (`analyze.sh:59`). |
| **R2** — `chore` | ~~zero diff under `tests/`~~ **REJECTED OUTRIGHT** — `test:` is 43 of the last 400 commits, and the bare-`tempfile` sweep documented at `main.rs:74-120` is a pure chore touching all of `tests/`. **Refined:** must not add a new user-facing CLI surface — no added `#[arg(long = "`, no new `Commands::` variant, no *new* page under `docs/`. Catches a feature shipped as a chore: `.misc.md`, no version bump, no release note, no docs. |
| **R3** — `bug` | **Accept, sharpened.** "A file under `tests/` changed" is too weak (a whitespace edit to `CATALOG.md` satisfies it). Require **adding or modifying at least one `#[spec]`-annotated test**, body-fingerprinted so whitespace does not count — pure reuse of `list_tests`. This is the mechanical form of the existing `reproduce-first` skill, not a new demand. **Escape hatch:** a line beginning `No-Test: <reason>` **in the fragment** — mirroring `m2.allowlist`'s documented-exception pattern, and keeping the exception visible in the release-notes source. Needed for genuine cases (Windows symlink materialisation, CI-config bugs, the e2e tier itself). |
| **R4** — `prd` | **Accept, refined.** Requiring a `prds/**` file *in the diff* would fail most legitimate PRD PRs — a PRD spans many milestones and many PRs, and only the first touches the file (`prds/fork-256-…`: M1 shipped on PR #270, M2–M4 still open). Check **existence** on the filesystem instead: a `.feature.md`/`.breaking.md` fragment named `changelog.d/<n>.*` must have a matching `prds/<n>-*.md`, `prds/fork-<n>-*.md`, or either under `prds/done/`. |
| **R5** — source of truth | `assemble-changelog.sh` **reads `pyproject.toml`'s `[[tool.towncrier.type]] directory` entries** instead of hardcoding `TYPES` at `:25`. Deletes the C4 divergence structurally. Minimum acceptable fallback: cut the list to the five real types and comment-name `pyproject.toml` as the source — but prefer the parse, since "keep in lockstep" comments are exactly what produced C4. |

### Anti-empty-gate measures — the point of the PRD

- **`--self-test`** run in CI immediately before the real invocation, following `scripts/check-symlinks.sh --self-test` (`ci.yml:325-326`) verbatim: construct a genuinely violating case and assert rejection, so the same binary is shown failing seconds before it passes. Review it as production code — it must not decay into `assert!(derive("").is_err())`.
- **The success line names the derived type and the supplying tier**: `work-type-check: ok (work type 'bug' from branch prefix 'fix/', base <sha>, 5 rules)`. A bare green is unreadable — `empty-gates.md`'s whole thesis.
- **Non-zero exit on an unresolvable base**, with a test asserting exactly that.
- **Exemptions are printed, never silent**: `renovate/**`, `sync/**` and `upstream/**` are exempt (Renovate PRs automerge without a human, `ci.yml:14-16`; a gate that blocks them gets disabled), and the skip appears in the success line — `work-type-check: skipped (branch 'renovate/…' is exempt)`.

### Naming collision — `type:` is taken four times

`type:<agent>` in the TUI filter grammar (`filter_sessions` `src/ui.rs:2841`, tokens `:2873-2887`, resolved via `agent_registry::resolve_type_alias`), `agent-event --type` (`src/main.rs:201`), `remote add --type` (`:526`), `connect --type` (`src/connect.rs:34`).

The filter case is **silent**: `src/ui.rs:2889-2891` returns an empty `Vec` for an unrecognized token, so a user typing `type:bug` after reading the vocabulary doc gets **zero sessions and no error** — indistinguishable from "nothing matches".

**Avoidance:** the design introduces **no user-facing token** — the work type is derived, never typed. Where a name is unavoidable, use the two-word `work-type` / `work_type`: subcommand `work-type-check` (not `type-check`, which also reads as a compiler operation), fragment key `No-Test:` (not `type:`). If a TUI filter is ever wanted it must be `work:` or `kind:` **and rejected loudly** — which means fixing the silent early return at `src/ui.rs:2889` first. Out of scope; file as a follow-up.

---

## New vs. reconciliation — the honest accounting

**~80% reconciliation, ~20% new.** Four new artefacts, and only four:

1. **`cargo xtask work-type-check`** — one module in `xtask/linkage-check/src/`, ~350 lines plus tests. The only real code.
2. **`docs/develop/work-types.md`** — the vocabulary table and rationale, under rule 11's developer-docs home, linked from `CONTRIBUTING.md`.
3. **Two lines in `ci.yml`**, plus `fetch-depth: 0` on the `build` job.
4. **One GitHub label (`chore`)** — optional and decoupled, since the gate never reads labels.

Plus one clause in CLAUDE.md rule 1 (branch names carry a work-type prefix), which reaches codex and opencode through the `AGENTS.md → CLAUDE.md` symlink at zero cost.

**Already exists and is reused:** the five-type vocabulary and its `pyproject.toml` supplier; two gates already consuming it; the aliasing precedent in `TYPE_HEADERS`; three of four labels; idempotent label create/apply (`label_create_argv` `src/issue_dispatch.rs:1136`, `issue_edit_add_label_argv` `:387`, the `LabelSpec`/`TRIAGE_LABELS` pattern `:1072`, the ensure loop `issue_dispatch_run.rs:880`); all four branch and commit prefixes; `prds/` naming (128 of 129 conform); per-type workflow skills (`reproduce-first` = `bug`, `dot-ai-write-docs` = `doc`, `dot-ai-prd-create|start|done` = `prd`); the merge-base and `#[spec]`-delta machinery; the `--self-test` pattern; the subcommand multiplexer; and the vendor-neutral distribution already in place — `AGENTS.md → CLAUDE.md` plus **34 `.agents/skills/*` symlinks** guarded by `scripts/check-symlinks.sh` at `ci.yml:325`.

**Nothing durable lands in `.claude/skills/dot-ai-*`.** All four artefacts live in `xtask/`, `docs/develop/`, `CLAUDE.md`, `.github/` and `src/` — none clobbered by sync. Project-local skills (`reproduce-first`, `verify-pr`) may be edited to reference the vocabulary; those are ours (CLAUDE.md rule 13).

---

## Milestones — each independently shippable

**M1 — Reconcile the changelog vocabulary.** *(shell only; highest value per line)* Retire `added`/`changed`/`fixed`/`removed` from `assemble-changelog.sh:12-25`; have it read `pyproject.toml` (R5). Ships alone, fixes C4 — a real latent release-blocker — and needs no gate. Zero risk: all four names have ~zero usage.

**M2 — Write the vocabulary down.** *(docs only)* `docs/develop/work-types.md` carrying the table, the alias rationale, F3's "breaking is severity not type", conflicts C1–C3, and the note that `/dot-ai-prds-get` is inert on this fork. Link from `CONTRIBUTING.md`. Add the branch-prefix clause to CLAUDE.md rule 1.

**M3 — The gate, R0 only.** `cargo xtask work-type-check` with derivation, ambiguity checks, `--base`, `--self-test`, the honest success line, and CI wiring with `fetch-depth: 0`. **Shipping R0 alone is deliberate** — it surfaces the base-ref and fetch-depth problems against one rule rather than five.

**M4 — Rules R1–R4.** Each independently additive; split further if any proves noisy. Rename `prds/fork-agent-type-badge-toggle.md` to carry its issue number here (it is the 1 of 129 `prds/` files lacking one, and only R4 cares).

**M5 — The `chore` label.** `TYPE_LABELS` beside `TRIAGE_LABELS` (`src/issue_dispatch.rs:1072`), ensured in the existing loop (`issue_dispatch_run.rs:880`). Optional; ships last precisely because the gate does not depend on it — which is the point.

---

## Test plan

**Correction worth stating explicitly: xtask tests must NOT carry `#[spec]`.** `linkage-check` scans only `tests/` and the **root package's** `src/` (`main.rs:272-273`; `xtask/docs/src/lib.rs:74-75` sets the same two roots), so a `#[spec]` under `xtask/*/src/` is invisible to the scan and its catalog ID would fail **rule 1** (no annotation, not allowlisted). Confirmed by inspection: `xtask/linkage-check/src/clean_tmp.rs` has **48 `#[test]` and zero `#[spec]`**.

**Tier A — plain `#[test]` in `xtask/linkage-check/src/work_type.rs`**, following `clean_tmp.rs`'s `mod tests` (`:1467`). These run in the fast tier because both aliases carry `--workspace` (CLAUDE.md rule 5, issue #489), matched by `ci.yml:202`. No CATALOG entry, no `#[spec]`, no naming constraint. Cover:

- derivation from each of the five suffixes, including the `prd → feature|breaking` two-to-one map
- derivation from each branch prefix; unknown prefix → failure
- **neither supplier present → failure** *(the vacuity guard)*
- two fragments with conflicting types → failure, both named
- fragment and branch disagree → failure, both named
- a retired suffix (`.fix.md`, `.added.md`) → failure — regression-pins the v0.24.3 incident and C4
- **`--base` unresolvable → non-zero exit**, explicitly asserted *(the empty-gate guard; a test, not a comment)*
- R1: `doc` adding `#[spec(` → failure; `doc` touching only rustdoc in `src/` → **pass** *(pins the rejection)*
- R2: `chore` touching `tests/` heavily → **pass** *(pins the rejection)*; `chore` adding `#[arg(long = "` → failure
- R3: `bug` with no `#[spec]` delta → failure; modified body → pass; `No-Test:` in the fragment → pass; whitespace-only test edit → failure *(proves the fingerprint)*
- R4: `.feature.md` stem with no matching `prds/` file → failure; matching `prds/done/` → pass

Fixtures follow `xtask/linkage-check/tests/duplicate_catalog_id.rs`, which already builds synthetic sources containing `#[spec("fixture/dupcat/001")]` (`:38`). Mind linkage-check rule 8: use the harness temp helpers, never a bare `tempfile::tempdir()`.

**Tier B — optional `#[spec]` L2** under `tests/` (which *is* scanned), as a thin subprocess spawn in the shape of `cli/continue-removed/001`. Full ceremony: `#[spec("cli/work-type/001")]`, fn `work_type_001_…`, a mandatory `/// Scenario:` doc comment, and a `##### cli/work-type/001 — <headline>` entry in `tests/CATALOG.md` with the five bullets. Use `cli/` rather than inventing a `gates/` area.

**Tier C — `--self-test`.** Not a cargo test: a binary mode run in CI immediately before the real invocation. It covers what Tier A cannot — that the gate is wired up and running on this machine, on this run.

---

## Migration: zero

- **Changelog fragments** — nothing. `changelog.d/` holds only `.gitkeep` (F2). The only migration-shaped work is retiring four *unused* aliases (M1), touching zero files on disk.
- **`enhancement`-labelled issues** — nothing. Per D3 it is the on-GitHub spelling of `prd`; the alias is documentation and the gate never reads labels, which is exactly why it is free.
- **`prds/`** — one optional rename (`fork-agent-type-badge-toggle.md`), 128 of 129 already conform.
- **In-flight branches** — all 28 prefixed branches pass on day one via tier 2. Unprefixed ones (`fork-only`, `renovate/…`, `sync/…`, `upstream/…`) are covered by the printed exemption list or need a fragment.

---

## Risks

- **E1 — the base ref cannot be resolved and the gate exits 0.** Highest-probability failure: `ci.yml:132` is depth-1 with no `origin/main`. Mitigated by `fetch-depth: 0`, explicit `--base`, non-zero exit, and a Tier-A test.
- **E2 — the gate never runs on the PRs that matter.** `build` is skipped when `devbox_only`/`flake_only` (`ci.yml:128-129`) — those are Renovate's and exempt anyway, but keep the gate in `build` beside `linkage-check`, which has no path filter.
- **E3 — R0 becomes unfailable** once every branch is prefixed, leaving derivation untested in production. This is why `--self-test` is not optional and why the success line must name the supplying tier.
- **E4 — over-strict rules get bypassed rather than obeyed.** Both rejected rules would have fired on legitimate work weekly. Ship R0 alone (M3), watch, then add R1–R4 (M4).
- **E5 — `--self-test` rots into a tautology.** Review it as production code.
- **E6 — the vocabulary doc drifts from the gate.** Four copies is the same shape as C4. The gate reads `pyproject.toml`; the doc **names the file rather than restating the list**.
- **E7 — a sync commit reverts something.** Only if durable content lands in `dot-ai-*`. Nothing here does. Watch item: if `analyze.sh` ever needs to learn a new suffix, the design has drifted off F1 — go back and re-read it.
- **E8 — `chore` (label) vs `misc` (suffix) looks like a bug.** They are deliberately different spellings of one concept. The doc must state the alias table as intentional, with the emphasis `TYPE_HEADERS` gets.
- **E9 — non-numeric fragment stems.** A `.feature.md` with a non-numeric stem passes R0 and fails R4; seven historical fragments used them. Decide in M4 whether the numeric stem is required for `feature`/`breaking` — **recommended, and free right now** because `changelog.d/` is empty.

---

## Resume checklist

Everything below is unstarted.

1. **Rule 20 search over both trackers, issues AND PRs (`--state all`)** — `gh issue list` / `gh pr list` on `prageethw/dot-agent-deck` and `vfarcic/dot-agent-deck` for `changelog type`, `work type`, `towncrier`, `label vocabulary`. Then file the fork issue and rename this file to `prds/fork-<n>-work-type-vocabulary.md`.
2. **Create the worktree** (rule 1) at a disk-backed sibling, never the root checkout, never the scratchpad (rule 18):
   ```bash
   git worktree add -b feat/<n>-work-type-vocabulary ../dot-agent-deck-worktype origin/main
   cd ../dot-agent-deck-worktype && git branch --unset-upstream
   ```
   Push explicitly: `git push origin HEAD:refs/heads/feat/<n>-work-type-vocabulary`.
3. **Claim from inside that worktree**: `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck` — `worker-agent-deck`, not `dot-agent-deck` (`issue` is a fork-only subcommand).
4. **M1 then M2** — shell and docs, delegated to **coder**. Ship first: real value, no gate risk, no Rust. Open the **draft PR** on the first commit or the push fires no CI (rule 5).
5. **M3** — TDD: **tester** writes the Tier A RED tests, **coder** implements, **tester** confirms GREEN. Watch E1 on the first CI run.
6. **M4**, same TDD shape. **M5** optional, last.
7. **reviewer** + **auditor**, findings written to the root checkout's `.dot-agent-deck/` (rule 15; derivable via `dirname "$(git rev-parse --path-format=absolute --git-common-dir)"`).
8. **release** → `/prd-done`, marking the existing draft ready. Pause at the merge gate.
9. **At merge, file the upstream-offer issue** for M1/M3/M4 only (rule 19) — M2's vocabulary is fork preference and does not travel.

**Test-run policy throughout (rule 5 fork addendum): every test run happens in CI.** Workers commit, push, and read RED/GREEN from the PR. Only `cargo fmt --check`, `cargo clippy --workspace --all-targets --features e2e -- -D warnings` and `cd <worktree> && cargo xtask linkage-check` run locally. Neither local-run carve-out applies — no real-agent spawn/attach path is touched, and no e2e test changes, so the demo-reel step is skipped entirely. One push per delegation (rule 22), and the orchestrator states what that push must answer.

## Verification

- **M1** — run `bash scripts/assemble-changelog.sh <ver>` against a fixture `changelog.d/`: the five real suffixes assemble into the right headers; a `.added.md` now fails **at PR time** rather than at release time.
- **M3/M4 — CI fast tier**: the Tier A tests flip RED → GREEN. **And deleting the `--self-test` step must break the build** — that is the only proof the gate is wired up rather than merely present.
- **The gate proves itself on its own PR**: this branch is `feat/<n>-…` with a `.feature.md` fragment and a `prds/` file, so R0 and R4 both resolve on the very change that introduces them.
- **Vendor-neutrality check** — confirm `AGENTS.md` still resolves to `CLAUDE.md` and `scripts/check-symlinks.sh` passes, so the new CLAUDE.md clause reaches codex and opencode without a second file.
- **Read e2e results from the log, never the run conclusion** (rule 8): `gh run view <id> --repo prageethw/dot-agent-deck --log | sed 's/\x1b\[[0-9;]*m//g' | grep -E 'tests run:|TRY 3 FAIL'`.
