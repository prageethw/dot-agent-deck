# PRD #376: Devbox-native CI entrypoints and a Semaphore pipeline spike

**Status**: Not started
**Priority**: Medium
**Created**: 2026-08-04

## Problem Statement

Two problems, one of which is a live bug.

**The toolchain is pinned locally and floating in CI.** `devbox.json` pins `rustc@1.97.0`, `cargo@1.97.0`, `clippy@1.97.0`, `rustfmt@1.97.0` and `cargo-nextest@0.9.140`. `.github/workflows/ci.yml:33` uses `dtolnay/rust-toolchain@stable` and `:37` installs nextest via `taiki-e/install-action@nextest` — both resolve to whatever is current on the day the job runs. So the local gate (`cargo test-fast`, aliased in `.cargo/config.toml`) and CI compile with different toolchains, and the divergence widens silently until a new release introduces a lint or a behaviour change. The first symptom will be a clippy failure that cannot be reproduced locally.

That is the same class of problem the `ci.yml:41-58` comment documents from 2026-07-30, where `cargo test` and `cargo nextest run` produced different flake behaviour and the fix was to align the runners so that *"green locally means the same thing as green in CI."* The runner was aligned; the toolchain version was not.

**There is no pipeline abstraction to reuse.** `Taskfile.yml` covers docs, demo reels, checksums, homebrew and scoop — release and packaging automation. It has no `build`, `test`, `lint` or `ci` task. The CI steps exist only as raw `cargo` invocations inside `ci.yml`, so nothing outside GitHub Actions can run them, including an agent working locally.

Separately, we want to evaluate Semaphore as a second CI provider (including its agentic `sem-ai` tooling) for a comparison. That evaluation needs the build steps to be invokable outside GHA anyway, because Semaphore has no equivalent of the marketplace actions `ci.yml` depends on. The toolchain has to be provisioned by hand there regardless — and `devbox.json` already pins exactly the right set, already under Renovate management (`renovate.json`, `matchManagers: ["devbox"]`, automerged for patch/minor).

So the second provider is the forcing function, but the pinning fix is worth landing on its own.

## Solution Overview

Introduce `task`-level CI entrypoints that wrap the existing cargo invocations, and provision the toolchain from `devbox.json` rather than from marketplace actions. Stand the result up as a new Semaphore pipeline covering the three jobs Semaphore Cloud can host, measure it against the GHA baseline, and use those numbers to decide whether to retrofit GHA.

Deliberately **do not** touch `ci.yml` or `release.yml` in this PRD. They are the working gate that Renovate automerges against, and they are the experimental control for the measurement. Editing them in the same change makes the comparison meaningless and puts a green pipeline at risk for no benefit.

## Scope

### In Scope

- New `task` entrypoints wrapping the six commands `ci.yml` runs: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build --release`, `cargo nextest run`, `cargo xtask linkage-check` (`ci.yml:38-68`) and `cargo audit` (`ci.yml:138`).
- `cargo-audit` added to `devbox.json`. `ci.yml:137` currently runs `cargo install cargo-audit --locked`, which compiles from source on every run and is almost certainly the slowest single step in CI. This is an unambiguous win independent of everything else in this PRD.
- `.semaphore/semaphore.yml` covering the three jobs Semaphore Cloud can host: `build` (Linux), `build-macos`, `security`.
- Measurement against the GHA baseline: cold nix bootstrap, warm bootstrap, `target/` cache hit rate, per-job and total wall clock.
- A recorded decision on whether to retrofit `ci.yml`, backed by those numbers.

### Out of Scope

- **Any edit to `ci.yml` or `release.yml`.** `git diff` against both must be empty when this PRD closes. Retrofitting GHA is a follow-up, gated on M5's numbers.
- **Windows.** Nix does not run natively on Windows — `ci.yml:72` already records this (*"the devbox/nix toolchain used locally is Linux-only"*) — and Semaphore Cloud has no hosted Windows runner, only self-hosted agents. `build-windows` stays on GHA with rustup. Two toolchain-provisioning paths is the accepted end state, not a gap to close later.
- **Publishing of any kind.** Publishing is confined to `release.yml` (triggered by `push: tags: ['v*']`) and `docs-publish.yml` (`workflow_call` only). `ci.yml` has no side effects, so a `ci.yml`-shaped Semaphore pipeline has nothing to double-publish. No publish flags, kill switches or conditionals are introduced anywhere — adding one to `release.yml` would create a silent-non-publish failure mode strictly worse than the double-publish it prevents.
- **Making Semaphore a required status check.** Renovate automerges cargo patch and ≥1.0 minor bumps on green CI (`renovate.json`). An unproven pipeline does not go in that path.
- Semaphore promotions and deployment targets. Interesting for a later comparison, but they imply release-shaped behaviour, which is out of scope here.

## Technical Approach

### Task entrypoints

Thin wrappers, one per existing CI step, so the mapping to `ci.yml` stays auditable and a diverging step is visible as a diff rather than as a behaviour change. No consolidation into a single `task ci` that hides which step failed — the four-job split in `ci.yml` exists so a platform-specific break is visible independently (`ci.yml:70-82`, `:103-114`), and the entrypoints should preserve that granularity.

### Caching is the part that does not abstract

This is the main technical risk and the reason this is a spike rather than a refactor.

`Swatinem/rust-cache@v2` (`ci.yml:36`) is not a thin wrapper. It derives keys from `Cargo.lock` plus the rustc version, selectively saves `~/.cargo` registry and git db plus `target/`, and prunes stale and incremental artifacts to keep the archive small. Replacing it means owning that logic.

Worse, the cache *backend* is irreducibly provider-specific: GHA uses the `actions/cache` REST API, Semaphore uses its `cache store` / `cache restore` CLI, and locally there is no cache because `target/` is already warm. No script abstracts that away — the pipeline will be provider-agnostic where it costs nothing (invoking cargo) and provider-coupled exactly where CI time is won or lost.

And the nix store itself now needs caching on both providers, which is a second cache problem that does not exist today.

### Nix bootstrap cost

Devbox in CI needs nix installed and packages fetched. Cold, that is minutes. The conventional fix is `jetify-com/devbox-install-action`, which is a marketplace action and therefore unavailable on Semaphore and self-defeating on GHA. Bootstrap plus store caching has to be hand-rolled per provider. The honest expectation is that the first version is **slower** than the current `rust-cache`-based jobs; M5 exists to find out by how much.

### Machine mapping

| GHA job | Semaphore equivalent |
|---|---|
| `build` (`ubuntu-latest`) | `f1-standard-4` — 4 vCPU / 16 GB, Ubuntu 24.04 |
| `build-macos` (`macos-latest`) | `a2-standard-4` — Apple Silicon M2, 4 vCPU / 8 GB, Xcode16 or Xcode26 |
| `security` (`ubuntu-latest`) | `f1-standard-4` |
| `build-windows` | none — out of scope |

Note `a2-standard-4` has 8 GB against `ubuntu-latest`'s 16 GB, and it runs `cargo build --release` plus the full `nextest` tier. Memory pressure is a live possibility.

### Cost

`vfarcic/dot-agent-deck` is public, so GHA is free for it. Semaphore's `f1` is $0.0075/min, so this is added cost, not saved cost — roughly $0.35–0.40 per full run. The evaluation must be argued on wall clock, cache behaviour and agent experience, not price.

### Renovate

No regression. `renovate.json` already has a `matchManagers: ["devbox"]` rule grouped as "Devbox packages" with automerge for digest/pin/patch/minor, so moving toolchain versions from GHA action refs into `devbox.json` keeps them bot-managed. The `github-actions` manager rule simply has less to do.

## Success Criteria

- Each `ci.yml` step has a `task` entrypoint that runs identically on a local devbox shell and on Semaphore.
- `cargo-audit` comes from `devbox.json`; nothing compiles a CI tool from source.
- The Semaphore pipeline is green for `build`, `build-macos` and `security` on a PR and on a push to `main`.
- Measured and recorded: cold bootstrap, warm bootstrap, `target/` cache hit rate, per-job and total wall clock, each against the current GHA baseline.
- `git diff` against `ci.yml` and `release.yml` is empty.
- A written decision on retrofitting GHA, citing the numbers — including "no" as an acceptable outcome.

## Milestones

- [ ] **M1 — Task entrypoints and `cargo-audit` in devbox.** All six steps runnable locally through `task`; `cargo-audit` resolved from `devbox.json`. Landable and useful on its own even if every later milestone is abandoned.
- [ ] **M2 — Darwin toolchain gate.** Confirm `devbox` resolves `rustc@1.97.0` and the rest for `aarch64-darwin` from a binary cache rather than building from source. **If it builds Rust from source, stop and reconsider** — the macOS job is not viable and the scope shrinks to Linux only.
- [ ] **M3 — Semaphore Linux green.** `build` and `security` passing, with `target/` and nix-store caching in place.
- [ ] **M4 — Semaphore macOS green.** `build-macos` passing on `a2-standard-4`, memory headroom confirmed.
- [ ] **M5 — Measurements recorded.** The full comparison table against the GHA baseline, written down where the retrofit decision will be made.
- [ ] **M6 — Decision and docs.** Retrofit-or-not recorded with rationale; `docs/develop/` note covering the task entrypoints and how to run a CI step locally.

## Risks

- **The abstraction misses the part that matters.** Caching is where CI time lives and it is exactly what cannot be made provider-agnostic. The pipeline will still carry `if GITHUB_ACTIONS / elif SEMAPHORE` branches in its cache steps. If that is unacceptable, the premise of the PRD is weaker than it looks.
- **CI gets slower.** `rust-cache` is well tuned; a hand-rolled equivalent plus a nix bootstrap plausibly loses to it initially. M5 is the check, and a slower result is a legitimate reason to close this PRD without retrofitting GHA.
- **macOS binary cache miss.** nixpkgs darwin cache hit rates are worse than Linux. Building the Rust toolchain from source on `a2-standard-4` would make the macOS job unusable. M2 gates this deliberately.
- **Scope creep into `ci.yml`.** The single largest risk for a session that does not have the originating discussion. `ci.yml` and `release.yml` are off limits: they are the control for the measurement, `release.yml` touches real users' `brew upgrade` path through `vfarcic/homebrew-tap` and `vfarcic/scoop-bucket`, and Renovate automerges against `ci.yml` being green.
- **Confounded comparison.** Changing GHA and standing up Semaphore at the same time means a slow Semaphore job cannot be attributed — machine, nix bootstrap or hand-rolled cache. This is the concrete reason for the out-of-scope rule above, not tidiness.
- **Two toolchain paths forever.** Windows keeps rustup and marketplace actions, so the pinning fix does not reach it. Given `portable-pty` is held at `=0.8.1` for a Windows ConPTY reason (`renovate.json`, `Cargo.toml`), Windows is where the load-bearing bugs live — and it is the platform this change cannot help.

## Open Questions

1. **Does `devbox` resolve the pinned Rust toolchain for `aarch64-darwin` from a binary cache?** Decides whether M4 exists at all. Check before writing any macOS pipeline.
2. **How much of `Swatinem/rust-cache`'s behaviour has to be reimplemented?** Key derivation is easy; the pruning of stale and incremental artifacts is what keeps the archive small enough to be worth restoring. Restoring a bloated `target/` can be slower than a cold build.
3. **Do the task entrypoints eventually become GHA's interface too, or stay Semaphore-only?** This is the M6 decision. Staying Semaphore-only leaves the version-skew bug unfixed on the platform that actually gates merges, which would be an odd place to stop.
4. **Semaphore pins 1.97.0 while GHA floats on `stable` — does that confound the comparison?** A red Semaphore job could be a genuine 1.97-vs-current difference rather than a Semaphore problem. Worth deciding up front whether to pin GHA temporarily for the measurement window, which is the one edit to `ci.yml` that might be justified.
5. **Is the `r1` native-ARM runner worth a separate job?** `release.yml` cross-compiles `aarch64-unknown-linux-gnu` through `cross` (Docker) and `aarch64-crossbuild-check.yml` guards it; Semaphore's `r1` machines are native ARM, which could remove the cross machinery. But `r1` has no Docker support, so anything container-shaped fails there. Out of scope for this PRD, potentially its own.
6. **Does `cargo xtask linkage-check` need anything not already in `devbox.json`?** It builds the `xtask-linkage-check` package, so probably not, but confirm rather than assume.

## Work Log

### 2026-08-04 — Created

Came out of a Semaphore evaluation discussion. Three decisions worth preserving, because they are the parts a fresh session is most likely to undo:

1. **The order is inverted on purpose.** The obvious plan is "make GHA provider-agnostic, then porting is trivial." Rejected: marketplace actions do not exist on Semaphore, so the toolchain has to be hand-provisioned there regardless. Writing the devbox layer on the Semaphore side first means no extra work, keeps GHA as an unmodified control, and produces the numbers that justify (or kill) the retrofit before touching a working gate.
2. **The version-skew fix is the real motivation**, not portability. Portability of the invocation layer is cheap and mostly cosmetic; caching stays provider-specific either way. `devbox.json` pinning vs `rust-toolchain@stable` floating is an actual latent bug.
3. **No publish conditionals anywhere.** `ci.yml` has no side effects and `release.yml` is out of scope, so the double-publish problem does not arise at this scope. If a full release pipeline is ever demoed on Semaphore, do it by pointing at **separate destinations** — the existing `NAME=dot-agent-deck-beta` channel in `Taskfile.yml`, a throwaway tap, a `:semaphore-test` image tag — rather than by flagging the real one. A flag can be misconfigured; a different destination structurally cannot collide. Note also that `release.yml`'s `concurrency: group: release` gives zero protection against a second CI provider, since concurrency domains do not span systems.
