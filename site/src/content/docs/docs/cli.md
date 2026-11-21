---
title: CLI reference
description: Everyday splinterm commands for opening windows, managing topology, controlling agent access, and scripting JSON/NDJSON output.
---

The `splinterm` CLI defaults to human-readable output. Pass `--output json` or `--output ndjson` for versioned machine contracts—see [Bounded automation](/docs/automation/) before scripting those, since machine mode uses explicit policy authority instead of human graphical trust.

## Open windows

```bash
splinterm dojos                    # open Recent Dojos (alias: sessions)
splinterm reopen                   # attach to the last running Dojo
splinterm launch                   # open a fresh graphical Lair
splinterm window --lair-id L --dojo-id D
```

## Inspect running state

```bash
splinterm ping                     # check daemon reachability
splinterm list                     # active Lairs
splinterm list --all               # include exited history
splinterm topology                 # full Lair/Dojo/Splint tree
splinterm inspect SPLINT_ID        # details for one Splint
```

IDs printed by these commands are stable topology UUIDs, not compositor window handles.

## Create and arrange workspaces

```bash
splinterm new project --cwd "$HOME/src/project"
splinterm new-dojo LAIR_ID --name logs -- /usr/bin/journalctl -f
splinterm split SPLINT_ID --axis vertical --side second --ratio 600
splinterm ratio SPLINT_ID 500
splinterm rename-lair LAIR_ID NAME
splinterm rename-dojo DOJO_ID NAME
splinterm rename-splint SPLINT_ID TITLE
splinterm rename-splint SPLINT_ID --use-app-name
```

Anything after `--` runs directly as an argument vector—Splinterm never re-quotes it through a shell.

New Lairs start with `Dojo 1`. Omitting `--name` on `new-dojo` picks one higher than the Lair's highest `Dojo N` without reusing deleted numbers.

## Lifecycle

| Command | Effect |
| --- | --- |
| `kill SPLINT_ID` | Stop a running process while keeping its Splint and metadata. |
| `restore SPLINT_ID` | Start a new incarnation of one exited Splint from saved metadata. |
| `restore-dojo DOJO_ID` | Restore all exited Splints in a Dojo. |
| `restore-lair LAIR_ID` | Restore all exited Splints in a Lair. |
| `relaunch SPLINT_ID [--cwd DIR] [-- ARGV...]` | Start a new incarnation of an exited Splint, optionally replacing its directory or argv. |
| `close SPLINT_ID` | Remove an exited Splint and collapse its split. |
| `close-dojo DOJO_ID` | Remove a Dojo once all of its Splints have exited. |
| `upgrade [--now]` | Live-upgrade `splinterd` in place after a package update without ending running shells (from 0.2.0 onward). |
| `reset` | Back up and wipe all persistent topology, then restart `splinterd`. |

Restoring is always explicit; Splinterm never re-runs saved commands automatically.

## Agent status

| Command | Effect |
| --- | --- |
| `agent-status set STATE [--detail TEXT] [--ttl SECONDS]` | Report `working`, `needs_input`, `done`, `error`, or `idle` for a Splint. |
| `agent-status clear` | Clear the reported status. |
| `agent-status get [SPLINT_ID]` | Read one Splint's current status. |

Inside a Splint, the target defaults to `$SPLINTERM_SPLINT_ID` and `$SPLINTERM_SPLINT_INCARNATION`. Outside one, pass `--splint ID --incarnation N` (without either, the command exits `2` with `not inside a Splint`). See [Bounded automation](/docs/automation/) for Claude Code and Codex hook recipes.

## Agent access and MCP setup

| Command | Effect |
| --- | --- |
| `access list` | Show active workspace grants, pending access requests, and matching policy rules. |
| `access revoke ID [--yes]` | Revoke one runtime grant or remembered `ra_` entry. |
| `access revoke --all [--dry-run] [--yes]` | Revoke every consent grant (including remembered `ra_` grants) and pending request (leaves `policy.json` untouched). |
| `approvals` | Answer pending agent access prompts from a terminal on headless hosts (refused inside a Splint or requesting agent's process tree). |
| `mcp setup claude-code\|codex\|vscode [--dry-run] [--replace]` | Register `/usr/bin/splinterm-mcp` in the host's config (grants no access by itself; VS Code form is not yet verified against a real install). |

## Configuration, keymaps, and presets

```bash
splinterm config check
splinterm keymap list
splinterm keymap show
splinterm keymap show omarchy-tmux
splinterm keymap conflicts
splinterm preset list
splinterm preset show NAME
splinterm preset check
splinterm preset run NAME --cwd "$PWD" --dry-run
```

See [Configuration](/docs/configure/configuration/) and [Dojo presets](/docs/presets/) for file schemas and Omarchy workflows.

## Remote profiles

```bash
splinterm remote list
splinterm remote inspect PROFILE
splinterm remote check PROFILE
splinterm remotes
splinterm --remote PROFILE
splinterm --remote PROFILE reopen
```

`remote list` and `remote inspect` are strictly local. `remote check` runs a read-only SSH, relay, and daemon probe without opening a window. `splinterm remotes` launches the graphical profile chooser and returns immediately. See [Remote access](/docs/remote/).

## Machine output

```bash
splinterm --output json --schema-major 2 --timeout-ms 5000 topology
splinterm --output ndjson --schema-major 2 --timeout-ms 300000 \
  subscribe terminal SPLINT_ID
```

Machine mode never prompts interactively and emits versioned `splinterm.cli.v2` envelopes with explicit timeouts, structured exit codes, incarnation checks, and stream resync notices. Human-readable output and private daemon frames are not stable APIs.

For every flag and subcommand, run `splinterm --help` or check [`docs/cli.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/cli.md).
