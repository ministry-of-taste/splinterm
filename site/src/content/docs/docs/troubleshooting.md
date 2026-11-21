---
title: Troubleshooting
description: Quick checks for installation paths, splinterd health, authorization errors, config failures, and missing Dojos.
---

Start here when something local isn't behaving as expected. Keep in mind that Splinterm 0.2.0 is validated on x86_64 Omarchy and Arch Linux under Hyprland—issues outside that environment may not have a supported fix yet.

## Check which binaries are on `PATH`

```bash
command -v splinterm
command -v splinterd
```

On a packaged install, both should resolve under `/usr/bin/`. A development binary earlier in `PATH` (such as `~/.local/bin/splinterm` or `~/.cargo/bin/splinterm`) won't match `/usr/bin/splinterd`'s trusted graphical client check and will be rejected.

## Check the daemon

```bash
systemctl --user status splinterd.service
splinterm ping
```

If the user service isn't running yet, start it:

```bash
systemctl --user start splinterd.service
```

The default Unix socket lives at `$XDG_RUNTIME_DIR/splinterm/splinterd.sock` (development runs via `./splinterm-test` override this with `SPLINTERM_SOCKET`).

## A window exits as unauthorized

For a fresh graphical launch, `splinterd` grants trusted-UI access only when the client executable matches the `splinterm` binary sitting right beside the running `splinterd` executable (by device and inode). Check:

1. `command -v splinterm` points to `/usr/bin/splinterm`, not a shadowing user-local copy.
2. The running daemon is `/usr/bin/splinterd` from the same package build.
3. After a package upgrade replaces `/usr/bin/splinterm`, existing windows opened before the upgrade keep working while their connections stay open, and any fresh connection must come from the installed `/usr/bin/splinterm` (or the snapshot pinned by a live-upgraded daemon). If a pre-upgrade client process loses its connections and tries to reconnect—or if `splinterd` is running from a different directory than `command -v splinterm`—it will be rejected; reopen from the launcher or align the daemon (`splinterm upgrade --now` or restart `splinterd.service`).

## The daemon restarted under heavy load

Check whether systemd hit a resource limit and inspect the daemon's journal:

```bash
systemctl --user show splinterd.service \
  -p Result -p NRestarts -p TasksCurrent -p TasksMax \
  -p MemoryCurrent -p MemoryHigh -p MemoryMax
journalctl --user-unit splinterd.service -n 50 --no-pager
```

Terminal workloads run inside a separate `app-splinterm.slice` hierarchy (with nested per-Dojo and per-Splint scopes), keeping them isolated from `splinterd`'s own task and memory budget. Note that `MemoryHigh` applies memory pressure/reclaim rather than a hard OOM kill, and `MemoryCurrent` includes reclaimable page cache.

After a restart, inspect exited sessions with `splinterm list --all`. Splinterm never re-runs saved commands automatically—use `restore` explicitly on what you want back.

## A new Splint fails to launch

The packaged daemon fails closed if it can't place a new shell inside its systemd scope. Inspect the daemon logs and workload slice:

```bash
journalctl --user-unit splinterd.service -n 40 --no-pager
systemctl --user status app-splinterm.slice
systemd-cgls --user-unit app-splinterm.slice
```

It's normal for `app-splinterm.slice` to show as inactive when no Splints are running. Avoid working around placement errors by starting `/usr/bin/splinterd` manually outside systemd—that bypasses the packaged workload containment.

## Config fails at startup

Test the exact config file and read the line-numbered error:

```bash
splinterm config check
# Or test an alternate file:
SPLINTERM_CONFIG=/path/to/config.ini splinterm config check
```

Unknown sections, unknown keys, and invalid values fail explicitly instead of being guessed. Compare against [Configuration](/docs/configure/configuration/).

## A Dojo is missing from Recent Dojos

Recent Dojos lists only Dojos whose panes are still running. Exited Dojos stay in saved metadata until you restore or close them. Check both active and exited sessions:

```bash
splinterm list
splinterm list --all
```

Unpromoted transient Lairs (when `persistent-by-default=no` or launched via an XDG command) are also intentionally omitted from Recent Dojos.

## Automation or MCP calls are denied

Reaching the Unix socket, running as the same Linux user, connecting over SSH, or running inside a Splint does not grant automation access.

- Check active workspace grants and matching policy rules with `splinterm access list` and `splinterm policy inspect ~/.config/splinterm/policy.json`.
- If you upgraded `/usr/bin/splinterm-mcp`, its SHA-256 digest changed—policy rules pinning the old digest will fail closed until updated and reloaded.
- A **controller denied** error is often normal: another client (such as your open window) currently holds exclusive input/resize control of that Splint while other clients can still observe it.
