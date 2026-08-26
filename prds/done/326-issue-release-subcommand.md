# PRD #326 — `issue release` subcommand

**Issue:** [prageethw/dot-agent-deck#326](https://github.com/prageethw/dot-agent-deck/issues/326)
**Priority:** Medium
**Status:** Done on PR [#582](https://github.com/prageethw/dot-agent-deck/pull/582) (branch `feat/326-issue-release-subcommand`), after two rounds of reviewer/auditor findings, both resolved. Round 1 found two issues, no blockers (a markdown-injection gap in `--reason`, and `release --force` alone being a lower-friction route to the same end state as `claim --takeover --confirm-stopped`); both fixed. Round 2 gave both reviewer and auditor a clean **mergeable** verdict. All CI green, `mergeable: MERGEABLE`, `mergeStateStatus: CLEAN`. Non-blocking follow-ups tracked in [#584](https://github.com/prageethw/dot-agent-deck/issues/584).

## Problem statement

`worker-agent-deck issue claim` (PRD fork#235) can claim an issue and transfer a claim (`--takeover --confirm-stopped`), but has no way to release one. A stale claim — an orchestration that stopped without a formal handoff — locks the issue against re-entry (by design; that's the whole point of the lock) until someone clears it by hand: a `gh issue comment` plus a `gh issue edit --remove-label`, with no validation of who's doing it or whether there was actually a claim to clear. The issue documents four such hand-written releases across three issues in one week, including a sitting where half of all currently-claimed issues turned out to be stale.

`--takeover --confirm-stopped` is not a substitute — it transfers the claim to the caller, which is wrong when the intent is "leave this unclaimed," not "I'll take it now."

## Decisions

| Question | Direction | Why |
|---|---|---|
| What's the command shape? | `worker-agent-deck issue release <n> --repo <owner/name> [--force] [--confirm-stopped] [--reason <text>]`, a new `IssueCmd::Release` variant alongside `Claim`. | Mirrors the existing `claim` subcommand's shape and conventions exactly, so the two are learnable together. |
| How does it decide whether to release? | A new pure `decide_release` function, structurally mirroring `decide_claim`: refuse on an unclaimed issue (nothing to release); release outright if the caller holds it; otherwise refuse unless overridden. | Reuses `resolve_caller_identity`/`read_current_claim` unchanged — no second notion of "who am I" or "what's the current state," the exact failure mode issue #286 (the companion `PreToolUse` hook work) also guards against. |
| What does overriding someone else's claim cost? | **Both** `--force` and `--confirm-stopped` — the identical two-flag requirement `claim --takeover --confirm-stopped` already has, for both the held-by-a-different-identity case and the identity-unknown (labelled, no claim comment) case. | Round 1 review found a single `--force` flag made `release` + a plain follow-up `claim` a *cheaper* route to the same end state `claim --takeover --confirm-stopped` reaches — routing around the deliberate two-step friction CLAUDE.md rule 23 documents ("an agent can't satisfy the override in the same breath it discovers the conflict"). Requiring both flags closes that gap by construction rather than by convention. |
| What gets recorded? | Every release posts a comment naming who released it and, for a forced release, who it was released from. An optional `--reason` is folded in, code-span-wrapped so it can never post a live `@mention` or an unintended `#N` cross-reference. | Symmetric with `claim`'s own provenance comment. The `--reason` sanitization was hardened during round 1 review after an adversarial-input pass found the unwrapped form rendered live markdown. |

## Known residual (not fixed here, tracked in #584)

Releasing an issue removes the `in-progress` label, so a follow-up `claim` on the now-unlabelled issue reads as a pristine first claim (`decide_claim`'s unlabelled branch always returns `takeover_from: None`) — the displacement fact survives only in the separate release comment, not in the subsequent claim comment. The two-flag friction fix above closes the *cost* asymmetry with `--takeover`; it does not merge the two comments into one provenance trail. Accepted as a documented consequence rather than folded into this PRD's scope.

**Fork-only?** No — the underlying `issue_claim`/`issue_dispatch` code is upstream code (PRD fork#235's own territory). Offer upstream per rule 19 once the fork has had a chance to exercise it.
