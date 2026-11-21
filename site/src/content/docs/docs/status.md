---
title: Current status
description: What shipped in Splinterm 0.2.0, what is validated today, and where the platform boundaries sit.
---

Splinterm **0.2.0** is the current stable release, published on 2026-11-03 for **x86_64 Omarchy and Arch Linux** running native Wayland under Hyprland. Source code, [GitHub release assets](https://github.com/ministry-of-taste/splinterm/releases/tag/v0.2.0), documentation, and both AUR package bases (`0.2.0-1`, including optional MCP split packages) are public.

### What's new in 0.2.0

- **Live daemon upgrade** from 0.2.0 onward so package updates keep running shells alive (upgrading from 0.1.x to 0.2.0 is a one-time restart that ends existing shells).
- **Easy agent access:** Workspace-scoped **Look**, **Look and type**, and **Manage** grants with click-to-arm consent prompts, one-command `splinterm mcp setup`, and `splinterm access` management.
- **Explicit agent status:** Live status indicators in the Lair Explorer and on Dojo tabs via `splinterm agent-status` or MCP.
- **Startup Dojo picker** (`main.startup=dojo-picker`) and graphical **remote launcher** (`splinterm remotes` / **Open Remote…**).
- **Omarchy 4 integration:** Live tracking of `foot.ini`, `colors.toml`, `shell.toml`, Glassy Windows, and Omarchy text sizes.

## Capability summary

| Area | Status |
| --- | --- |
| Native Wayland client | Validated on Hyprland (keyboard, pointer, clipboard, IME, fractional scaling, damage-driven CPU/SHM rendering) |
| Persistent sessions, layouts, and explicit restore | Implemented and validated |
| Transient XDG command windows & `persistent-by-default=no` | Implemented and validated |
| Window-local Dojo tabs, Explorer, and context menus | Implemented and validated |
| Configurable keymaps, searchable help, and Lair controls | Implemented and non-graphically validated; packaged graphical acceptance pending |
| Dojo presets and collision-safe Omarchy Bash helpers | Implemented and validated |
| Vi copy mode and trusted local field editing | Implemented and validated |
| Multi-client controller transfer | Implemented and validated |
| [Native remote graphical windows](/docs/remote/) | Validated over profile-bound OpenSSH; remote image transfer excluded |
| [JSON/NDJSON automation & SSH stdio relay](/docs/automation/) | Implemented and validated |
| [Optional MCP adapter](/docs/mcp/) | Validated split package (`splinterm-mcp-bin` / `splinterm-mcp`) |
| Easy agent access | Released in 0.2.0 (Look / Look and type / Manage grants, visible access, revocation) |
| Explicit agent status | Released in 0.2.0 (VM-accepted icons and focus-gated spinner in Explorer and tabs) |
| Live daemon upgrade | Validated from 0.2.0 onward (Nix installs still restart in 0.2.0) |
| Terminal images (Sixel, static Kitty, inline iTerm2 PNG) | Documented supported subsets (local sessions only) |
| Arch / Omarchy packaging | GitHub releases and AUR `0.2.0-1` (`splinterm-bin`, `splinterm`, and MCP split packages) |
| NixOS flake & module | Source-built 0.2.0 flake/module with headless checks; wider graphical acceptance separate |
| Other Linux distros / compositors / support lifetime | Not promised |

## Honest boundaries

- **Narrow validated target:** Daily-driver validation covers x86_64 Omarchy and Arch Linux under Hyprland. Future `0.x` releases may still adjust interfaces with documented migration, and no long-term support lifetime is promised yet.
- **AUR can offer prereleases:** The `splinterm-bin` and `splinterm` AUR packages advance to release candidates between stable releases—check the version your helper offers before confirming.
- **Security-conscious, not a sandbox:** Automation is gated by binary identity, scopes, limits, controller leases, and untrusted-output separation, keeping cooperating tools inside approved bounds. It does not isolate hostile code already running as your Linux account.
- **No process checkpointing across daemon stops:** Stopping `splinterd` ends its child shells; saved metadata lets you explicitly **restore** layouts, never resume frozen application memory.
- **AT-SPI scope:** Accessibility covers navigation and status chrome—not full terminal-grid screen-reader support.
- **Topology is separate from window management:** Creating or editing a Dojo via CLI or MCP never focuses, moves, or tiles Hyprland windows.

## Release provenance (`v0.2.0`)

<details>
<summary>Exact commit, CI runs, manifest digest, and AUR base commits</summary>

- **Source commit (`main`):** `1f129ff88c1cfdcd498ec44eed38b3328d9c94a1`
- **Merged-commit CI run:** `41226461095`
- **Candidate build run:** `41523853666` (built once)
- **Protected promotion run:** `41578629716` (published exact artifacts without rebuilding; verified all 5 assets)
- **Manifest SHA-256:** `42a7012051ba5422b33c1ac0f9fbcb35d786ab99634a45b58c8daa4ab953e917`
- **AUR `0.2.0-1` (verified 2026-11-08):** source base `5474f70f19c43e26dc709838a17e02e32c3a4400`, prebuilt base `de99d4f3e88f2bb5f0123bcc1b6f0ba5f7f7f66c`
- **Graphical acceptance note:** Feature acceptance was recorded in the Omarchy VM on earlier builds of the same features rather than a fresh final installed-package matrix.

</details>

For the full repository-authoritative ledger, see [`docs/status.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/status.md) and the [public roadmap](/docs/roadmap/).
