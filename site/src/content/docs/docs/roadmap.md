---
title: Roadmap
description: Where Splinterm is headed from stable 0.2.0 toward a supported 1.0 release.
---

Splinterm's roadmap uses **Now / Next / Later / Explore** horizons instead of calendar dates. These describe the outcomes we're working toward—not delivery deadlines or compatibility guarantees.

For what works today, check [Current status](/docs/status/). For the full strategic write-up, see [`docs/product-roadmap.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/product-roadmap.md). Maintainer task tracking lives outside the public repo; accepted architectural decisions land in `docs/adr/` and these docs.

## Now: daily-driver confidence

*Current focus (0.2.x on x86_64 Omarchy / Arch Linux)*

Make session persistence intuitive, desktop integration seamless, and upgrades boring on the platform Splinterm validates today:

- Clear, recognizable Lair states (named, pinned, transient, restorable, expired) and predictable save/pin/restore/delete controls—still without saving terminal bodies or secrets.
- Rock-solid Omarchy theme fidelity and desktop integration.
- Continued validation of local file-drop path insertion and clipboard PNG saving (remote image transfer stays excluded).
- Smoother install, upgrade, recovery, diagnostics, and agent-consent flows.
- Ongoing startup and memory-footprint tracking with explicit triage for regressions.

**How we know we're there:** A new user can install Splinterm, organize real work, close windows freely, return to their sessions, and predict every destructive action without asking the maintainer.

## Next: a real 1.0 support contract

Calling a release `1.0` means committing to an explicit, testable relationship with users—not just hitting a feature count:

- Clear platform support matrix, release channels, compatibility windows, and breaking-change rules.
- Tested upgrade, rollback, reset, and recovery runbooks.
- Stable human workflows, `config.ini` surface, JSON/NDJSON schemas, and Arch packaging contracts.
- Straightforward issue-reporting and security-reporting processes with realistic expectations.
- Clear resource limits, diagnostics, and failure messages treated as ordinary product UX.

## Later: one connected workspace

Once the local daily driver is rock-solid, make local, remote, headless, and agent-assisted work feel like one cohesive environment:

- Polished remote SSH profiles, connection diagnostics, and smooth recovery after drops.
- Stable integration kits and reference recipes for scripts and MCP hosts.
- Clear visual distinction between human and automated activity inside shared sessions.
- Portable, declarative workspace definitions that never execute arbitrary shell scripts.

Even here, Splinterm won't add a public daemon network listener, a cloud account or hosted control plane, synced secrets, or simultaneous multi-user typing.

## Explore: broader Linux support

Directions we're open to investigating—not commitments:

- Broader NixOS and Home Manager deployment acceptance beyond the 0.2.0 source flake/module.
- Additional Wayland compositors backed by real compatibility testing.
- Additional distro packages with proper service and upgrade handling.
- A carefully scoped extension model.
- Selective compatibility work driven by real terminal applications.

We only expand to a new platform when real users are blocked, we can test it continuously, we can state an honest support boundary, and it's worth the core work it delays.

## Deliberate non-goals

Splinterm does **not** aim for reboot-transparent process survival, arbitrary `foot.ini` compatibility, unrestricted agent access, collaborative simultaneous typing, a hosted control plane, or claiming support for distros we don't continuously test.

Splinterm is a terminal for people first. Good automation makes it more useful, without turning it into an "AI terminal."

## How decisions get made

Priorities come from release validation, real user issues, documentation feedback, and aggregate website analytics. The terminal application itself collects **no** telemetry.
