---
title: Installation
description: Install or upgrade Splinterm on Arch Linux and Omarchy.
---

Splinterm **0.2.0** is built and validated for **x86_64 Omarchy and Arch Linux** running native Wayland under Hyprland. See [Current status](/docs/status/) for exact platform limits and NixOS notes.

## Install from AUR (recommended)

The `splinterm-bin` package downloads verified prebuilt binaries and pulls in `xdg-terminal-exec` without compiling Rust locally. Use `yay` or `paru`:

```bash
yay -S splinterm-bin
```

If you also want the optional MCP server for coding agents, install its matching split package:

```bash
yay -S splinterm-mcp-bin
```

Prefer compiling locally? Source-built `splinterm` and `splinterm-mcp` packages are on AUR as well (switching between source and `-bin` packages prompts once to replace the conflicting package).

:::note[Checking the AUR version]
The AUR packages track both stable releases and release candidates—they aren't pinned to stable-only. Check the version your helper offers before confirming an install or upgrade. To pin strictly to stable `v0.2.0` if AUR has moved ahead, follow the exact-tag instructions in the [packaging guide](https://github.com/ministry-of-taste/splinterm/blob/main/docs/packaging.md).
:::

## Recommended on Omarchy

Omarchy's default Hyprland rules don't tag Splinterm as a terminal yet. Until they do, Omarchy's universal `Super+C` sends a regular `Ctrl+C` (interrupt) instead of copying, `Super+V` sends `Ctrl+V`, and Omarchy's terminal transparency and `Ctrl+Space` input-method rules skip the window.

Run this once after installing:

```bash
splinterm integration omarchy enable
splinterm integration omarchy status
```

That adds a small user-owned Hyprland rule for the `terminal` tag, sets Splinterm as your default terminal, and runs the Omarchy screensaver inside Splinterm. Everything it touches is reversible with `splinterm integration omarchy disable`.

Until the tag is in place, the first Splinterm window on Omarchy 4 shows one desktop notification as a reminder. To silence it without enabling the integration, add `omarchy-integration-hint=no` under `[main]` in `config.ini`.

## Upgrading from 0.1.x

From **0.2.0 onward**, Splinterm supports live daemon upgrades so your running shells survive package updates.

Upgrading from **0.1.x to 0.2.0** is a one-time exception: the internal protocol changed, so the upgrade restarts `splinterd` and ends any running shells. Save your work and run that upgrade from another terminal (such as Foot) rather than inside Splinterm.

## Install with `./install.sh`

If you'd rather skip an AUR helper, clone the repository from a terminal not owned by `splinterd` and run the release installer:

```bash
git clone https://github.com/ministry-of-taste/splinterm.git
cd splinterm
./install.sh
```

The script fetches the newest published SemVer release (including prereleases), verifies the release manifest digest and package checksums, saves an emergency snapshot of the previous binaries for manual recovery, installs through Pacman, and verifies the installed client identity. GitHub CLI login is optional; anonymous downloads work out of the box.

To build and package your current committed `HEAD` locally instead:

```bash
./install.sh --source
# Or include the full package test suite:
./install.sh --source --check
```

:::caution[Source mode uses committed HEAD only]
`./install.sh --source` intentionally ignores uncommitted changes in your working tree. Check out the exact `v0.2.0` tag if you want to compile the published stable release from source rather than the tip of `main`.
:::

## What installation leaves untouched

Installing the package will never:

- change your default terminal;
- edit your Omarchy or Hyprland configuration;
- enable systemd user lingering; or
- install or authorize the optional MCP adapter.

Ready to try it? Head to the [quickstart](/docs/quickstart/). If you installed the MCP adapter, continue to [MCP adapter setup](/docs/mcp/)—installing the binary alone grants no access.
