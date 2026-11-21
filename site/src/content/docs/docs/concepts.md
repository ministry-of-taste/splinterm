---
title: Core concepts
description: How Splinterm separates persistent sessions (Lairs, Dojos, Splints) from disposable Wayland windows.
---

Most terminals tie your shell to the window on screen: close the window and the shell dies. Splinterm splits those two jobs apart. A background daemon (`splinterd`) owns your sessions and processes; Wayland windows are just lightweight views into that state.

Splinterm uses three core layers for what persists—collectively your **Topology**—and two names for the temporary views on your desktop.

## What persists in `splinterd`

```text
Topology
└── Lair: project atlas
    ├── Dojo: editor
    │   ├── Splint: nvim
    │   └── Splint: tests
    └── Dojo: services
        └── Splint: api server
```

### Lair
A named workspace or project—typically one per repo or area of work. A Lair groups related Dojos (and can remain empty).

### Dojo
A single persistent layout inside a Lair. A Dojo is a split-pane tree (horizontal and vertical splits) whose leaves are Splints. Think of it like a saved workspace tab that keeps living even when no window is showing it.

### Splint
One individual terminal pane. Every Splint has a stable ID, its own scrollback and terminal state, saved launch metadata, and a running (or exited) process.

### Topology
The full catalog of Lairs, Dojos, Splints, layouts, and metadata managed by `splinterd`.

## What stays disposable on screen

### Window
A native Wayland window managed by your compositor (see [Why native Wayland?](/docs/wayland/)). A window can display one or more Dojos, and closing it only disconnects your view—it doesn't kill the shells underneath.

### Tab
A window-local pointer to a daemon-owned Dojo. Tabs live only inside that window: you can show Dojos from different Lairs side by side as tabs, reorder them, or close a tab to detach it without affecting the running Dojo.

:::tip[Want throwaway terminals too?]
Ordinary unnamed terminals persist by default, but you can switch them to close-with-window behavior (`persistent-by-default=no`) or auto-clean untouched shells (`close-unused=yes`). See [Sessions and persistence](/docs/sessions/) and [Configuration](/docs/configure/configuration/).
:::

## Lifecycle terms you'll see in the CLI and UI

- **Attach** — connect a window or client to a running Dojo or Splint.
- **Detach** — close a view or tab without stopping the underlying process.
- **Restore** — explicitly start a fresh process in an exited Splint using its saved working directory and command metadata.
- **Incarnation** — a single process run inside a Splint. Each time a Splint is restored or relaunched, its incarnation number increments while its Splint ID stays the same (useful for scripts and agents so they never send input to the wrong process).
- **Controller** — the one client currently allowed to send keyboard input or resize a Splint. Many clients can watch at once, but only one controls it at a time.

Keeping persistent sessions separate from graphical windows is what makes everything else in Splinterm click: you can switch machines over SSH, close windows freely, or let an authorized script watch a build pane—without mixing session state with window management.
