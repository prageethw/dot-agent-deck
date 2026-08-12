# Upgrading a running deck — why it is not a `brew upgrade`

Merging a fix does not deploy it. **A running deck keeps using the binary it was launched from, and its installed hooks keep calling the binary that installed them**, so a fix can sit on `main` for a long time while the deck you are actually using still has the bug.

This is not a bug in itself — every resolution below is deliberate and has a reason recorded at its call site. But the consequences are not obvious, and rediscovering them costs real time. This page is the record.

## Three things resolve the binary, three different ways

| What | How it resolves | Consequence |
|---|---|---|
| **The daemon** | `std::env::current_exe()`, explicitly **not** `$PATH` (`src/daemon_attach.rs:247`) | It becomes whatever binary the TUI that spawned it was |
| **`delegate` / `work-done`** | Agents invoke them **by name**, so `$PATH` decides | Governed by `.dot-agent-deck.toml`'s role commands, independent of the above |
| **Installed hooks** | An **absolute path baked in at install time**, also from `current_exe()` (`src/hooks_manage.rs:193`, `:230`; `src/codex_hooks_manage.rs:438`) | A hook keeps calling whatever binary installed it until `hooks install` is re-run |

The `current_exe()` choice is deliberate and should not be "fixed" to use `$PATH`. The doc comment at `daemon_attach.rs:247` records why: *"non-interactive ssh shells routinely skip `~/.local/bin` (we hit this exact bug three times — commits `493248b`, `bbf2236`, `ea8c748`)."*

## The trap: attaching is not spawning

The daemon is **lazy-spawn-on-attach** — `ensure_daemon_running` probes the attach socket and only spawns when it is unreachable.

So if you launch a new TUI while an old daemon is still alive, **it attaches to the old daemon**. No error, no warning, no change in behaviour. It looks exactly like the upgrade did not work, because functionally it did not.

There is also **no launchd job** — no plist, nothing in `launchctl list`. The daemon does not self-respawn; it comes back from whichever TUI you next launch. That is what makes the sequence below work at all.

## The sequence that actually upgrades it

```bash
# 1. Quit every TUI, then stop the daemon. THIS MUST COME FIRST —
#    otherwise step 2 just attaches to the old one.
pkill -f 'dot-agent-deck daemon serve'

# 2. Relaunch from the binary you want to be running.
<your-binary>

# 3. Verify you got what you expected.
pgrep -fl 'daemon serve'          # must name the new binary, not the old path
<your-binary> daemon status       # roster — confirms the new build is live
```

If hooks were installed by an older binary and you want them pointing at the new one, re-run `hooks install` **from the new binary** — the path is baked at install time, so nothing else updates it.

## What it costs

Every pane dies. Worktrees, branches and pushed commits all survive — only in-flight agent context is lost, so any interrupted worker needs re-delegating. With several concurrent orchestrations that can be a dozen panes at once, which is why the upgrade tends to get deferred, which is why fixes sit undeployed.

**That deferral is the real hazard.** A merged fix for a bug you are actively hitting is worth nothing until the daemon restarts, and the symptom looks identical before and after the merge.

## Fork releases never reach Homebrew

`release.yml` gates the Homebrew and Scoop publish steps on `github.repository == 'vfarcic/dot-agent-deck'` (`:382`, `:388`, and the matching Scoop steps). The comment above them explains why: those steps clone `vfarcic/homebrew-tap` and `vfarcic/scoop-bucket` and bake `github.com/vfarcic/dot-agent-deck/releases/download/…` URLs into artifacts only they consume. *"None of that can or should succeed from a fork"* — and on the fork's first tag-triggered release (v0.35.8) `Publish Homebrew formula` failed on a missing `HOMEBREW_TAP_TOKEN`, which failed `finalize`, which skipped the rest.

So **`vfarcic/tap` can never ship a fork build**. A fork release produces GitHub assets only, and `brew upgrade` will only ever move you between upstream builds. Installing a fork build means fetching the asset (or building it) and putting it on `PATH` yourself.

This is also why a fork build and a brew build can coexist and be confused for one another: `/opt/homebrew/bin/dot-agent-deck` stays installed and becomes active again the moment anything earlier on `PATH` is removed.
