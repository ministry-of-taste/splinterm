---
title: Sessions and persistence
description: Work with Lairs, Dojos, Splints, windows, detach, reopen, restore, and reset.
---

`splinterd` owns terminal processes and persistent topology. Graphical Splinterm processes can disconnect and be replaced without ending those sessions.

## New work and existing work

Commandless desktop/SUPER+ENTER, bare `splinterm launch`, picker **New**, and in-Window **New Terminal** use `multiplexer.persistent-by-default` (`yes` by default). With `no`, the Window owns the fresh unnamed Lair and closing it terminates and removes the complete Lair. Explicit names, native command-bearing launches, presets, restore/relaunch, remote creation, automation, and MCP remain persistent.

Creating another Dojo or explicitly naming/renaming a Dojo promotes the complete transient Lair when `multiplexer.persist-on-tab-organization=yes` (the default). Promotion is permanent. Set it to `no` to retain Window ownership even for organized or multi-tab Lairs. An XDG launch carrying a command remains client-bound regardless of configuration. Unpromoted transient Lairs are never saved, restored, listed in Recent Dojos, or selected by `reopen`. Reopening is intentionally separate:

```bash
splinterm dojos   # choose New Terminal or a recent running Dojo
splinterm reopen  # reopen the last remembered running Dojo
```

`splinterm sessions` remains a compatibility alias for `dojos`.

A graphical window may attach up to 32 distinct Dojos as local tabs, including Dojos from different Lairs. Opening a Dojo already present in that window activates its tab instead of duplicating it. **Ctrl+Shift+B** hides or restores the local tab strip without changing the tab set; the choice lasts only for that Window.

## Closing different things

These actions have deliberately different effects:

| Action | Result |
| --- | --- |
| Close a persistent window | Detaches its local tabs and views |
| Close a Window owning an unpromoted transient Lair | Terminates its processes and removes the Lair |
| Close a transient XDG command window | Terminates every process and removes its complete Lair |
| Close a tab | Detaches that Dojo from this window |
| Close other tabs | Detaches every other local tab without ending their Dojos |
| Terminate Dojo from a tab menu | Confirms, then ends the Dojo's pane processes |
| Terminate a live Splint | Ends its process after explicit confirmation |
| Close an exited Splint | Removes the exited pane from topology |
| Close a Dojo | Removes it only when all affected Splints have exited |
| Restore | Starts a new process incarnation from saved metadata |

## Durable metadata

Metadata is stored under `$XDG_STATE_HOME/splinterm/`, falling back to `$HOME/.local/state/splinterm/`. Writes use an owner-only atomic primary and a previous-generation backup.

Splinterm persists identities, names, layout, focus hints, lifecycle state, and reviewed launch metadata for persistent Lairs. Transient XDG command Lairs are filtered from every durable projection. Splinterm does **not** persist terminal and scrollback bodies, clipboard data, PTY handles, grants, controller tokens, or transient owner leases.

Startup quarantines malformed metadata and never executes a saved command automatically.

## Reset all sessions

Use the guarded command instead of moving state files manually:

```bash
splinterm reset       # interactive confirmation
splinterm reset --yes # explicit unattended confirmation
```

Reset moves the complete session database to a timestamped backup, restarts the canonical user service, waits for its configured socket, and leaves policy and configuration untouched.

## Tab controls

The visible tab strip supports activation, detach-only close, and a `+` action for Recent Dojos. Right-click a tab for its menu, titled with the tab's Dojo: Rename tab…, Activate tab, and New Dojo (in its Lair); then the detach-only Close tab and Close other tabs; then the confirmed Terminate Dojo…, last. Opening the menu does not activate the target tab first. In the Explorer, a Dojo's menu lists Open, Rename…, Restore…, Close tab and Terminate Dojo…, in that order, and a Lair's ends with Terminate Lair….

The command palette separates the hierarchy: **Choose Lair** lists Lairs and switches through the selected Lair's most recently used Dojo, while **Choose Dojo** lists only Dojos in the active Lair. **Recent Dojos** remains a global cross-Lair list.

These are trusted application controls. Terminal content cannot paint, rename, or activate them.

## Remote Dojos

A configured SSH profile can open its own Recent Dojos picker and native Window:

```bash
splinterm --remote PROFILE
splinterm --remote PROFILE reopen
```

Without a terminal, the **Splinterm Remotes** app-menu entry, the **Remote Dojos** desktop action, `splinterm remotes`, or the **Remote Dojos…** row of local Recent Dojos opens one Window that lists your profiles, connects, lists that host's Dojos, and becomes the remote terminal. **Open Remote…** in the command palette shows the profiles as a pop-up and opens the chosen one in exactly one new Window.

Remote and local recency are kept separate. Losing the SSH connection closes local views and releases controllers without terminating daemon-owned remote processes. See [Remote access](/docs/remote/).

:::caution
Reset affects the complete persistent topology. Inspect what is running before confirming it.
:::
