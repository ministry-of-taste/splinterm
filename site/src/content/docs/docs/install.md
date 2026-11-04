---
title: Installation
description: Install or update Splinterm on Arch Linux and Omarchy.
---

The validated installation target is **x86_64 Omarchy/Arch Linux with native Wayland under Hyprland**. **0.2.0 is the current stable release**; future release candidates remain prereleases. Stable 0.2.0 does not promise broader platform compatibility or a support lifetime. Future 0.x releases may change interfaces with documented migration. Live daemon upgrade is supported from 0.2.0 on, but the one-time 0.1.x to 0.2.0 upgrade restarts the daemon and ends its shells. Save your work and perform that upgrade from a terminal not owned by `splinterd`.

## Install the prebuilt AUR package

**AUR is prerelease-capable, not stable-only.** The same package names can advance
to release candidates and offer them as upgrades over stable 0.2.0. Both AUR
bases still offer `0.1.1-1`; 0.2.0 distribution is pending. Check the offered
version before confirming installation or upgrades. For stable 0.2.0 specifically,
use its verified release assets or the packaging guide's exact-tag build:

https://github.com/ministry-of-taste/splinterm/blob/main/docs/packaging.md

Install the recommended [splinterm-bin AUR package](https://aur.archlinux.org/packages/splinterm-bin). It downloads verified prebuilt x86_64 binaries and resolves the `xdg-terminal-exec` AUR dependency without compiling Splinterm locally:

```bash
yay -S splinterm-bin
```

The optional policy-scoped MCP adapter is a separate exact-version prebuilt package:

```bash
yay -S splinterm-mcp-bin
```

Source-built `splinterm` and `splinterm-mcp` packages remain available. Migrating from them prompts once to approve replacement by the conflicting `-bin` packages. `paru` may be used instead of `yay`. AUR availability does not expand the supported target or promise a support lifetime. Check the package version offered by your helper before confirming an upgrade.

## Install the current versioned release directly

For the newest published versioned package, clone the public repository and run the release installer:

```bash
git clone https://github.com/ministry-of-taste/splinterm.git
cd splinterm
./install.sh
```

The installer selects the newest published qualifying SemVer `v…` release, **including prereleases**; it is not a stable-only selector. For stable 0.2.0 specifically, use its release assets or the exact-tag build instructions in the packaging guide:

https://github.com/ministry-of-taste/splinterm/blob/main/docs/packaging.md

The installer verifies the GitHub-recorded candidate-manifest digest and exact package checksums, preserves an emergency binary snapshot, warns before stopping a running daemon, installs through Pacman, and verifies the packaged client identity. The snapshot supports diagnosis and manual recovery; it is not a package-consistent rollback.

The repository and versioned release assets are public. GitHub CLI authentication is optional; the installer falls back to anonymous verified downloads.

:::caution
The default installer downloads only a published versioned release; it never selects historical `edge-*` releases or an arbitrary `main` commit. Source mode operates on a clean committed `HEAD` and does not package uncommitted worktree changes. A development checkout is not the published stable release; use the exact `v0.2.0` tag when building stable 0.2.0. Review the current worktree before using source mode.
:::

## Build from committed source

To compile and package the current committed checkout locally:

```bash
./install.sh --source
```

Include the complete package test suite with:

```bash
./install.sh --source --check
```

## What installation does not change

The installer does not:

- make Splinterm your default terminal;
- edit Omarchy or Hyprland configuration;
- enable persistent systemd user lingering; or
- opt a fresh installation into the optional MCP package.

## Recommended on Omarchy

Omarchy's Hyprland rule that marks windows as terminals does not list Splinterm. Until it does, Omarchy's universal `Super+C` sends `Ctrl+C` (interrupt) and `Super+V` sends `Ctrl+V` to Splinterm instead of copying and pasting, and Omarchy's terminal transparency and `Ctrl+Space` input-method rules skip it. Run this once to fix that:

```bash
splinterm integration omarchy enable
splinterm integration omarchy status
```

It adds the `terminal` tag through a small user-owned Hyprland module, makes Splinterm your default terminal, and runs the Omarchy screensaver in Splinterm. `splinterm integration omarchy disable` reverses all of it exactly. The package install message recommends it, and the first Splinterm Window under Omarchy 4 shows one desktop notification until the tag is present; set `omarchy-integration-hint=no` under `[main]` in `config.ini` to silence it.

Continue to the [quickstart](/docs/quickstart/) after installation. If an MCP host needs bounded access, follow the separate [MCP adapter setup](/docs/mcp/); installing the adapter alone grants no authority.
