# Work types

This repo has one work-type vocabulary — `bug | prd | doc | chore` — and every surface that used to carry its own spelling of that vocabulary now aliases onto it (PRD [fork#340](https://github.com/prageethw/dot-agent-deck/issues/340)).

Before this PRD the repo had five surfaces carrying work-type vocabulary, three of which disagreed with each other, and two of which were already hard gates that could block a release: `pyproject.toml`'s towncrier types, `scripts/assemble-changelog.sh`'s hardcoded list, `.claude/skills/dot-ai-tag-release/analyze.sh`'s stricter list, GitHub labels, and the (upstream-only) `PRD` label convention that `prds/` files never actually used here.

## The vocabulary

| Work type | changelog suffix | GitHub label | in `prds/`? | branch prefix | commit prefix |
|---|---|---|---|---|---|
| `bug` | `.bugfix.md` | `bug` | no | `fix/` | `fix:` |
| `prd` | `.feature.md`, or `.breaking.md` when it breaks the TUI↔daemon contract | `enhancement` | yes | `feat/` | `feat:` |
| `doc` | `.doc.md` | *none — see "documentation is an area label" below* | no | `docs/` | `docs:` |
| `chore` | `.misc.md` | `chore` | no | `chore/` | `chore:`/`ci:`/`test:`/`refactor:` |

Every branch and commit prefix in this table is already in use on this repo — nothing here is invented. The `chore` GitHub label does not exist yet; creating it is optional follow-up work (M5) and is decoupled from the rest of this vocabulary, since nothing here reads GitHub labels to enforce anything.

## Why `chore` aliases onto `.misc.md` instead of getting its own changelog suffix

The changelog-fragment suffix set is frozen at five (`feature`, `bugfix`, `breaking`, `doc`, `misc`) because `.claude/skills/dot-ai-tag-release/analyze.sh` hard-errors on any fragment suffix outside that set. That file lives in the sync-clobbered `dot-ai-*` mirror (CLAUDE.md rule 13) — a project-local edit to it does not survive the next sync, as recorded there: commit `04a3641` was reverted byte-for-byte by `e94388d` five days later. A new `.chore.md` suffix would break `/tag-release` at the very first release carrying a chore fragment, with a fix that cannot be carried in-repo.

So `chore` deliberately aliases onto the existing `.misc.md` suffix, rather than getting a suffix of its own. This is the same move `assemble-changelog.sh`'s `TYPE_HEADERS` table already makes elsewhere, collapsing nine historical names onto six changelog headers.

**This alias is deliberate, not a bug.** `chore` (the GitHub label and work type) and `misc` (the changelog suffix) are different spellings of the same concept, chosen so the frozen five-suffix set never has to grow. Do not read `chore` vs. `misc` as an inconsistency to "fix."

## `breaking` is a severity axis, not a work type

`breaking` drives the version bump (minor while the repo is `0.x`, per CLAUDE.md rule 12) — it is not a fifth work type. A `bug` fix can be breaking, and so can a `prd`. Keeping `breaking` out of this vocabulary avoids forcing a wrong classification on the first breaking bugfix: a `.breaking.md` fragment can pair with either a `fix/` or a `feat/` branch, and the work type is still `bug` or `prd` respectively.

## Conflicts worth recording explicitly

**`documentation` is an area label wearing a type label's name.** `.github/labeler.yml` auto-applies the `documentation` label on `pull_request_target` to any PR touching `docs/**` or a root `*.md`. Because CLAUDE.md rule 11 and the `dot-ai-write-docs` skill mean most `prd`-type PRs update docs too, `documentation` gets applied to those PRs as well — so it cannot mean "this is doc work." It stays an area label. Trying to repurpose it as the `doc` work-type label is unwinnable and would produce a label that is silently wrong on most PRs that carry it — a live empty gate, not a fix.

**Upstream carries both `feature` and `enhancement`; this fork carries only `enhancement`.** On this fork, `enhancement` is the sole GitHub label for `prd`-type work, and a `feature` label is not to be created. This fork syncs from upstream and inherits its habits, so this is worth stating even though the fork has no `feature` label today.

**`/dot-ai-prds-get` is inert on this fork.** That skill fetches open issues carrying the `PRD` label. That label exists upstream (`#0052CC`) and does not exist on this fork, so the skill silently returns nothing here. Per this repo's no-destructive-migration stance, the fix is not to create a `PRD` label — that would duplicate `enhancement`. `enhancement` plus the `prds/` directory is this fork's answer for tracking PRD work, and `/dot-ai-prds-get` should be treated as inert here. This is itself a live example of the empty-gate failure mode this vocabulary exists to close.

**`pyproject.toml` declares a changelog filename and issue format it never uses.** `pyproject.toml` declares `filename = "docs/CHANGELOG.md"` and a `vfarcic` `issue_format`, but towncrier itself is never invoked — `scripts/assemble-changelog.sh` is. This is harmless. Note it; do not "fix" it.

## Where the authoritative type list lives

The list of changelog-fragment suffixes is not restated here a fifth time. `pyproject.toml`'s `[[tool.towncrier.type]]` entries are the source of truth — `scripts/assemble-changelog.sh` reads from it — so consult that file directly rather than trusting a copy of the list in this doc, which could drift the same way the pre-fork#340 surfaces did.
