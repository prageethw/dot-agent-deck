# Upgrading a running deck — why it is not a `brew upgrade`

Merging a fix does not deploy it. **A running deck keeps using the binary it was launched from, and its installed hooks keep calling the binary that installed them**, so a fix can sit on `main` for a long time while the deck you are actually using still has the bug.

This is not a bug in itself — every resolution below is deliberate and has a reason recorded at its call site. But the consequences are not obvious, and rediscovering them costs real time. This page is the record.

## The naming convention: a fork build is installed as `worker-agent-deck`

**A fork build is installed under the filename `worker-agent-deck`. The name `dot-agent-deck` is reserved for the upstream Homebrew install.**

This is not cosmetic. `vfarcic/tap` can never ship a fork build (see the last section), so an upstream brew binary and a hand-installed fork build coexist on the same `PATH` indefinitely — and the filename is the only thing that tells them apart at a glance. `--version` does not: both print a bare SemVer, and the fork's tags run ahead of upstream's, so a higher number is not evidence of which lineage you are looking at. A fork build installed as `dot-agent-deck` is therefore a defect rather than a preference — it is indistinguishable from the upstream binary while behaving differently.

| Path | What it should be |
|---|---|
| `~/.local/bin/worker-agent-deck` | the fork build you launch; also the absolute path baked into the `~/.claude/settings.json` hook entries |
| `/opt/homebrew/bin/dot-agent-deck` | the upstream brew install, moved only by `brew upgrade` |

### Installing a fork build

`cargo install --path .` is the **wrong** command here: it installs under the cargo target name, which is the reserved one. Build and copy explicitly. Build from a checkout **at the tag** you want — `build.rs` resolves the version as `DAD_VERSION` → git tag → `CARGO_PKG_VERSION`, so a detached checkout at the tag is what makes the binary report that version rather than the `Cargo.toml` placeholder:

```bash
git worktree add --detach ../dot-agent-deck-vX.Y.Z vX.Y.Z
cd ../dot-agent-deck-vX.Y.Z
cargo build --release --locked
rm -f ~/.local/bin/worker-agent-deck                      # see below — do NOT skip
cp target/release/dot-agent-deck ~/.local/bin/worker-agent-deck
cd - && git worktree remove ../dot-agent-deck-vX.Y.Z
```

**The `rm -f` is load-bearing on macOS, and omitting it fails in a way that looks like a broken build.** `cp` over an existing binary rewrites it in place, which invalidates the ad-hoc code signature the linker applied; on Apple Silicon the kernel then `SIGKILL`s it on every exec. The symptom is `--version` printing **nothing at all** and exiting **137**, with no error message and no hint that signing is involved — a binary that ran perfectly from `target/release/` seconds earlier. Removing the destination first means `cp` creates a fresh inode and the signature survives. (Observed installing v0.37.1, 2026-08-11.) If you hit it anyway, `codesign --force -s - ~/.local/bin/worker-agent-deck` re-signs in place.

Build in a **throwaway worktree on a disk-backed sibling path**, never in the root checkout (CLAUDE.md rule 1 — it is what the running daemon and panes are attached to, and it routinely carries an uncommitted `.dot-agent-deck.toml`) and never on the scratchpad or a tmpfs (rule 18 — a `--features e2e` `target/` is several GB).

## Three things resolve the binary, three different ways

| What | How it resolves | Consequence |
|---|---|---|
| **The daemon** | `std::env::current_exe()`, explicitly **not** `$PATH` (`src/daemon_attach.rs:247`) | It becomes whatever binary the TUI that spawned it was |
| **`delegate` / `work-done`** | Agents invoke them **by name**, so `$PATH` decides | Governed by `.dot-agent-deck.toml`'s role commands, independent of the above |
| **Installed hooks** | An **absolute path baked in at install time**, also from `current_exe()` (`src/hooks_manage.rs:193`, `:230`; `src/codex_hooks_manage.rs:438`) | A hook keeps calling whatever binary installed it until `hooks install` is re-run |

The `current_exe()` choice is deliberate and should not be "fixed" to use `$PATH`. The doc comment at `daemon_attach.rs:247` records why: *"non-interactive ssh shells routinely skip `~/.local/bin` (we hit this exact bug three times — commits `493248b`, `bbf2236`, `ea8c748`)."*

**Open caveat, where row 2 meets the naming convention.** The generated orchestrator context instructs workers to run `dot-agent-deck delegate …` / `dot-agent-deck work-done …`, resolved **by name** from `$PATH`. Under the convention above, the only `dot-agent-deck` on `PATH` is the *upstream* brew build — so those calls do not necessarily run the fork binary you just installed, and a fork-only change to `delegate` can be live in your TUI while the agents keep calling something else entirely. This is unresolved, not decided. Before trusting a `delegate`/`work-done` fix to be deployed, check what it actually resolves to **inside the agent's own shell** (the role commands run under `devbox`, whose `PATH` is not necessarily yours): `command -v dot-agent-deck`.

## The trap: attaching is not spawning

The daemon is **lazy-spawn-on-attach** — `ensure_daemon_running` probes the attach socket and only spawns when it is unreachable.

So if you launch a new TUI while an old daemon is still alive, **it attaches to the old daemon**. No error, no warning, no change in behaviour. It looks exactly like the upgrade did not work, because functionally it did not.

There is also **no launchd job** — no plist, nothing in `launchctl list`. The daemon does not self-respawn; it comes back from whichever TUI you next launch. That is what makes the sequence below work at all.

## The sequence that actually upgrades it

```bash
# 1. Quit every TUI, then stop the daemon. THIS MUST COME FIRST —
#    otherwise step 2 just attaches to the old one.
#    Match on 'daemon serve' alone: the daemon is argv[0]-named after whichever
#    binary spawned it (current_exe), so a pattern containing 'dot-agent-deck'
#    silently fails to match a daemon spawned by 'worker-agent-deck'.
pkill -f 'daemon serve'

# 2. Relaunch from the binary you want to be running.
worker-agent-deck

# 3. Verify you got what you expected.
pgrep -fl 'daemon serve'              # must name the new binary, not the old path
worker-agent-deck daemon status       # roster — confirms the new build is live
worker-agent-deck --version           # the version you just installed
```

If hooks were installed by an older binary and you want them pointing at the new one, re-run `hooks install` **from the new binary** — the path is baked at install time, so nothing else updates it. Reinstalling under the *same* filename does not need this: the baked path is unchanged, so the existing hook entries keep working and simply pick up the new build.

## What it costs

Every pane dies. Worktrees, branches and pushed commits all survive — only in-flight agent context is lost, so any interrupted worker needs re-delegating. With several concurrent orchestrations that can be a dozen panes at once, which is why the upgrade tends to get deferred, which is why fixes sit undeployed.

**That deferral is the real hazard.** A merged fix for a bug you are actively hitting is worth nothing until the daemon restarts, and the symptom looks identical before and after the merge.

## Fork releases never reach Homebrew

`release.yml` gates the Homebrew and Scoop publish steps on `github.repository == 'vfarcic/dot-agent-deck'` (`:382`, `:388`, and the matching Scoop steps). The comment above them explains why: those steps clone `vfarcic/homebrew-tap` and `vfarcic/scoop-bucket` and bake `github.com/vfarcic/dot-agent-deck/releases/download/…` URLs into artifacts only they consume. *"None of that can or should succeed from a fork"* — and on the fork's first tag-triggered release (v0.35.8) `Publish Homebrew formula` failed on a missing `HOMEBREW_TAP_TOKEN`, which failed `finalize`, which skipped the rest.

So **`vfarcic/tap` can never ship a fork build**. A fork release produces GitHub assets only, and `brew upgrade` will only ever move you between upstream builds. Installing a fork build means fetching the asset (or building it) and putting it on `PATH` yourself.

This is also why a fork build and a brew build can coexist and be confused for one another: `/opt/homebrew/bin/dot-agent-deck` stays installed and becomes active again the moment anything earlier on `PATH` is removed. **The naming convention at the top of this page is the answer to that** — keeping fork builds under `worker-agent-deck` means the two can never shadow each other by accident, and `which` alone tells you which lineage you are about to run.

It also means a stray fork build installed as `~/.local/bin/dot-agent-deck` is worse than a duplicate: it shadows the brew binary while wearing its name, so `dot-agent-deck --version` reports a fork version that no `brew upgrade` will ever move. If you find one, that is what happened.
