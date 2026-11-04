---
title: Troubleshooting
description: Diagnose common local Splinterm installation, daemon, session, and configuration problems.
---

This page covers the first local checks. Splinterm 0.2.0 is stable within a narrow validated Omarchy/Arch environment, so failures outside that target may not have a supported resolution.

## Check the installed command

```bash
command -v splinterm
command -v splinterd
```

On the packaged system, the trusted graphical client and daemon are adjacent under `/usr/bin`. A user-local `splinterm` earlier in `PATH` does not match the running system daemon's trusted-UI identity.

## Check the daemon

```bash
systemctl --user status splinterd.service
splinterm ping
```

Start the user service on demand if needed:

```bash
systemctl --user start splinterd.service
```

The default socket is `$XDG_RUNTIME_DIR/splinterm/splinterd.sock`. A development instance may use another path through `SPLINTERM_SOCKET`.

## The daemon restarted after a heavy workload

Check whether systemd recorded an out-of-memory result and inspect the daemon's independent control-plane limits:

```bash
systemctl --user show splinterd.service \
  -p Result -p NRestarts -p TasksCurrent -p TasksMax \
  -p MemoryCurrent -p MemoryHigh -p MemoryMax
journalctl --user-unit splinterd.service -n 50 --no-pager
```

The packaged task ceiling protects the daemon from unbounded process creation, while `MemoryHigh` causes reclaim and throttling rather than imposing a hard memory ceiling. Terminal workloads run in a separate aggregate slice with nested per-Dojo and per-Splint boundaries, so they do not share the daemon's task or memory-pressure budget. `MemoryCurrent` includes charged page cache, some of which may be reclaimable under pressure.

After a daemon restart, inspect exited topology with `splinterm list --all`. Restoration is explicit because Splinterm never reruns saved commands automatically.

## A new Splint reports an internal launch failure

The packaged daemon fails closed when it cannot place a terminal helper in its exact systemd scope before executing the shell. Inspect the daemon and workload hierarchy:

```bash
journalctl --user-unit splinterd.service -n 40 --no-pager
systemctl --user status app-splinterm.slice
systemd-cgls --user-unit app-splinterm.slice
```

The aggregate slice can be inactive when no Splints are running. Do not work around placement failure by launching the packaged daemon manually: that removes the package's required-containment contract. A direct development daemon may fall back with a bounded warning, but it does not claim workload isolation.

## A window exits as unauthorized

Check all of the following:

1. `command -v splinterm` resolves to the packaged client.
2. The running daemon executable is the packaged adjacent `splinterd`.
3. The client and daemon come from the same package build.
4. Old windows were reopened after replacing either executable.

Do not install a development client to an earlier user-local `PATH` entry and treat it as the packaged desktop client.

## Configuration fails at startup

Run with the intended file and read the line-numbered diagnostic:

```bash
SPLINTERM_CONFIG=/path/to/config.ini splinterm launch
```

Unknown keys and malformed values fail rather than being guessed. Compare the file with the [supported configuration](/docs/configure/configuration/).

## A Dojo is missing from Recent Dojos

The native picker opens only Dojos whose complete pane layout is still running. Exited Splints remain in persistent metadata, but starting them again requires explicit restore.

Inspect active and exited topology through the human CLI:

```bash
splinterm list
splinterm list --all
```

## Automation is denied

Socket access, the same Unix account, SSH login, or running inside a Splint does not grant automation authority. Persistent automation requires a valid owner-controlled policy for the exact executable identity, operation scopes, resources, and limits.

A controller denial can also be normal: one client may hold a Splint's exclusive input/resize lease while others continue observing.
