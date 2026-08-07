---
sidebar_position: 6
title: Keyboard Shortcuts
---

# Keyboard Shortcuts

## Mouse

Every keyboard action below is also reachable with the mouse — the dashboard is fully clickable, and every clickable control carries its keyboard shortcut inline, so the on-screen controls double as a legend and clicking one does exactly what its shortcut does. Two things the labels cannot tell you: a single click on a dashboard card selects it while a double click focuses its pane, and the button bar along the bottom wraps onto more rows on a narrow terminal rather than dropping any of its commands.

A mode tab's side panes scroll when the pointer is over them; anywhere else the wheel scrolls the focused pane. In command mode it always moves Agent Deck's own scrollback and is never forwarded to the agent, so a full-screen TUI running in a pane cannot move under you while you read. While you are typing in a pane, the wheel goes to the agent whenever the agent has mouse reporting enabled.

## Global Shortcuts

| Key | Action | Works from |
|---|---|---|
| `Ctrl+D` | Toggle between command mode and the pane — press it in a pane to reach the dashboard, press it again to go back to the pane you came from | Any mode |
| `Ctrl+N` | New pane (directory picker, then name + command form) | Any mode |
| `Ctrl+T` | Toggle stacked / tiled layout — stacked shows only the focused pane at full height, tiled shows every pane at once | Any mode |
| `Ctrl+L` | Cycle the sidebar/pane-column split: Default → Narrow (25/75) → Hidden (sidebar gone, pane column full-width) → Default. One stage for the whole deck — cycling on any tab moves every other tab too, and a tab you open afterwards adopts the current stage | **Command mode only**, on **Dashboard and Orchestration tabs** |
| `Ctrl+E` | Toggle command-entry lock — while locked, typing on any pane other than the orchestrator's does not reach that pane's PTY, unless that pane is waiting for input. One lock for the whole deck — toggling it on any Orchestration tab changes every other one too, and a tab you open afterwards adopts the current value | **Command mode only**, on **Orchestration tabs only** |
| `Ctrl+W` | Close the selected pane on the dashboard, or tear down the entire mode tab (agent + side panes) when used on a mode tab — after a confirmation dialog. The dashboard tab itself cannot be closed. | **Command mode only** |

### Which mode you're in

`Ctrl+D` toggles between two modes, and the deck names the one you are in. A chip at the far left of the bottom bar reads ` COMMAND ` when your keystrokes drive the deck and ` TYPING ` when they go into the focused pane — in the same place on every tab, except while an inline **Filter** or **Rename** field is open, where that row *is* the input field and its own prompt tells you where your keystrokes are going. Those two words are the vocabulary the rest of this page uses: "command mode" is ` COMMAND `, and "typing in a pane" (`PaneInput` internally) is ` TYPING `. The chip says where you are; the first button in the bar — `[Back to Pane Ctrl+D]` or `[Command Mode Ctrl+D]` — says where `Ctrl+D` would take you.

Three other things follow the mode:

- **The cursor.** The focused pane shows a cursor only while you are typing into it. A cursor means, without exception, that what you type lands in that pane.
- **The focused pane dims, with a banner.** Entering command mode dims the focused pane's content — still perfectly readable, just visibly inert — and overlays a `COMMAND MODE · Ctrl+D to type` banner. The banner clears itself after a moment, or immediately when you press a command-mode key or click a bottom-bar button; a key that isn't bound to anything keeps it up, because that is the moment you most likely thought you were talking to the agent. The dimming stays for as long as you are in command mode.
- **The selected dashboard card.** It keeps its `▸ ` marker in both modes so you never lose track of the selection, but its highlight is de-emphasised while you are typing in a pane — the deck looks inert exactly when the pane looks live.

### `Ctrl+L` cycles one split for the whole deck, from command mode

`Ctrl+L` is clear-screen in shells, readline, and every agent you run in a pane — Claude Code included. So while you are typing in a pane, `Ctrl+L` is sent straight through to that program as `^L` (byte `0x0c`) and clears its screen; it does not move the split. Press `Ctrl+D` first, and `Ctrl+L` there cycles the split. It is the same trade `Ctrl+W` makes below: one extra keystroke when you meant the deck, in exchange for the chord always reaching the program you are typing into.

The stage is a single deck-wide value, not one per tab. Cycling it on any Dashboard or Orchestration tab moves every other tab as well, and a tab you open afterwards starts at whatever stage is current. What a stage resolves to still depends on the tab type: `Default` is 33/67 on the Dashboard and 34/66 in an Orchestration tab, while `Narrow` (25/75) and `Hidden` look the same everywhere. Mode tabs have no sidebar split, so `Ctrl+L` never cycles anything from one and reaches the pane as ordinary input. The stage resets to `Default` on the next launch rather than persisting.

### `Ctrl+E` locks command entry to the orchestrator pane

In an Orchestration tab, direct keystrokes are locked to the orchestrator pane by default — typing while focused on any other pane in that tab does not reach that pane's PTY, and a status message ("Pane locked — Ctrl+d then Ctrl+e to unlock") confirms the keystroke was dropped rather than silently swallowed. The orchestrator pane itself always accepts input regardless of lock state, and while unlocked every pane in the tab accepts direct input normally.

**`Ctrl+e` toggles the lock from command mode only.** `Ctrl+e` is end-of-line in shells, readline, and every agent you run in a pane. So while you are typing in a pane it is sent straight through to that program as `^E` (byte `0x05`) and moves the cursor to the end of the line; it does not touch the lock. Press `Ctrl+D` first, and `Ctrl+e` there toggles it. It is the same trade `Ctrl+W` and `Ctrl+L` make: one extra keystroke when you meant the deck, in exchange for the chord always reaching the program you are typing into. `Ctrl+d`, `Ctrl+n` and `Ctrl+t` still work from any pane, locked or not — the lock never swallows a global chord.

**The lock is one value for the whole deck, not one per tab.** Toggling it on any Orchestration tab changes it on every other open Orchestration tab as well, and an Orchestration tab you open afterwards adopts whatever the current value is rather than starting locked. It reflects how you are working right now, not which tab you happened to open. The value resets to locked on the next launch rather than persisting.

**A pane that is waiting for input is not locked.** While a non-orchestrator role pane reports `Needs Input` — the agent has stopped and asked you something, and the sidebar and tab label already say so — the lock stops gating that pane entirely and your keystrokes reach it, so you can answer the question where it was asked. The lock re-engages the instant the status clears. The lock's subject is the *unsolicited* interruption: typing at an agent that is working, while the orchestrator believes it owns that state. Answering an agent that explicitly stopped and asked is not one, so it costs no unlock. One consequence worth knowing: status is agent-reported, so an agent that never reports `Needs Input` gets no carve-out and still needs a deliberate unlock, and a pane whose status is stuck on `Needs Input` stays typeable with nothing on screen to distinguish it from a correctly-waiting one.

Dashboard and Mode tabs are unaffected — the lock never applies there, and `Ctrl+e` on those falls through as ordinary input (e.g. readline's end-of-line binding) in both modes.

### `Ctrl+W` closes only from command mode

`Ctrl+W` is delete-previous-word in shells, readline, vim, and essentially every program you run inside a pane. So while you are typing in a pane, `Ctrl+W` is sent straight through to that program as `^W` (byte `0x17`) and deletes a word — it does not close anything. Press `Ctrl+D` first, and `Ctrl+W` there asks you to confirm before closing.

The confirmation defaults to **Cancel**, so an accidental `Ctrl+W` followed by a reflexive `Enter` leaves your pane exactly where it was. Choosing **Close** stops the agent and removes the card.

### `Ctrl+C`

While you are typing in a pane, `Ctrl+C` is delivered to the terminal as SIGINT (0x03). In command mode it opens the quit dialog — **Detach** (default) / **Stop** / **Cancel**, see [Dialogs](#dialogs) — and pressing `Ctrl+C` again from there leaves immediately.

## Tab Navigation

The tab bar appears when more than one tab is open.

| Key | Action |
|---|---|
| `Ctrl+PageDown` | Next tab (works from any mode, including in a focused pane) |
| `Ctrl+PageUp` | Previous tab (works from any mode, including in a focused pane) |
| `Tab` / `Right` / `l` | Next tab — **only in command mode** |
| `Shift+Tab` / `Left` / `h` | Previous tab — **only in command mode** |

The command-mode-only keys reach the agent instead while you are typing in a pane, so press `Ctrl+D` first.

## Mode Tab

These shortcuts work in command mode when a mode tab is active.

| Key | Action |
|---|---|
| `j` / `Down` | Focus next pane (cycles: agent → side panes → agent) |
| `k` / `Up` | Focus previous pane (cycles: agent → last side pane → … → agent) |
| `Enter` | Start typing into the selected pane (agent pane if none selected) |
| `Esc` | Deselect side pane (return focus indicator to agent) |
| Mouse click | Click a side pane to select it; click agent pane to deselect |

`Ctrl+D` leaves the pane, and `Ctrl+D` again goes back into it.

## Dashboard

These shortcuts work in **command mode**. If you're typing in an agent pane, press `Ctrl+D` first to leave the pane — otherwise the keystroke is sent to the agent.

| Key | Action |
|---|---|
| `j` / `Down` | Select next card (wraps at end) |
| `k` / `Up` | Select previous card (wraps at start) |
| `1`–`9` | Jump to card N and focus its pane |
| `PageUp` | Scroll the focused pane back (see [Scrolling back through a pane](#scrolling-back-through-a-pane)) |
| `PageDown` | Scroll the focused pane forward |
| `/` | Filter sessions (opens filter input — see [Dialogs](#dialogs)) |
| `r` | Rename selected session (opens rename input — see [Dialogs](#dialogs)) |
| `g` | Generate `.dot-agent-deck.toml` (opens config-generation prompt — see [Dialogs](#dialogs)) |
| `s` | Open the **Scheduled Tasks** manager (`S` also works) (see [Scheduled Tasks](./scheduled-tasks.md)) |
| `?` | Toggle help overlay |
| `y` / `n` | Approve / deny a pending permission request (only when an agent is waiting) |
| `Esc` | Clear active filter |

### Scrolling back through a pane

`PageUp` / `PageDown` scroll the focused pane's output back and forward — the keyboard equivalent of the scroll wheel. They are the `scroll_pane_up` and `scroll_pane_down` actions and are remappable like any other binding (see [Actions and defaults](#actions-and-defaults)).

They work in **command mode only**. While you are typing in a pane they are sent straight through to whatever is running there as `ESC[5~` / `ESC[6~`, so a pager, an editor, or the agent's own scrollback keeps them; press `Ctrl+D` first and the same keys scroll the deck's view of the pane instead. `Ctrl+PageUp` / `Ctrl+PageDown` are separate chords and stay on tab navigation.

## Directory Picker

| Key | Action |
|---|---|
| `j` / `Down` | Select next directory |
| `k` / `Up` | Select previous directory |
| `l` / `Right` / `Enter` | Enter directory (or confirm if no subdirs) |
| `h` / `Left` / `Backspace` | Go up one level |
| `Space` | Confirm current directory |
| `/` | Enter filter mode; type to narrow directories (case-insensitive) |
| `Esc` | Clear filter (press twice to close) |
| `q` | Cancel |

Directory lists loop end-to-end, and the `..` parent entry stays visible even when a filter is active.

## New Pane / Mode Form

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Switch between fields |
| `Left` / `Right` / `h` / `l` | Cycle mode selector (when modes available) |
| `Enter` | Confirm field / submit form |
| `Esc` | Cancel |

## Dialogs

Several dashboard shortcuts open transient input fields or selection dialogs. The keys for each:

| Dialog | Trigger | Keys |
|---|---|---|
| **Filter** | `/` | Type to narrow visible cards · `Backspace` to delete · `Enter` to accept and stay filtered · `Esc` to clear and close |
| **Rename** | `r` | Type the new name · `Enter` to confirm · `Esc` to cancel |
| **Generate config** | `g` | `Up`/`Down` (or `k`/`j`) to choose **Yes** / **No** / **Never** · `Enter` to confirm · `Esc` to cancel. **Yes** sends a prompt to the agent to write `.dot-agent-deck.toml`; **Never** suppresses the hint permanently for that directory. |
| **Quit** | `Ctrl+C` from command mode | `Up`/`Down` (or `k`/`j`) to choose **Detach** (default) / **Stop** / **Cancel** · `Enter` to confirm · `Esc` to dismiss · `Ctrl+C` again to leave immediately. Detach keeps your agents running in the daemon; Stop terminates them and asks once more first. |
| **Close confirmation** | `Ctrl+W` from command mode, the `[Close]` button, or a tab's `[×]` | `Up`/`Down` (or `k`/`j`) to choose **Cancel** (default) / **Close** · `Enter` to confirm · `Esc` to dismiss. The dialog names its target — a single dashboard pane, or a Mode/Orchestration tab and all its panes. It closes exactly what was selected when it opened, and any keystroke you typed before it appeared is discarded rather than answering it. If a pane refuses to stop, the tab is kept holding whatever could not be closed, so you can press `Ctrl+W` again to retry. |
| **Help overlay** | `?` | `?`, `Esc`, or `q` to dismiss |

## Customizing Keybindings

Every shortcut above can be remapped. dot-agent-deck reads an optional config file at:

```
~/.config/dot-agent-deck/keybindings.toml
```

(Override the path with the `DOT_AGENT_DECK_KEYBINDINGS` environment variable.) Keybindings are resolved **client-side**, on the machine running the TUI — so when two clients attach to one remote daemon, each can have its own bindings.

The file has two sections, `[global]` and `[dashboard]`. You only need to list the actions you want to change; everything else keeps its default. The help overlay (`?`) and the button bar are generated from the active config, so they always show your real keys.

### Key notation

- **Modifiers:** `Ctrl+`, `Alt+`, `Shift+` — combine in any order, e.g. `Alt+Shift+t`.
- **Named keys:** `Enter`, `Esc`, `Tab`, `Space`, `Up`, `Down`, `Left`, `Right`, `Backspace`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, and `F1`–`F12`.
- **Printable characters:** `a`–`z`, `0`–`9`, `/`, `?`, etc.
- **Unbound:** an empty string (`new_pane = ""`) disables the action entirely.

Notation is case-insensitive for modifier and named keys (`ctrl+enter` == `Ctrl+Enter`).

### Example

```toml
# ~/.config/dot-agent-deck/keybindings.toml
# Only override what you need — defaults apply for everything else.

[global]
toggle_layout = "Alt+Shift+l"   # move it off Ctrl+t
toggle_orchestration_split = "Alt+Shift+s"   # move it off Ctrl+l
new_pane = ""                    # disable the new-pane shortcut

[dashboard]
help = "F1"                      # open help with F1 instead of ?
```

### Actions and defaults

`[global]`:

| Action | Default | Description |
|---|---|---|
| `dashboard` | `Ctrl+d` | Toggle between command mode and the pane — works from any mode |
| `new_pane` | `Ctrl+n` | New pane (directory picker → name + command) — works from any mode |
| `close_pane` | `Ctrl+w` | Close selected pane / tear down mode tab, with confirmation — **command mode only**; in a pane the chord is ordinary input for whatever is running there |
| `toggle_layout` | `Ctrl+t` | Toggle stacked / tiled layout — works from any mode |
| `toggle_orchestration_split` | `Ctrl+l` | Cycle the deck's sidebar/pane-column split — Default → Narrow (25/75) → Hidden (0/100) → Default — on **Dashboard and Orchestration tabs**, **command mode only**; one stage shared by every tab, and in a pane the chord is ordinary input (clear-screen) for whatever is running there |
| `toggle_orchestration_lock` | `Ctrl+e` | Toggle command-entry lock — while locked (the default), only the orchestrator pane accepts direct input, plus any pane that is waiting for input — on **orchestration tabs**, and only while you are **not typing into a pane**; one lock shared by every Orchestration tab. In command mode the deck claims the chord even when a pane is focused; once you are typing into a pane it is sent through to whatever is running there as ordinary input (end-of-line), so press `Ctrl+D` first |
| `jump_1` … `jump_9` | `1` … `9` | Jump to card N and focus its pane |

`close_pane`, `toggle_orchestration_split` and `toggle_orchestration_lock` live in `[global]` because the section names the TOML table your binding is read from, not the modes it applies in. Whatever chord you bind any of the three to is command-mode only and reaches the pane as ordinary input everywhere else.

`[dashboard]` (command mode):

| Action | Default | Description |
|---|---|---|
| `move_down` | `j` | Select next card |
| `move_up` | `k` | Select previous card |
| `move_left` | `h` | Previous tab |
| `move_right` | `l` | Next tab |
| `filter` | `/` | Filter sessions |
| `rename` | `r` | Rename selected session |
| `help` | `?` | Toggle help overlay |
| `focus_pane` | `Enter` | Focus selected pane |
| `clear_filter` | `Esc` | Clear active filter |
| `approve_permission` | `y` | Approve a pending permission request |
| `deny_permission` | `n` | Deny a pending permission request |
| `generate_config` | `g` | Generate `.dot-agent-deck.toml` (config-generation prompt) |
| `scroll_pane_up` | `PageUp` | Scroll the focused pane back — **command mode only**; in a pane the key is passed to the agent |
| `scroll_pane_down` | `PageDown` | Scroll the focused pane forward — **command mode only**; in a pane the key is passed to the agent |

The `Down`/`Up`/`Tab`/`Shift+Tab`/`Left`/`Right` aliases and `Ctrl+PageUp` / `Ctrl+PageDown` tab navigation are not remappable and always work alongside your bindings. Because `Ctrl+PageUp` / `Ctrl+PageDown` are separate chords from the unmodified `PageUp` / `PageDown`, remapping the scroll actions does not affect tab navigation.

Rebinding an action both enables the new chord and retires the default, so `scroll_pane_up = "Ctrl+u"` leaves plain `PageUp` doing nothing in command mode. Setting either scroll action to `""` leaves the wheel as the only way to scroll that pane.

**Quit is not a remappable action.** No key directly quits — `Ctrl+C` (hardcoded, non-overridable) opens the quit dialog (Detach / Stop / Cancel). There is no `quit` config key; a `quit = "…"` line is treated as an unknown action and ignored with a warning.

### Edge cases

- **No config file** → all defaults.
- **Malformed file** → dot-agent-deck warns on stderr and falls back to all defaults; it never crashes.
- **Conflicting bindings** (two actions on the same key) → a warning is printed and the first-defined action wins; the later one is left unbound.
- **Unknown action name** → ignored with a warning.
- **Empty binding** (`action = ""`) → that action is unbound and its default key does nothing.
- **`Ctrl+c` is never routed through your config.** Even if you bind another action to it, `Ctrl+c` from command mode always opens the quit dialog — it cannot be turned into "new pane", "switch tab", or anything else.
