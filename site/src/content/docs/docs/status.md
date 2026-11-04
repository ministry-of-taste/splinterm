---
title: Current status
description: What is implemented, validated, limited, planned, and unreleased in Splinterm.
---

Splinterm **0.2.0 is the current stable release**, published on 2026-11-03 at 04:43 UTC (2026-11-02 in the maintainer's local time) for x86_64 Omarchy/Arch Linux with native Wayland under Hyprland. It is GitHub Latest, not a prerelease. Earlier stable releases remain historical. Source, documentation, and GitHub releases are public; AUR distribution of 0.2.0 is pending. Broader platform compatibility and a support lifetime are not promised; future 0.x releases may change interfaces with documented migration.

Stable release notes and assets:

https://github.com/ministry-of-taste/splinterm/releases/tag/v0.2.0

Stable 0.2.0 adds live daemon upgrade from 0.2.0 on, easy agent access,
explicit agent status, the startup picker, remote launch from inside the app,
and Omarchy 4 theme integration. AT-SPI remains navigation/status only, not
terminal bodies or whole-terminal screen-reader support; remote image transfer
is not included.

The release came from `main` commit
`1f129ff88c1cfdcd498ec44eed38b3328d9c94a1`. Merged-commit CI run `41226461095`
passed. Candidate run `41523853666` built the packages once; protected promotion
run `41578629716` published those exact artifacts without rebuilding and verified
all five assets. Manifest SHA-256:
`42a7012051ba5422b33c1ac0f9fbcb35d786ab99634a45b58c8daa4ab953e917`.
Both AUR package bases still offer `0.1.1-1`, including matching optional MCP
packages; 0.2.0 distribution has not happened yet. No new graphical sequence
was run for finalization; feature acceptance was recorded in the Omarchy VM on
earlier builds of the same features, not a fresh final installed-package matrix.
The current development tip may differ from the immutable release.

## What that means

- The product runs and has a normal graphical daily-use path.
- Core terminal, persistence, multiplexing, packaging, and automation milestones have recorded validation.
- The current target is narrow: x86_64 Omarchy/Arch Linux with native Wayland.
- Install 0.2.0 from its GitHub release or exact-tag source checkout; AUR still offers 0.1.1-1 until distribution lands.
- Broader distribution and long-term compatibility promises have not been released.

## Capability summary

| Area | Current state |
| --- | --- |
| Native Wayland presentation | Keyboard, pointer, clipboard, IME, scaling, and damage-driven rendering validated on the documented Hyprland target |
| Persistent sessions and explicit restore | Implemented and validated |
| XDG command lifecycle | Commandless launches remain persistent; command-bearing launches use trusted client-bound transient Lairs |
| Pane layouts and multiple Dojos | Implemented and validated |
| Window-local Dojo tabs, tab strip, and context menus | Implemented and validated |
| Configurable keymaps, searchable help, and current-Lair controls | Implemented and non-graphically validated; packaged graphical acceptance pending |
| Atomic Dojo presets and optional Bash helpers | Implemented and validated |
| Vi copy mode and trusted local field editing | Implemented and validated |
| Multi-client controller transfer | Implemented and validated |
| JSON/NDJSON automation | Implemented and validated |
| SSH stdio relay | Implemented and validated |
| [Native remote graphical client](/docs/remote/) | Implemented and validated with profile-bound OpenSSH transport; remote image transfer is excluded |
| [MCP adapter](/docs/mcp/) | Implemented and validated as an optional, separately policy-identified package |
| Agent status reporting | Released in 0.2.0; accepted in the Omarchy VM: explicit reports through `splinterm agent-status` and the MCP adapter, shown in the Explorer and on Dojo tabs as icons, with a working spinner that moves only in the focused Window and holds still with reduced motion |
| Easy agent access | Released in 0.2.0; workspace-scoped Look, Look and type, or Manage grants with deliberate consent, visible access, and revocation |
| Live daemon upgrade | Implemented and validated from 0.2.0 on; the one-time 0.1.x to 0.2.0 upgrade restarts the daemon and ends its shells |
| Sixel, practical Kitty static images, inline iTerm2 PNG | Documented supported subsets |
| Arch/Omarchy package | Versioned GitHub release and AUR packages validated |
| Public source and versioned builds | Available |
| [AUR packages](https://aur.archlinux.org/packages/splinterm-bin) | 0.1.1-1 available; 0.2.0 distribution pending. Prebuilt `splinterm-bin`, source-built `splinterm`, and optional MCP split packages; this channel can include release candidates, not only stable releases |
| Stable release | 0.2.0 for the documented platform |
| Broader compatibility and support lifetime | Not promised |
| Nix and broader distributions | 0.2.0 release flake/module and headless checks; wider deployment acceptance and broader Linux support remain separate |

## Important boundaries

Splinterm is **security-conscious**, not absolutely secure. [Automation](/docs/automation/) is constrained by executable identity, explicit scopes, resource limits, controller ownership, revocation, and bounded audit metadata. Terminal output is always untrusted data and cannot grant authority.

**Live daemon upgrade is supported from 0.2.0 on.** The one-time 0.1.x to 0.2.0 upgrade restarts the daemon and ends its shells; the private protocol versions cannot communicate. Save your work and perform that upgrade from another terminal, then reopen Splinterm Windows. Nix installations still require a restart in 0.2.0. Stopping the daemon ends its child processes; saved topology does not checkpoint applications. See [Installation](/docs/install/).

Persistent topology is also separate from graphical presentation. Creating or mutating a Dojo does not map, focus, move, or resize a native Wayland window. Read [Why native Wayland?](/docs/wayland/) for the direct-compositor benefits, comparison model, and explicit non-claims.

## Before depending on it

Review the [public roadmap](/docs/roadmap/) and the exact specialist documentation for the feature you intend to use. Stable 0.2.0 does not expand the documented compatibility scope; repository [`docs/status.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/status.md) remains authoritative.
