---
title: Quickstart
description: Open a Splinterm terminal, close the window, and jump back into your running session.
---

The quickest way to understand Splinterm is to close a window while something is still running inside it.

## 1. Install Splinterm

Follow [Installation](/docs/install/) on x86_64 Omarchy or Arch Linux. If you're on Omarchy, run `splinterm integration omarchy enable` once so `Super+C`/`Super+V` and terminal transparency work properly.

## 2. Open a terminal

Launch Splinterm from your app menu, or run:

```bash
splinterm-xdg-terminal-exec
```

That opens a fresh persistent **Lair** with one **Dojo** and one terminal pane (**Splint**), starting in your current directory.

## 3. Close the window while work is running

Start something easy to recognize—`htop`, a tailing log, or a dev server—then close the window.

Closing the window (or its last tab) only detaches the graphical client. The daemon (`splinterd`) keeps your shell and layout running in the background.

## 4. Pick up where you left off

Open the native Dojo picker:

```bash
splinterm dojos
```

Select your running Dojo to reattach (`splinterm sessions` is an alias for the same command). Inside any Splinterm window, press **Ctrl+Shift+S** to open the same picker.

To jump straight back into whichever running Dojo you used last:

```bash
splinterm reopen
```

:::note[Reopening vs. restoring]
Reopening attaches to processes that are still running. If a shell or command has already exited, starting it again from saved launch metadata is an explicit **restore**—Splinterm never re-runs exited commands on its own.
:::

## Everyday shortcuts

| Action | Shortcut |
| --- | --- |
| Command palette | `Ctrl+Shift+P` |
| Recent Dojos | `Ctrl+Shift+S` |
| Split horizontally | `Ctrl+Shift+Enter` |
| Split vertically | `Ctrl+Shift+\` |
| Move between panes | `Ctrl+Shift+Arrow` |
| Cycle tabs | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| Jump to tab 1–9 | `Ctrl+Shift+1` … `Ctrl+Shift+9` |
| New Dojo tab | `Ctrl+Shift+D` |
| Detach active tab | `Ctrl+Shift+Q` |
| Toggle tab strip | `Ctrl+Shift+B` |
| Search scrollback | `Ctrl+Shift+F` |
| Copy / paste | `Ctrl+Shift+C` / `Ctrl+Shift+V` |

A few handy details as you settle in:

- **Copy & paste on Omarchy:** `Super+C`/`Super+V` and `Ctrl+Insert`/`Shift+Insert` also work as copy/paste aliases alongside `Ctrl+Shift+C`/`V` while leaving `Ctrl+C` as normal terminal interrupt—just make sure you've run `splinterm integration omarchy enable` (see [Recommended on Omarchy](/docs/install/#recommended-on-omarchy)).
- **Jumping back to live output:** When scrolled back through history, press `Enter` once to snap back to live output without sending a newline to your shell.
- **Prefer tmux chords?** Set `profile=omarchy-tmux` under `[key-bindings]` in `config.ini` for `Ctrl+Space` and `Ctrl+B` prefixes, searchable key help (`Prefix+?`), vi copy mode (`Prefix+[`), and quick Lair save/pin/preview/restore bindings (`Prefix Shift+S/F/V/O`).

## Where to go next

- Learn how Lairs, Dojos, Splints, and windows relate in [Core concepts](/docs/concepts/).
- Configure window-owned transient terminals, restore, and reset in [Sessions and persistence](/docs/sessions/).
- Customize fonts, Omarchy themes, and keymaps in [Configuration](/docs/configure/configuration/), or spin up full multi-pane workspaces with [Dojo presets](/docs/presets/).
