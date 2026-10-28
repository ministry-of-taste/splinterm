---
title: CLI reference
description: Find the human commands for opening, inspecting, arranging, restoring, and remotely accessing Splinterm.
---

The `splinterm` CLI serves both people and structured clients. Human output is the default. JSON and NDJSON are versioned machine contracts with separate authority rules; see [Bounded automation](/docs/automation/) before scripting them.

## Everyday graphical workflows

```bash
splinterm dojos                    # open Recent Dojos
splinterm reopen                   # reopen the last still-running Dojo
splinterm launch                   # fresh graphical Lair; lifetime follows configuration
splinterm window --lair-id L --dojo-id D
```

`splinterm sessions` remains a compatibility alias for `dojos`. A Window is a disposable view and can hold several Dojos as local tabs.

## Inspect topology

```bash
splinterm ping
splinterm list
splinterm list --all
splinterm topology
splinterm inspect SPLINT_ID
```

`list` emphasizes active Lairs. `--all` includes exited-only history. IDs returned by these commands are stable topology identities, not window handles.

## Create and arrange work

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

Arguments after `--` are executed directly as an argument vector. Splinterm does not rebuild them into shell text.

A new Lair starts with `Dojo 1`. If `new-dojo` omits `--name`, Splinterm chooses one greater than that Lair's highest exact `Dojo N` name without reusing gaps. Explicit names are preserved.

## Lifecycle

| Command | Effect |
| --- | --- |
| `kill SPLINT_ID` | End a live process but retain its Splint. |
| `restore SPLINT_ID` | Start one exited Splint from saved metadata. |
| `restore-dojo DOJO_ID` | Restore every exited Splint in a Dojo. |
| `restore-lair LAIR_ID` | Restore every exited Splint in a Lair. |
| `relaunch SPLINT_ID [-- ARGV...]` | Start a new incarnation, optionally with new argv. |
| `close SPLINT_ID` | Remove an exited Splint and collapse its layout branch. |
| `close-dojo DOJO_ID` | Remove a Dojo after every Splint has exited. |
| `reset` | Back up and clear all persistent topology, then restart cleanly. |

Restore is always explicit. Saved launch metadata is never executed automatically.

## Agent status

| Command | Effect |
| --- | --- |
| `agent-status set STATE [--detail TEXT] [--ttl SECONDS]` | Report `working`, `needs_input`, `done`, `error`, or `idle` for the current Splint. |
| `agent-status clear` | Clear the current Splint's reported status. |
| `agent-status get [SPLINT_ID]` | Read one Splint's reported status. |

Inside a Splint the target comes from `$SPLINTERM_SPLINT_ID` and `$SPLINTERM_SPLINT_INCARNATION`; `--splint ID --incarnation N` overrides it. Without either, the command exits 2 with `not inside a Splint`. See [Bounded automation](/docs/automation/) for authority and agent hook recipes.

## Local configuration and presets

```bash
splinterm config check
splinterm keymap list
splinterm keymap show
splinterm keymap conflicts
splinterm preset list
splinterm preset show NAME
splinterm preset check
splinterm preset run NAME --cwd "$PWD" --dry-run
```

Read [Configuration and keymaps](/docs/configure/configuration/) and [Dojo presets](/docs/presets/) for the strict schemas and bundled Omarchy workflows.

## Remote profiles

```bash
splinterm remote list
splinterm remote inspect PROFILE
splinterm remote check PROFILE
splinterm remotes
splinterm --remote PROFILE
splinterm --remote PROFILE reopen
```

The first two commands are local-only. `splinterm remotes` returns at once and prints nothing; it opens one Window that lists the profiles, connects to the chosen one, lists its Dojos, and becomes the remote terminal. `remote check` performs bounded, read-only SSH, relay, and daemon probes without mapping a Window. See [Remote access](/docs/remote/).

## Machine output

```bash
splinterm --output json --schema-major 2 --timeout-ms 5000 topology
splinterm --output ndjson --schema-major 2 --timeout-ms 300000 \
  subscribe terminal SPLINT_ID
```

Machine mode never prompts. It uses stable schema-major-2 envelopes, explicit timeouts, structured exit categories, resource policy, incarnation checks, and resynchronization rules. Human-readable output and private daemon frames are not compatibility contracts.

For every flag and operation, the current executable's `--help` and repository [`docs/cli.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/cli.md) remain authoritative.
