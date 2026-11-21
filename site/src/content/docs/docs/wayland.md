---
title: Why native Wayland?
description: What Splinterm gets from speaking Wayland directly, why daemon persistence matters more, and where the limits are.
---

Splinterm is a native Wayland client—it talks directly to your compositor instead of going through XWayland.

Speaking Wayland natively makes the terminal behave like a first-class citizen on a modern Linux desktop, though plenty of terminals do that today. What actually sets Splinterm apart is pairing that lightweight Wayland client with `splinterd`, a headless daemon that owns your shells and layouts underneath.

```text
Hyprland / Wayland compositor
             ↓
  disposable splinterm window
             ↓
     persistent splinterd
             ↓
   shells · layouts · scrollback
```

## What direct Wayland gives you

### Compositor-aware scaling
Splinterm reads output geometry and scale straight from Wayland. With the default `output-scale` font policy, text stays sharp across mixed-DPI monitors and fractional scaling—no blurry XWayland upscaling. See [Font sizing and Wayland scale](/docs/configure/configuration/#font-sizing-and-wayland-scale).

### Native input, clipboard, and IME
Keyboard events, pointer actions, Wayland clipboard exchange, compose sequences, and IME preedit all run natively. Splinterm reports the active terminal cell rectangle back to the compositor so IME candidate popups appear right where you're typing.

### Damage-driven rendering
Splinterm tracks which terminal regions changed and schedules redraws with compositor frame callbacks instead of repainting the whole window on every tick. (This is CPU composition over Wayland shared memory, not GPU rendering.)

### Optional compositor blur
When your active theme enables translucency and blur—and your compositor advertises the matching protocol—Splinterm requests native background blur. On compositors without that protocol, it simply runs without blur.

### Tighter client isolation than X11
Wayland doesn't give ordinary clients the global window snooping and input injection X11 historically allowed. That's a healthier baseline, though it isn't a complete security boundary on its own—Splinterm still enforces separate rules for automation policy, controller ownership, and untrusted terminal output.

## Why persistence is the bigger shift

Plenty of modern terminals already have a Wayland backend. Where they differ from Splinterm is who owns your shell:

| Setup | Display surface | Process owner | Closing the window |
| --- | --- | --- | --- |
| X11 terminal via XWayland | X11 compatibility layer | Usually the terminal process | Usually kills the shell |
| Wayland terminal | Native Wayland surface | Usually the terminal process | Usually kills the shell |
| Terminal + tmux or Zellij | Terminal plus nested multiplexer | Nested multiplexer | Multiplexer session persists |
| **Splinterm** | Disposable native Wayland surface | Headless `splinterd` | Shells and layouts keep running |

Because `splinterd` itself needs neither Wayland nor X11, your sessions keep running when every window is closed—and headless servers can run `splinterd` with no graphical stack at all.

Splinterm's terminal emulation comes from [Foot](https://codeberg.org/dnkl/foot), a fast Wayland-native terminal. Splinterm builds persistent daemon topology, built-in panes and tabs, SSH remote windows, and scoped automation on top of that foundation.

## What native Wayland does *not* promise

- **Not automatic speed.** Skipping XWayland doesn't guarantee faster rendering by itself.
- **Not GPU rendering.** Splinterm currently renders on the CPU into Wayland shared-memory buffers.
- **Not every compositor.** The validated target is x86_64 Omarchy and Arch Linux under Hyprland.
- **Not a security sandbox.** Wayland isolation helps at the windowing layer, not against arbitrary processes running as your own user.
- **Not compositor window control.** Creating or arranging a Dojo via CLI or MCP never focuses, moves, resizes, or assigns workspaces to your Hyprland windows.

See [Current status](/docs/status/) for exact platform boundaries and [Core concepts](/docs/concepts/) for how persistent topology and disposable windows fit together.
