---
title: Configuration
description: Configure Splinterm fonts, Omarchy theme integration, session lifetime, Explorer, keymaps, and clipboard behavior.
---

Splinterm reads its configuration from:

```text
${XDG_CONFIG_HOME:-~/.config}/splinterm/config.ini
```

Start from the commented example in `config/splinterm/config.ini`. To test an alternate file without replacing your main config, run with `SPLINTERM_CONFIG=/path/to/config.ini`.

Splinterm parses strictly: unknown sections, unknown keys, and malformed values fail with a line-numbered message rather than silently falling back. Run `splinterm config check` anytime to validate your file.

## Starter example

```ini
[main]
# Leave font and size unset to follow Omarchy's monospace font and text size live.
# font=JetBrains Mono Nerd Font:style=Regular
# font-pixelsize=14
font-sizing-policy=output-scale
padding-left=12
padding-right=12
padding-top=12
padding-bottom=12
initial-columns=80
initial-rows=24
login-shell=yes

[scrollback]
lines=1000

[multiplexer]
persistent-by-default=yes
persist-on-tab-organization=yes
tab-strip-border=yes
close-unused=no
divider-style=line
pane-gap=0
frame-title=splint

[cursor]
style=block
blink=yes

[explorer]
agent-status=yes
status-colors=yes
status-glyphs=icons

[key-bindings]
profile=splinterm
# file=keybindings.toml
# prefix-timeout-ms=1000

[presets]
# file=presets.toml
allow-unrestricted-commands=no
```

## Settings reference

### `[main]`, `[scrollback]`, and `[cursor]`

| Key | Purpose | Default / range |
| --- | --- | --- |
| `main.font` | Fontconfig pattern; unset follows fontconfig `monospace` live | Fontconfig `monospace` Regular |
| `main.font-pixelsize` | Terminal font size in pixels (opts out of Omarchy text size) | `6`–`96`; Omarchy text size, else `14` |
| `main.font-point-size` | Terminal font size in points (mutually exclusive with pixelsize) | `6`–`96`; unset |
| `main.font-sizing-policy` | `output-scale` or `physical-dpi` | `output-scale` |
| `main.ui-font-pixelsize` | Text size for tabs, Explorer, menus, palette, and pickers | `6`–`96`; Omarchy text size, else `14` |
| `main.startup` | `new-dojo` or `dojo-picker` for commandless desktop launches | `new-dojo` |
| `main.omarchy-integration-hint` | Show a one-time reminder under Omarchy 4 if the `terminal` tag is missing | `yes` |
| `main.padding-*` | Independent logical padding on `left`, `right`, `top`, `bottom` | `0`–`10000`; `12` |
| `main.initial-columns` / `initial-rows` | Initial grid dimensions | `2`–`480` (`80`) / `2`–`128` (`24`) |
| `main.shell` | Shell executable for an empty launch | Account login shell |
| `main.theme` | Explicit Splinterm JSON theme path (opts out of Omarchy auto-discovery) | unset |
| `scrollback.lines` | Daemon scrollback budget per Splint | `0`–`1,000,000`; `1000` |
| `cursor.style` / `cursor.blink` | `block`, `beam`, or `underline`; blink `yes`/`no` | `block` / `yes` |

### `[multiplexer]`

| Key | Purpose | Default / range |
| --- | --- | --- |
| `persistent-by-default` | Whether unnamed local graphical launches persist (`yes`) or close with their window (`no`) | `yes` |
| `persist-on-tab-organization` | Adding or explicitly naming a Dojo promotes a transient Lair to persistent | `yes` |
| `close-unused` | Close Tab and window close terminate Dojos that were never used | `no` |
| `new-dojo-directory` | Starting directory for new Dojos/Lairs: `inherit`, `home`, absolute path, or `~/path` | `home` |
| `new-splint-directory` | Starting directory for new splits (same choices) | `inherit` |
| `tab-strip-border` | Draw a thin full-width bottom rule under the tab strip | `yes` |
| `divider-style` | `line`, `frame`, or `none` | `line` |
| `pane-gap` | Logical px gap between panes and around window edges; shrinks if space is tight | `0`–`64`; `0` |
| `frame-title` | Show `splint` titles on framed panes or `none` | `splint` |

### `[explorer]`, `[integration]`, `[clipboard]`, `[key-bindings]`, `[presets]`

| Key | Purpose | Default |
| --- | --- | --- |
| `explorer.agent-status` | Show reported agent status in the Explorer and on Dojo tabs | `yes` |
| `explorer.status-colors` | Color status glyphs and the `Asking` marker from the theme (`no` uses plain text color) | `yes` |
| `explorer.status-glyphs` | `icons` (two-cell icons) or `dots` (one-cell geometric shapes) | `icons` |
| `explorer.current-marker` | Marker icon before the active Lair, Dojo, and Splint (e.g. `eye`, `crosshair`, `focus-2`) | `eye` |
| `explorer.full-height` | Dock the Explorer full window height beside the tab strip (`no` places it below) | `yes` |
| `explorer.collapsed` | Start the Explorer collapsed to its compact icon rail | `no` |
| `explorer.row-lines` | `1` (single line) or `2` (adds agent detail or working directory under each Splint) | `1` |
| `integration.dialog-shortcut` | Shortcut to engage a waiting trusted dialog's keyboard without answering (`ctrl+alt+<letter-or-digit>` or `none`; also bound globally under Hyprland while a dialog waits) | `ctrl+alt+g` |
| `integration.remote-agent-prompts` | Show remote host agent-access prompts in your `splinterm --remote` window | `yes` |
| `clipboard.image-directory` | Private `0700` directory (no symlinks) for saving pasted clipboard PNGs via the palette | unset |
| `clipboard.middle-click-paste` | Paste primary selection on middle click (`no` passes middle click to mouse-aware apps) | `yes` |
| `clipboard.osc52` | Terminal OSC 52 clipboard access: `copy-paste` (programs in a controlled pane, including SSH sessions and agents, can read your clipboard without asking), `copy` (write-only), or `off` | `copy-paste` |
| `key-bindings.profile` | Built-in keymap profile: `splinterm` or `omarchy-tmux` | `splinterm` |
| `key-bindings.file` | Optional strict TOML overlay relative to `config.ini` | unset |
| `key-bindings.prefix-timeout-ms` | Prefix timeout for `omarchy-tmux` | `250`–`5000`; `1000` |
| `presets.file` | Optional strict TOML preset catalog | unset |
| `presets.allow-unrestricted-commands` | Opt into the bundled `c`, `cx`, and `cy` unrestricted agent aliases | `no` |

Optional `[colors]`, `[colors-dark]`, and `[colors-light]` sections also let you override `alpha`, `blur`, tab/Explorer backgrounds, and individual chrome roles (`accent`, `focus-ring`, `text-muted`, `warning`, etc.) while keeping the rest of your Omarchy theme live. See `config/splinterm/config.ini` for the full commented list.

**Live reload:** **Reload configuration** applies `main.ui-font-pixelsize`, `[colors*]` overrides, every `[multiplexer]` key except `persistent-by-default`, all `[explorer]` keys, `integration.remote-agent-prompts`, all `[clipboard]` keys, and `[key-bindings]` without restarting the window or its shells.

## Terminal lifetime

- **`persistent-by-default=no`:** Unnamed desktop launches (`Super+Enter`), bare `splinterm launch`, picker **New**, and in-window **New Terminal** belong to their window and end when it closes. Named launches, command launches, presets, restores, remotes, automation, and MCP stay persistent.
- **`persist-on-tab-organization=yes` (default):** Opening a second Dojo or explicitly naming/renaming a Dojo promotes a transient Lair to persistent permanently. Generated default names (`Dojo 1`, etc.) don't trigger promotion.
- **`close-unused=yes`:** **Close Tab** and compositor window close will terminate a Dojo instead of detaching *only* if all of the following are true at close time:
  - it still has only its original, unsplit default shell pane;
  - the pane has never received keyboard, paste, or mouse input (focus events don't count);
  - neither the Dojo nor its pane was ever explicitly named or renamed;
  - no other window is attached to it; and
  - the shell itself is still in the foreground (a program started by your shell rc counts as use).

  Anything else detaches normally, and **Detach Window** always detaches. If that closed Dojo was the last one in a Lair with an auto-generated name, the empty Lair is removed too; user-named Lairs remain.

## Font sizing and Wayland scale

The default `main.font-sizing-policy=output-scale` follows Wayland compositor output geometry, including fractional scaling. Switch to `physical-dpi` if you want point-sized fonts derived from the display's reported physical DPI instead.

`main.font-pixelsize` and `main.font-point-size` are mutually exclusive. Leave both unset to follow Omarchy's live text size automatically. See [Why native Wayland?](/docs/wayland/) for how scaling fits into the native client.

## Omarchy theme integration

When `main.theme` is unset, Splinterm tracks your active Omarchy theme automatically—no manual export script needed—and reloads valid theme edits live without restarting your daemon, shells, or windows.

- **Theme discovery order:** Checks `${XDG_STATE_HOME:-~/.local/state}/omarchy/current/theme/`, then `~/.local/state/omarchy/current/theme/`, then Omarchy 3.x's `~/.config/omarchy/current/theme/` (falling back cleanly during theme switches without jumping to stale copies).
- **Colors (`foot.ini` + `colors.toml`):** Terminal ANSI colors, selection, alpha, and blur come from the theme's `foot.ini` (`[colors]` and `[colors-dark]` merge key-by-key just like Foot). Chrome roles come from `colors.toml` (canonical Omarchy 4 names first, then legacy aliases). Active-tab text automatically chooses whichever of background or foreground has higher WCAG contrast, and inactive tab labels mix toward foreground if needed to clear 4.5:1 contrast.
- **Glassy Windows & `shell.toml`:** Toggling Omarchy's Glassy Windows updates `foot.ini` (`alpha=0.65`, `blur=yes`), which Splinterm picks up within half a second—making only the default background, padding, tab strip, and docked Explorer translucent while text, explicit cell backgrounds, and the cursor stay crisp and opaque. Popups, menus, and pickers follow Omarchy 4's `shell.toml` (`[menu]` and `[popups]` `background-alpha`), automatically raising alpha if needed to keep text readable over bright wallpapers.
- **Live text size:** With no font size set in `config.ini`, Splinterm follows Omarchy's `shell.toml` `[font] base-size` live (converted to points using the same `px × 9/12` rule Omarchy uses for Foot and other terminals). Window chrome has its own size (`main.ui-font-pixelsize`, otherwise Omarchy's `base-size` in px, otherwise `14` px) and is unaffected by terminal zoom.
- **Opting out:** Point `main.theme` at a JSON file to disable Omarchy auto-discovery:
  ```ini
  [main]
  theme=~/.config/splinterm/theme.json
  ```

Malformed or incomplete theme edits are ignored safely while keeping the last valid palette.

## Keymaps and the command palette

Splinterm binds only a fixed registry of built-in application actions—configuration cannot bind arbitrary shell commands. Because dispatch, the command palette, tab menus, and in-app help all read the same resolved keymap, displayed shortcuts never drift out of sync.

Inspect keymaps locally without contacting `splinterd`:

```bash
splinterm config check
splinterm keymap list
splinterm keymap show
splinterm keymap show omarchy-tmux
splinterm keymap conflicts
```

### Profiles and copy mode

- **`splinterm` (default):** Direct `Ctrl+Shift+…` shortcuts covered in the [quickstart](/docs/quickstart/).
- **`omarchy-tmux`:** Adds `Ctrl+Space` and `Ctrl+B` prefixes, tmux-like pane/tab controls, pane zoom, searchable key help (`Prefix+?`), config reload, vi copy mode (`Prefix+[`), and current-Lair save/pin/preview/restore (`Prefix Shift+S/F/V/O`).
- **Tab jump chords:** `Ctrl+Shift+1`–`9` jump to tabs 1–9 in both profiles (`omarchy-tmux` also provides `Alt+1`–`9`). On non-US keyboard layouts where a Shift+digit produces a bound symbol (such as zoom-in), the symbol binding keeps priority.
- **Copy mode (`Prefix+[`):** Navigate with `h/j/k/l`, arrows, Home/End, or PageUp/PageDown; press `v` to select, `y` or `Super+C` to copy to the Wayland clipboard and exit, or `Escape` to cancel.
- **Omarchy copy/paste:** Both profiles accept `Ctrl+Shift+C`/`V`, `Super+C`/`V`, and `Ctrl+Insert`/`Shift+Insert` while leaving `Ctrl+C` as terminal interrupt. Run `splinterm integration omarchy enable` once on Omarchy so universal `Super+C`/`V` routes to terminal copy/paste.
- **Snap to live:** Pressing `Enter` while scrolled back in history returns to live output without sending a newline to the shell.

### Custom keybinding overlay

Create a strict TOML overlay and point `key-bindings.file` at it:

```toml
version = 1
inherits = "splinterm"

[[unbind]]
sequence = ["Ctrl+Shift+P"]

[[binding]]
sequence = ["Ctrl+Alt+P"]
action = "app.command-palette"
```

Duplicate bindings, unknown actions, and conflicts fail at startup with exact file/line context (and name the `[[unbind]]` needed to free a built-in chord).

## Presets and remote profiles

- **Dojo presets** live in an optional strict TOML file selected by `[presets] file=presets.toml`. See [Dojo presets](/docs/presets/) for bundled Omarchy layouts and custom pane trees.
- **Remote SSH profiles** live in `${XDG_CONFIG_HOME:-~/.config}/splinterm/remotes.toml`. See [Remote access](/docs/remote/) for the schema and `splinterm remote check`.

## Moving from Foot

Rather than copying an entire `foot.ini`, bring over the supported settings you care about:

- `font` → `main.font`
- `pixelsize=N` → `main.font-pixelsize=N`
- `size=N` → `main.font-point-size=N`
- `initial-window-size-chars` → `main.initial-columns` and `main.initial-rows`
- `shell`, scrollback `lines`, `cursor.style`, and `[colors]` `alpha`/`blur` map directly.

Foot-specific features such as arbitrary custom bindings, server mode, notifications, and URL modes aren't configured through Splinterm's `config.ini`.
