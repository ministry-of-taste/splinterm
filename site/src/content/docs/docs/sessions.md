---
title: Sessions and persistence
description: How Lairs, Dojos, Splints, windows, transient terminals, restore, and reset work.
---

`splinterd` owns your terminal processes and session state. Wayland windows attach and detach freely—closing a window doesn't kill the shells running underneath unless you configure transient behavior or explicitly terminate a session.

## Opening new work vs. returning to existing work

By default (`multiplexer.persistent-by-default=yes`), every ordinary graphical launch creates a persistent **Lair**.

If you prefer throwaway terminals for quick one-off commands, set `persistent-by-default=no` under `[multiplexer]` in `config.ini`:

- Commandless desktop launches (`Super+Enter`), bare `splinterm launch`, picker **New**, and in-window **New Terminal** become **transient** (owned by that window). Closing the window terminates and removes the Lair.
- Explicit names, native command launches (`splinterm launch -- CMD`), presets, restore/relaunch, remote sessions, automation, and MCP always remain persistent.
- With `multiplexer.persist-on-tab-organization=yes` (the default), adding a second Dojo tab or naming/renaming a Dojo permanently promotes that transient Lair to persistent. Set it to `no` to keep organized multi-tab Lairs tied to the window.
- Launches invoked through XDG with a command (for example, another app opening a terminal to run a specific program) are always transient and end when the command finishes or the window closes.

Unpromoted transient Lairs never appear in Recent Dojos, aren't saved to disk, and aren't picked by `reopen`.

To jump back into running work:

```bash
splinterm dojos   # open Recent Dojos or start a new terminal
splinterm reopen  # attach directly to the last running Dojo
```

(`splinterm sessions` is an alias for `splinterm dojos`.)

One window can hold up to **32 distinct Dojos** as local tabs, including Dojos from different Lairs. Opening a Dojo that is already tabbed in the current window switches to that tab instead of duplicating it. Press **Ctrl+Shift+B** to hide or show the tab strip for that window.

## What closing does

Splinterm separates disconnecting a view from ending a process:

| Action | Effect |
| --- | --- |
| Close a persistent window | Detaches all of its tabs; running shells continue in `splinterd`. |
| Close a window owning an unpromoted transient Lair | Terminates its processes and removes the Lair. |
| Close a transient XDG command window | Terminates its processes and removes the Lair. |
| Close a tab | Detaches that Dojo from the window (unless `close-unused=yes` matches an untouched shell). |
| Close other tabs | Detaches every other tab in the window (or ends untouched shells when `close-unused=yes`). |
| **Terminate Dojo…** (tab or Explorer menu) | Prompts for confirmation, then ends every running Splint in that Dojo. |
| **Terminate Splint** | Prompts for confirmation, then ends that pane's process. |
| Close an exited Splint | Removes the exited pane and collapses its split. |
| Close a Dojo | Removes the Dojo once all of its Splints have exited. |
| **Restore** | Starts a new process in an exited Splint using saved launch metadata. |

## Tabs, Explorer, and the command palette

- **Tab strip:** Click to switch tabs, close a tab to detach it, or use `+` to open Recent Dojos. Right-clicking a tab opens its menu without switching away from your current tab first (`Rename tab…`, `Activate tab`, `New Dojo`, detach-only `Close tab` / `Close other tabs`, and confirmed `Terminate Dojo…` at the bottom).
- **Lair Explorer:** Right-clicking a Dojo offers `Open`, `Rename…`, `Restore…`, `Close tab`, and `Terminate Dojo…`; right-clicking a Lair ends with `Terminate Lair…`.
- **Command palette (`Ctrl+Shift+P`):** **Choose Lair** switches to a Lair's most recent Dojo, **Choose Dojo** lists only Dojos in the active Lair, and **Recent Dojos** (`Ctrl+Shift+S`) searches across all Lairs.

These controls belong to the trusted client chrome—terminal output inside a pane can never draw over, rename, or trigger them.

## What gets saved to disk

Session metadata lives in `${XDG_STATE_HOME:-~/.local/state}/splinterm/`, written atomically with owner-only permissions and a previous-generation backup.

- **Saved:** Lair/Dojo/Splint IDs, names, split layouts, focus hints, lifecycle state, and launch metadata (working directory and command argv) for persistent Lairs.
- **Never saved:** Terminal screen contents, scrollback history, clipboard contents, PTY handles, agent grants, controller tokens, or transient XDG command Lairs.

If metadata is corrupted on startup, `splinterd` quarantines it rather than crashing, and Splinterm never re-runs saved commands automatically—restarting exited work always requires an explicit **restore**.

## Resetting all sessions

If you want a completely clean slate, use the guarded reset command instead of deleting state files by hand:

```bash
splinterm reset       # prompts for confirmation
splinterm reset --yes # skip prompt
```

`reset` moves the current session database to a timestamped backup, restarts `splinterd.service`, waits for the socket to come back, and leaves your `config.ini` and automation policy untouched.

:::caution
`splinterm reset` clears your entire persistent topology. Run `splinterm list --all` first if you want to check what is running.
:::

## Remote Dojos

With an SSH profile in `remotes.toml`, you can open remote sessions as native Wayland windows:

```bash
splinterm --remote PROFILE
splinterm --remote PROFILE reopen
```

You can also open remote hosts without touching a terminal using the **Splinterm Remotes** app-menu entry, the **Remote Dojos** desktop action, `splinterm remotes`, **Remote Dojos…** inside Recent Dojos, or **Open Remote…** from the command palette.

Local and remote recency are tracked separately. If SSH drops, your local window closes and releases input control without terminating the remote daemon's shells. See [Remote access](/docs/remote/) for setup details.
