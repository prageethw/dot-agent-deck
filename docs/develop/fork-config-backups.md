# Fork-only config backups

This repository is a fork of `vfarcic/dot-agent-deck` (the `upstream` remote). Two config files are intentionally divergent from upstream in this fork:

- `devbox.json`
- `.dot-agent-deck.toml`

Upstream ships the original per-model devbox scripts (`agent-orchestrator`, `pi-sol`, `oc-big`, `codex-big`, and so on). This fork swapped the worker-role devbox commands to two generic wrappers (`claude-sonnet-devbox`, `codex-devbox`) in PR #7. Because both files also exist upstream with different content, a future `git fetch upstream && git merge upstream/main` could touch them if upstream edits the same files — silently reverting or mangling the fork's customisation.

## The backup files

Alongside each live file sits a byte-for-byte snapshot of the current fork version:

- `devbox.json.fork-backup`
- `.dot-agent-deck.toml.fork-backup`

These are **not** an automatic guard — they do not *prevent* an upstream merge from overriding the live files. They exist so that an accidental override can be **detected and undone manually**. (A `.gitattributes merge=ours` guard was considered and deliberately not adopted; the manual backup-and-restore approach was chosen for its simplicity.)

## Check-and-restore procedure (run after any upstream sync)

After `git merge upstream/main` (or any upstream sync), diff each live file against its backup:

```bash
diff devbox.json devbox.json.fork-backup
diff .dot-agent-deck.toml .dot-agent-deck.toml.fork-backup
```

If a `diff` reports no output, the file is unchanged and nothing needs doing.

If a `diff` shows differences, the merge changed something. Then either:

- **Restore the fork's version** (if the fork's config should win):

  ```bash
  cp devbox.json.fork-backup devbox.json
  cp .dot-agent-deck.toml.fork-backup .dot-agent-deck.toml
  ```

- **Or review the diff first** — upstream's change might be worth keeping, in which case merge the two by hand rather than blindly restoring.

## Keeping the backups fresh

If a live file is intentionally changed again in the future (another fork-only config update), refresh the corresponding `.fork-backup` file **in the same commit**:

```bash
cp devbox.json devbox.json.fork-backup
# and/or
cp .dot-agent-deck.toml .dot-agent-deck.toml.fork-backup
```

Otherwise the backup goes stale and the `diff` check becomes noise — it would report differences that reflect an out-of-date snapshot rather than an upstream override.
