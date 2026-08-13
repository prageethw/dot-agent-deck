# Upgrading a running deck — why it is not a `brew upgrade`

Merging a fix does not deploy it. **A running deck keeps using the binary it was launched from, and its installed hooks keep calling the binary that installed them**, so a fix can sit on `main` for a long time while the deck you are actually using still has the bug.

This is not a bug in itself — every resolution below is deliberate and has a reason recorded at its call site. But the consequences are not obvious, and rediscovering them costs real time. This page is the record.

## The naming convention: a fork build is installed as `worker-agent-deck`

**A fork build is installed under the filename `worker-agent-deck`. The name `dot-agent-deck` is reserved for the upstream Homebrew install.**

This is not cosmetic. `vfarcic/tap` can never ship a fork build (see the last section), so an upstream brew binary and a hand-installed fork build coexist on the same `PATH` indefinitely — and the filename is the only thing that tells them apart at a glance. `--version` does not help by itself: it prints `dot-agent-deck <semver>` regardless of lineage (the binary's `clap` name is unchanged — see the crate-name row in CLAUDE.md rule 21), so the fork build installed as `worker-agent-deck` still self-identifies as `dot-agent-deck` when asked, and the fork's tags run ahead of upstream's, so a higher number is not evidence of which lineage you are looking at either. `dot-agent-deck daemon hello` is the reliable discriminator instead — see the version inventory below. A fork build installed as `dot-agent-deck` is therefore a defect rather than a preference — it is indistinguishable from the upstream binary while behaving differently.

| Path | What it should be |
|---|---|
| `~/.local/bin/worker-agent-deck` | the fork build you launch; also the absolute path `hooks install` bakes into `~/.claude/settings.json`'s hook entries — only if it was the binary that last ran `hooks install`. See "Adopting this page's naming convention" below: hook entries don't self-correct, and a stale one from an older install can sit alongside the current one indefinitely |
| `~/.local/bin/dot-agent-deck` | a **symlink** to `worker-agent-deck` — never a second build. This is a deliberate, interim exception to the reservation below, not a second violation of it: agents invoke `dot-agent-deck` by name, and the symlink is what keeps those calls on the fork binary. See "Row 2 collides with the naming convention" below |
| `/opt/homebrew/bin/dot-agent-deck` | the upstream brew install, moved only by `brew upgrade` |

Verify which lineage a binary actually is with `dot-agent-deck daemon hello` (a static print — it does not spawn a daemon, `src/main.rs:350-358`), not `--version`. Read **`build_version`**, which pins the exact commit: that is the field that still answers the question. This is the per-binary version inventory CLAUDE.md rule 21 promises this page carries — it drifts as binaries are rebuilt, so treat any specific snapshot as a re-measure prompt rather than a fact to trust. Measured 2026-08-13:

```
/opt/homebrew/bin/dot-agent-deck daemon hello
  {"ok":true,"server_version":7,"build_version":"0.36.0-g6a98629","daemon_version":"0.36.0"}
~/.local/bin/worker-agent-deck daemon hello
  {"ok":true,"server_version":7,"build_version":"0.37.2-g5ffe2351-dirty","daemon_version":"0.37.2"}
```

(The `-dirty` suffix on the fork build means it was built from a working tree with uncommitted changes, not a clean release build — worth noting since it can otherwise look like an ordinary release version.)

**`server_version` no longer separates the lineages, and this page previously said it did.** It read "6 is upstream, 7 is fork" — true when measured 2026-08-11, false the next day: upstream's v0.36.0 raised its own `PROTOCOL_VERSION` to 7, brew upgraded to it on 2026-08-12, and both lineages now answer 7. Anything derived from that split is void with it — in particular the local-attach guard (`ensure_compatible_daemon_or_die`) no longer refuses a cross-lineage attach here, so a mismatched *attach* has gone as quiet as `delegate`/`work-done` already were. `build_version`'s short SHA is the discriminator that survives: resolve it against each lineage's history (`git merge-base --is-ancestor <sha> upstream/main`) rather than trusting the number in front of it.

**Nor can "the fork is the higher number" be relied on in either direction.** The inventory above has the fork build reporting `0.37.2` against brew's `0.36.0` — the higher number, as one would naively expect — but that is not a stable property: `git describe` resolves against whichever tag happens to be reachable from `origin/main`, and every sync rewrites that (see [`versioning.md`](versioning.md) § "The fork's own tags are orphaned by every sync"). The same machine has shown the fork build reading *lower* than brew's on other days, when its newest release tag had just been orphaned by a sync and `describe` fell back to an older upstream tag. So a version comparison between the two names tells you nothing about lineage either way, in either direction — re-measure with `daemon hello` and resolve `build_version`'s SHA rather than trusting whichever number happens to be bigger today.

**All of this — the shadowing, the symlink, `which` reporting the fork build — depends on `~/.local/bin` coming before `/opt/homebrew/bin` on `$PATH`.** Confirm it, don't assume it: `echo $PATH | tr ':' '\n' | grep -n 'local/bin\|homebrew/bin'` — `~/.local/bin` must have the lower line number. If Homebrew comes first instead, the brew binary wins every lookup, the symlink below does nothing, and every "shadows" claim on this page inverts.

### Installing a fork build

**Stop the daemon before you replace the binary you are about to overwrite — see "The sequence that actually upgrades it" below.** The order matters (next paragraph); working through this section top-to-bottom without stopping the daemon first is the single most common way to hit the SIGKILL described below.

`cargo install --path .` is the **wrong** command here: it installs under the cargo target name, which is the reserved one. Build and copy explicitly. Build from a checkout **at the tag** you want — `build.rs` resolves the version as `DAD_VERSION` → git tag → `CARGO_PKG_VERSION`, so a detached checkout at the tag is what makes the binary report that version rather than the `Cargo.toml` placeholder. Find the tag first (`git tag --sort=-v:refname | head -5`, or `gh release list --repo prageethw/dot-agent-deck --limit 5`), then run the whole block from the root of a clone you already have — never the root checkout (CLAUDE.md rule 1) — since `../dot-agent-deck-vX.Y.Z` and the trailing `cd -` both resolve against that starting directory:

```bash
mkdir -p ~/.local/bin   # first install only; harmless if it already exists
git worktree add --detach ../dot-agent-deck-vX.Y.Z vX.Y.Z
cd ../dot-agent-deck-vX.Y.Z
cargo build --release --locked
cp target/release/dot-agent-deck ~/.local/bin/.worker-agent-deck.new
mv -f ~/.local/bin/.worker-agent-deck.new ~/.local/bin/worker-agent-deck   # atomic — see below
cd - && git worktree remove ../dot-agent-deck-vX.Y.Z
```

**Install via a temp-file-then-rename, not `cp` straight onto the live path — it is load-bearing on macOS, not a style preference.** `cp` that rewrites a file **in place** invalidates the ad-hoc code signature the linker applied, and on Apple Silicon the kernel `SIGKILL`s the next exec — but only while some process is still running from that inode (tested directly: overwriting an idle binary works fine, rc=0; overwriting one a process is actively executing from produces exit **137** with no message, first try; re-signing with `codesign --force -s -` recovers it). Whether the daemon happens to still be running from the file you're overwriting is exactly the variable the ordering above exists to control, and it is easy to get wrong from a cold read of this section alone. `cp` to a new name then `mv -f` sidesteps the question entirely: `mv` within one filesystem is a rename, so the destination path is never briefly absent, the replacement file gets a **fresh inode** (no in-place rewrite ever happens, so there is nothing for the kernel to invalidate), and the previous binary — and the `~/.local/bin/dot-agent-deck` symlink pointing at it — stay valid right up until the rename completes. That also means a failed `cp` (full disk, typo'd path) leaves the old binary intact under **both** names, instead of the old `rm -f`-then-`cp` sequence, which deleted the working binary first and hoped the copy would succeed. If you hit the 137 anyway, `codesign --force -s - ~/.local/bin/worker-agent-deck` re-signs in place. (Observed installing v0.37.1, 2026-08-11.)

Build in a **throwaway worktree on a disk-backed sibling path**, never in the root checkout (CLAUDE.md rule 1 — it is what the running daemon and panes are attached to, and it routinely carries an uncommitted `.dot-agent-deck.toml`) and never on the scratchpad or a tmpfs (rule 18 — a `--features e2e` `target/` is several GB). `git worktree remove` (last line above) deletes that worktree's `target/` too, so each install from a fresh worktree is a full rebuild — reuse a persistent build worktree if you install often.

## Three things resolve the binary, three different ways

| What | How it resolves | Consequence |
|---|---|---|
| **The daemon** | `std::env::current_exe()`, explicitly **not** `$PATH` (`src/daemon_attach.rs:247`) | It becomes whatever binary the TUI that spawned it was |
| **`delegate` / `work-done`** | Agents invoke them **by name**, so `$PATH` decides | Governed by `.dot-agent-deck.toml`'s role commands, independent of the above |
| **Installed hooks** | An **absolute path baked in at install time**, also from `current_exe()` (`src/hooks_manage.rs:193`, `:230`; `src/codex_hooks_manage.rs:438`) | A hook keeps calling whatever binary installed it until `hooks install` is re-run |

The `current_exe()` choice is deliberate and should not be "fixed" to use `$PATH`. The doc comment at `daemon_attach.rs:247` records why: *"non-interactive ssh shells routinely skip `~/.local/bin` (we hit this exact bug three times — commits `493248b`, `bbf2236`, `ea8c748`)."*

**Row 2 collides with the naming convention, and the collision is not theoretical.** The command name `dot-agent-deck` is **hardcoded in the product**, and in more than one place: `src/state.rs` (`work_done_footer`, `state.rs:876`, `:896`) generates the `dot-agent-deck work-done …` text, but every `dot-agent-deck delegate …` string comes from **`src/ui.rs`** instead — the orchestrator-context builder (`ui.rs:2938`, `:2961`, `:2978`, `:2979`, `:2983`, plus a further `work-done --done` mention at `:2987`/`:2989`/`:3014`). The name is also hardcoded outside `src/` entirely, in `.dot-agent-deck.toml`'s role `prompt_template`s (e.g. the `coder` role), and in `docs/orchestration.md`, `docs/troubleshooting.md` and `docs/develop/pi-extension.md`. All of it resolves **by name** from `$PATH`. So the convention cannot simply be applied to that name and left there — if the only `dot-agent-deck` on `PATH` is the upstream brew build, every one of those calls runs it, and fixing only `state.rs` — the natural first guess — leaves every `delegate` call still resolving to it.

That does not fail loudly, and believing it would is the trap. Measured 2026-08-11: brew's upstream **0.35.10 speaks `PROTOCOL_VERSION` 6** while the fork's 0.37.1 speaks **7**, and since v0.36.0 a local *attach* on a mismatched protocol is refused outright (`ensure_compatible_daemon_or_die`, `src/main.rs:1409`) — but that guard has exactly one call site, the TUI attach path. `delegate` and `work-done` never reach it: they write to the **unversioned hook socket** (`hook::request_from_socket_with_deadline` / `hook::send_to_socket`), which carries no version field, and which the daemon injects into every spawned pane via `DOT_AGENT_DECK_SOCKET` regardless of which binary is talking to it. Deleting a fork build that happened to be installed under the reserved name does **not** hand delegation a loud refusal — it hands it a mismatched binary that runs to completion and **exits 0**: `work-done` is fire-and-forget and prints nothing, and the daemon drops any frame it cannot parse with no log at all (`src/daemon.rs:1530`); `delegate` either warns that the reply couldn't be parsed and proceeds, or times out waiting for one and proceeds — neither path fails the process. A worker who tests this by removing the symlink and running `work-done` sees exit 0 and concludes the setup is fine; it is not — the signal reached a socket and vanished. Keep the symlink for the reasons that actually hold: it removes the drift hazard of two files that must be upgraded in lockstep and silently diverge the moment one isn't, and it leaves exactly one file to replace instead of two. The name being load-bearing is real — the danger of removing it is silence, not a loud error.

**Resolution, until the hardcoded name is fixed at the source:** make `~/.local/bin/dot-agent-deck` a **symlink to `worker-agent-deck`**, not a second copy of the build.

```bash
test -x ~/.local/bin/worker-agent-deck || { echo "install worker-agent-deck first"; exit 1; }
ln -sfn worker-agent-deck ~/.local/bin/dot-agent-deck
ls -l ~/.local/bin/dot-agent-deck && command -v dot-agent-deck && dot-agent-deck daemon hello
```

A symlink keeps both names on one fork binary at one protocol version, and stays correct across every future upgrade because there is only ever one file to replace — which a second copy does not, since it silently goes stale the moment you upgrade the other. `ln -sfn` replaces the link atomically, unlike `rm -f` then `ln -s` (no window where the name is briefly absent), and the `test -x` guard stops you creating a **dangling** link: `ln -s` does not check its target, and PATH resolution silently skips an unexecutable or missing target and falls through to the next `dot-agent-deck` on `PATH` — `/opt/homebrew/bin/dot-agent-deck`, the protocol-mismatched brew binary — which, per "Row 2 collides" above, then fails *silently*, not loudly. The final `ls -l` / `command -v` / `daemon hello` line is the check that catches that failure mode; do not skip it.

It is still an interim measure: it shadows the brew binary, so `dot-agent-deck` no longer means "upstream" on this machine. The end state is for the generated prompts to name the fork binary directly.

One caveat worth knowing rather than acting on: both the daemon (`src/daemon_attach.rs:247`) and hook installation (`src/hooks_manage.rs:193`, `:230`) bake `std::env::current_exe()`. Whether Rust's `current_exe()` resolves a symlink on macOS has not been checked here — if a TUI is ever launched via the `dot-agent-deck` name rather than `worker-agent-deck`, whichever path it yields would get baked into `~/.claude/settings.json`, which would undercut "there is only ever one file to replace" above. Launch via `worker-agent-deck` to stay on the known-good side of this until it's checked.

When diagnosing, check what the name resolves to **inside the agent's own shell** rather than yours — role commands run under `devbox`, whose `PATH` need not match: `command -v dot-agent-deck`.

## The trap: attaching is not spawning

The daemon is **lazy-spawn-on-attach** — `ensure_daemon_running` probes the attach socket and only spawns when it is unreachable.

So if you launch a new TUI while an old daemon is still alive, **it attaches to the old daemon**. No error, no warning, no change in behaviour. It looks exactly like the upgrade did not work, because functionally it did not.

There is also **no launchd job** — no plist, nothing in `launchctl list`. The daemon does not self-respawn; it comes back from whichever TUI you next launch. That is what makes the sequence below work at all.

## The sequence that actually upgrades it

Installing the binary (above) and stopping/relaunching the daemon (below) are two independent steps, and neither leaves you stranded waiting on the other: a running daemon holds its own inode, so the `mv`-based install above is safe to run at any point relative to this section, and `daemon stop` works across protocol versions no matter which binary built the daemon — it never handshakes; it connects, reads `peer_pid`, issues `ListAgents` (with a forward-compat fallback for the field name), and SIGTERMs by pid (`src/daemon_stop.rs:130-215`). The one real ordering constraint is the one called out in the install section above: don't relaunch (step 2 below) before you stop (step 1) — that just reattaches to the old daemon.

```bash
# 1. Quit every TUI, then stop the daemon. THIS MUST COME FIRST —
#    otherwise step 2 just attaches to the old one.
#    Run it bare first — it refuses and names any live agents rather than
#    silently killing them, and that roster is the only place you see it:
worker-agent-deck daemon stop
#    Only on refusal, once you've seen and accepted what's running, add
#    --force — it escalates to SIGTERM-then-SIGKILL rather than asking again:
worker-agent-deck daemon stop --force

# 2. Relaunch from the binary you want to be running.
worker-agent-deck

# 3. Verify you got what you expected.
ps -Ao pid,command | grep 'daemon serve' | grep -v grep   # NOT pgrep — see below
worker-agent-deck daemon status       # roster — confirms the new build is live
worker-agent-deck --version           # confirms the filename only — always prints "dot-agent-deck <ver>", see above
worker-agent-deck daemon hello        # the reliable discriminator — server_version 6 vs 7, see the inventory above
```

**Use `daemon stop`, not `pkill` — and do not verify with `pgrep`.** `daemon stop` is the supported path (PRD #103 Phase 3, "documented alternative to `kill -9` after upgrading the binary"): SIGTERM, then poll until the daemon stops accepting connections. `daemon restart` does the same and lets the next invocation lazy-spawn a fresh one. This is the right recommendation everywhere, not only on macOS — it is the supported path with an agent-safety refusal built in, regardless of what `pkill`/`pgrep` happen to do on a given platform. (On Linux, unlike here, `pkill -f 'daemon serve'` matches normally — "pkill doesn't stop the daemon" below is a macOS-specific symptom of a recommendation that holds everywhere for a different, better reason.)

Do not reach for `pkill` even when it seems to have failed rather than worked: a broadened pattern is **dangerous**, not just ineffective. Measured on this machine, `pgrep -fl 'agent-deck'` matches dozens of live processes beyond the daemon — every running agent pane's shell and in-flight `cargo`/`devbox` builds, across every concurrent orchestration on the box. `pkill -f 'agent-deck'` kills all of that, not just the daemon, and none of the in-flight agent context it destroys comes back.

`pgrep -f 'daemon serve'` **matches nothing when run from inside a deck-managed pane**, even while the daemon is plainly running — verified 2026-08-11 against a live daemon that `ps -Ao pid,command` listed as `/Users/…/.local/bin/worker-agent-deck daemon serve`. The cause is **not** an argv-visibility limit: `ps` reads the daemon's full argv fine (that's the output above), and a plain `setsid` process of your own is still matched by `pgrep -f`, which directly rules out daemonization as the reason. The actual cause, from `man pgrep`: by default `pgrep`/`pkill` **exclude the calling process and all of its own ancestors** from the match. The daemon spawns every agent pane, so from inside one, the daemon is an ancestor of your shell — diffing `pgrep -af '.'` against `ps -Ao pid` turns up exactly the caller's ancestor chain missing, daemon included, and `pgrep -af 'daemon serve'` (the `-a` includes ancestors) matches normally from the same pane. This makes the failure **context-, not platform-specific**: from an ordinary terminal that is not a descendant of the deck, `pgrep -f`/`pkill -f 'daemon serve'` both work as expected — it only fails from inside a pane the daemon itself spawned. Do not generalize this to "pgrep can't see setsid/daemonized processes" — that was tested directly and is false. Match with `ps … | grep` when you need to see the process from inside a pane; `daemon stop` doesn't have this problem at all, since it connects over the socket rather than searching the process table.

If hooks were installed by an older binary and you want them pointing at the new one, re-run `hooks install` **from the new binary** — the path is baked at install time, so nothing else updates it. Reinstalling under the *same* filename does not need this: the baked path is unchanged, so the existing hook entries keep working and simply pick up the new build.

**Adopting this page's naming convention is not the same-filename case, and does need this step.** Moving from an old install (e.g. a fork build that was sitting at `~/.local/bin/dot-agent-deck`) to `~/.local/bin/worker-agent-deck` changes the path baked into `~/.claude/settings.json` at install time — the existing hook entries keep pointing at whatever last ran `hooks install`, stale binary and all, and nothing prunes them automatically. Verify and fix in one pass:

```bash
grep -o '[^"]*agent-deck[^"]*' ~/.claude/settings.json | sort -u   # every baked hook path
worker-agent-deck hooks install                                    # re-bake from the current binary
grep -o '[^"]*agent-deck[^"]*' ~/.claude/settings.json | sort -u   # confirm exactly one path remains
```

A stray second entry here is not cosmetic: it means some hook events are still being delivered by whatever binary that entry points at, and per "Row 2 collides" above, a protocol-mismatched delivery on that path fails **silently** — you won't see it fail, you'll just stop getting the hook.

## What it costs

Every pane dies. Worktrees, branches and pushed commits all survive — only in-flight agent context is lost, so any interrupted worker needs re-delegating. With several concurrent orchestrations that can be a dozen panes at once, which is why the upgrade tends to get deferred, which is why fixes sit undeployed.

**That deferral is the real hazard.** A merged fix for a bug you are actively hitting is worth nothing until the daemon restarts, and the symptom looks identical before and after the merge.

## Fork releases never reach Homebrew

`release.yml` gates the Homebrew and Scoop publish steps on `github.repository == 'vfarcic/dot-agent-deck'` (`:382`, `:388`, and the matching Scoop steps). The comment above them explains why: those steps clone `vfarcic/homebrew-tap` and `vfarcic/scoop-bucket` and bake `github.com/vfarcic/dot-agent-deck/releases/download/…` URLs into artifacts only they consume. *"None of that can or should succeed from a fork"* — and on the fork's first tag-triggered release (v0.35.8) `Publish Homebrew formula` failed on a missing `HOMEBREW_TAP_TOKEN`, which failed `finalize`, which skipped the rest.

So **`vfarcic/tap` can never ship a fork build**. A fork release produces GitHub assets only, and `brew upgrade` will only ever move you between upstream builds. Installing a fork build means fetching the asset (or building it) and putting it on `PATH` yourself.

This is also why a fork build and a brew build can coexist and be confused for one another: `/opt/homebrew/bin/dot-agent-deck` stays installed and becomes active again the moment anything earlier on `PATH` is removed. **The naming convention at the top of this page is the answer to that** — keeping fork builds under `worker-agent-deck` means the two can never shadow each other by accident, and `which` alone tells you which lineage you are about to run.

It also means a stray fork **build** installed as `~/.local/bin/dot-agent-deck` is worse than a duplicate: it shadows the brew binary while wearing its name, so `dot-agent-deck --version` reports a fork version that no `brew upgrade` will ever move — and it goes stale independently of the build you actually upgrade, so the two names drift apart silently. If you find one, replace it with the symlink described above rather than deleting it outright; deleting it does not fail loudly the way a protocol mismatch would — it silently falls through to the protocol-mismatched brew binary, and `delegate`/`work-done` run against it and exit 0 while their signals vanish (see "Row 2 collides with the naming convention" above).
