---
title: Configuration
description: Configure Splinterm fonts, geometry, shell behavior, scrollback, pane chrome, cursor, and theme overrides.
---

The default configuration path is:

```text
${XDG_CONFIG_HOME:-~/.config}/splinterm/config.ini
```

Start from the repository example at `config/splinterm/config.ini`. Set `SPLINTERM_CONFIG` to test another file without replacing your normal configuration.

## Minimal example

```ini
[main]
font=monospace:style=Regular
font-pixelsize=14
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

[key-bindings]
profile=splinterm
# file=keybindings.toml
# prefix-timeout-ms=1000

[presets]
# file=presets.toml
allow-unrestricted-commands=no
```

Malformed supported values fail startup. Unknown sections and keys produce line-numbered diagnostics.

## Common settings

| Key | Meaning | Default or range |
| --- | --- | --- |
| `main.font` | fontconfig pattern; styles remain in its resolved family | Fontconfig `monospace` Regular |
| `main.font-pixelsize` | terminal pixel font size; any size key stops the terminal following Omarchy's text size | 6–96; Omarchy's text size, otherwise 14 |
| `main.font-point-size` | alternative point size | 6–96; unset |
| `main.font-sizing-policy` | `output-scale` or `physical-dpi` | `output-scale` |
| `main.ui-font-pixelsize` | text size of tabs, Explorer, menus, the command palette and pickers, independent of the terminal font size and zoom; configuration reload applies it | 6–96; Omarchy's text size, otherwise 14 |
| `main.startup` | `new-dojo` or `dojo-picker`: what a commandless desktop launch opens | `new-dojo` |
| `main.omarchy-integration-hint` | under Omarchy without the `terminal` tag, show one desktop notification recommending `splinterm integration omarchy enable` | `yes` |
| `main.padding-*` | four independent logical edges | 0–10000; 12 |
| `main.initial-columns` | initial grid columns | 2–480; 80 |
| `main.initial-rows` | initial grid rows | 2–128; 24 |
| `main.shell` | executable for an empty launch | account login shell |
| `scrollback.lines` | daemon terminal history budget | 0–1,000,000; 1000 |
| `cursor.style` | `block`, `beam`, or `underline` | `block` |
| `multiplexer.persistent-by-default` | ordinary unnamed local graphical Lairs are persistent (`yes`) or Window-owned (`no`) | `yes` |
| `multiplexer.persist-on-tab-organization` | create/name/rename a tab atomically promotes its transient Lair | `yes` |
| `multiplexer.tab-strip-border` | a full-width line along the bottom of the tab strip, as thin as a `line` divider and under the active tab's thicker accent underline; configuration reload applies it | `yes` |
| `multiplexer.new-dojo-directory` | where a new Dojo or Lair opened inside a Window starts: `inherit`, `home`, an absolute path, or `~/path` (resolved on the owning daemon) | `home` |
| `multiplexer.new-splint-directory` | where a split inside a Dojo starts; same values | `inherit` |
| `multiplexer.close-unused` | Close Tab and Window close terminate a Dojo that was never used | `no` |
| `multiplexer.divider-style` | `line`, `frame`, or `none`; configuration reload applies it | `line` |
| `multiplexer.pane-gap` | empty logical px between neighbouring Splints: outside each frame with `frame`, around the centred line with `line`, plain spacing with `none`; also kept around the panes at the Window's edges; shrinks, down to none, where the Splints cannot fit it; configuration reload applies it | 0–64; `0` |
| `multiplexer.frame-title` | `splint` or `none`; configuration reload applies it | `splint` |
| `explorer.agent-status` | show reported agent status in the Explorer and on Dojo tabs (`no` hides it) | `yes` |
| `key-bindings.profile` | `splinterm` or `omarchy-tmux` | `splinterm` |
| `key-bindings.file` | optional strict TOML overlay | unset |
| `key-bindings.prefix-timeout-ms` | prefix timeout in milliseconds | 250–5000; 1000 |
| `presets.file` | optional strict preset catalog | unset |
| `presets.allow-unrestricted-commands` | enable packaged `c`, `cx`, `cy` aliases | `no` |

## Terminal lifetime

`persistent-by-default=no` applies to commandless desktop/SUPER+ENTER launches, bare `splinterm launch`, picker **New**, and in-Window **New Terminal**. Closing the owning Window terminates an unpromoted transient Lair. Explicitly named, command-bearing, preset, restore/relaunch, remote, automation, and MCP creation remains persistent.

With `persist-on-tab-organization=yes`, creating another Dojo or explicitly naming/renaming a Dojo atomically promotes the complete Lair. Promotion is permanent, so a later Window close detaches normally. Set it to `no` to keep organized and multi-tab Lairs Window-owned. Generated Lair and `Dojo 1` names do not promote, and unpromoted transient Dojos never enter Recent Dojos.

With `close-unused=yes`, **Close Tab** and closing the Window from the compositor terminate a Dojo that was never used instead of detaching from it. The daemon decides at the moment of the close: the Dojo must still hold only its original pane, never split; that pane must never have received keys, paste, or mouse input (focus reports do not count); it must have started as a plain default-shell terminal in the running daemon, in any Lair, named or not; the Dojo must never have been named, at creation (even a chosen name equal to the default label) or by a rename of it or its pane; no other Window may be attached, and a Window attaching during the close either keeps the Dojo or is refused; and the shell itself must hold the terminal foreground, so a program started from a shell startup file counts as use. Anything else detaches as before, as does **Detach Window**. When the closed Dojo was the last in a Lair the user never named (one created under a name Splinterm generated for a graphical launch, such as `terminal-1787899189123456789-4132`, and never renamed), that Lair goes too; a Lair the user named stays, empty, even when its name looks generated. Remote Windows get the same decision from the remote daemon.

## Font sizing and Wayland scale

The default `main.font-sizing-policy=output-scale` follows Wayland compositor output geometry, including fractional output scale. Choose `physical-dpi` when point-sized fonts should instead follow the output's reported mode and physical-size DPI.

`main.font-pixelsize` and `main.font-point-size` are mutually exclusive. Pixel-sized fonts remain fixed according to the selected sizing policy; point-sized fonts are converted from the effective DPI. See [Why native Wayland?](/docs/wayland/) for how scaling fits into the native client.

When neither size key is set, Splinterm follows Omarchy's text size, as described under Omarchy theme integration below. The minimal example above sets `font-pixelsize=14`, which opts out.

## Omarchy theme integration

With `main.theme` unset, Splinterm reads the active Omarchy theme from the first of these directories that exists, or whose theme Omarchy is staging as `current/next-theme`:

1. `${XDG_STATE_HOME:-~/.local/state}/omarchy/current/theme/`.
2. `~/.local/state/omarchy/current/theme/`, where Omarchy 4 always writes, when `XDG_STATE_HOME` points elsewhere.
3. Omarchy 3.x's `~/.config/omarchy/current/theme/`, read-only.

Splinterm re-checks the choice on every poll, so it follows the Omarchy 4 migration live. During a theme switch it keeps the directory it was reading rather than falling back to an old Omarchy 3.x copy, and an empty `omarchy/current` left in a higher-priority root never hides the live theme.

The effective `foot.ini` supplies terminal colors, selection, alpha, and blur. As in Foot, `[colors]` and `[colors-dark]` merge key by key into the dark palette. `colors.toml` supplies standard Omarchy roles, with the canonical Omarchy 4 name read before the legacy alias. The active-tab body uses `lighter_background`, then `lighter_bg`, then Foot `bright0`, rather than the terminal selection color. Inactive tab labels start from `muted`, then `color8`, `dark_foreground`, `dark_fg`, or Foot `bright0`. They keep that colour when it reaches 4.5:1 contrast against the tab strip without being louder than the foreground. Otherwise they use the first qualifying 25/50/75 % mix with the foreground, or the plain foreground. The accent comes from `accent`, `cursor`, `blue`, or `color4`. JSON themes may set `inactive_tab_foreground` explicitly.

Active tab text is independent from terminal selection text. Splinterm chooses whichever effective terminal background or foreground has higher WCAG contrast against the resolved active-tab body, preferring foreground on a tie. Native Omarchy themes need no Splinterm-specific roles. Explicit JSON themes may override the active-tab roles and use the same fallback rule against their own background and foreground roles.

Valid theme changes reload without restarting the daemon, shell, or window. A malformed or incomplete replacement retains the last valid palette.

Omarchy's Glassy Windows works through the theme's `foot.ini`, as it does for Foot. Turning it on adds `alpha=0.65` and `blur=yes` under `[colors-dark]` unless the theme already sets them, and turning it off removes them again. Splinterm applies each change within half a second. Only the default terminal background, padding, and tab strip become translucent; text, explicit cell backgrounds, and the cursor stay opaque and crisp. An explicit `[colors] alpha` or `blur` in Splinterm's own configuration overrides the theme and Glassy Windows.

Splinterm also follows the Omarchy 4 shell's `shell.toml`. It layers the user's `~/.config/omarchy/shell.toml` over the active theme's `shell.toml` key by key, as the shell does. Terminal windows pick up edits within half a second; standalone session-picker and remote-status windows read them when they open. The action palette and the Dojo and session picker follow `[menu] background-alpha`. The tab context menu, Dojo prompt, search, history, and the upgrade banner follow `[popups] background-alpha`. A panel is drawn as the window's own glass with that alpha layered over it, so panels stay opaque while Glassy Windows is off and with Omarchy's default of 1.0. Borders, selected rows, and text stay opaque, and Splinterm raises a panel's alpha as far as needed to keep its text at 4.5:1 contrast over any wallpaper. The docked Lair Explorer is part of the window, like the tab strip: its background, selected row and active filter use the terminal's own background alpha (foot.ini `alpha`, which Glassy Windows sets), so the backdrop shows through it as through the terminal, while its text, icons, divider and rules stay opaque.

While `config.ini` sets no font size, Splinterm also follows Omarchy's text size, which `omarchy-display-text-size` writes as `[font] base-size`: the user `shell.toml` value, then the theme's, then Splinterm's 14 px default. Like Omarchy does for Foot, kitty, ghostty and alacritty, it uses `px × 9/12` points rounded to a whole point, so 12 px gives 9 pt and 16 px gives 12 pt, and the default `output-scale` policy sizes those points exactly as Foot does. Open terminal windows rescale live. Like the shell, Splinterm reads `base-size` with `parseInt` and a 1 px floor and clamps the resulting points to the 6–96 pt it renders; a value without digits keeps the current size, or 12 px at startup. Set `main.font-pixelsize` or `main.font-point-size` to opt out.

Window chrome (tabs, Explorer, menus, the command palette and its lists, pickers and Dojo prompts) has its own text size: `main.ui-font-pixelsize` when set, otherwise Omarchy's `base-size` in px (12 px on a stock desktop) even when `config.ini` sets the terminal's size, otherwise 14 px. Runtime zoom resizes only the terminal. Chrome text stops growing where it would overflow its fixed row, and frame titles also stay within the terminal cell height of the pane border. A `base-size` change redraws chrome live, and **Reload configuration** applies a changed `main.ui-font-pixelsize`.

Set an explicit JSON palette only to opt out of native discovery:

```ini
[main]
theme=~/.config/splinterm/theme.json
```

## Keymaps and the command palette

Splinterm binds only a closed registry of application actions. Keyboard dispatch, command-palette labels, tab menus, and generated help therefore describe the same resolved keymap. Configuration cannot register shell commands or callbacks.

Inspect the active configuration without contacting the daemon:

```bash
splinterm config check
splinterm keymap list
splinterm keymap show
splinterm keymap show omarchy-tmux
splinterm keymap conflicts
```

The default `splinterm` profile provides the controls in the [quickstart](/docs/quickstart/). The `omarchy-tmux` profile adds familiar `Ctrl+Space` and `Ctrl+B` prefixes, pane and tab workflows, local pane zoom, stable-ID choosers, `Prefix+B` tab-strip toggling, trusted searchable `Prefix+?` key help, transactional configuration reload, `Prefix+[` vi copy mode, and `Prefix Shift+S/F/V/O` for Save, pin toggle, Preview, and Restore of the current Lair. The closed `lair.save`, `lair.pin-toggle`, `lair.preview`, and `lair.restore` actions remain available to strict overlays when another chord layout is preferred. `dojo.close-other-tabs` is likewise available even though neither profile claims a default chord.

`Ctrl+Shift+1` through `Ctrl+Shift+9` activate the first nine Dojo tabs in both profiles; `omarchy-tmux` also keeps `Alt+1`–`9` and lists them first. Keys match the symbol the layout produces, but with Shift held a number-row 1–9 key that matches nothing as a symbol is retried as its digit, so these chords work where Shift+1 produces `!` while symbol bindings such as `Prefix &` keep priority. A symbol binding also wins where a layout's Shift+digit produces it: zoom-in (`Ctrl++`, which also matches `=`) takes Swiss `Ctrl+Shift+1`, Turkish Q `Ctrl+Shift+4`, and Hungarian `Ctrl+Shift+3`/`7` instead of selecting a tab. With managed tabs, `Ctrl+Shift+2` and `Ctrl+Shift+6` no longer send `Ctrl+@` (NUL) and `Ctrl+^` to the terminal on US layouts; `Ctrl+2` or `Ctrl+Space` and `Ctrl+6` still do. Legacy direct single-Splint attachments still forward them.

Inside keybinding help, type to search the effective action labels, configuration names, shortcuts, sources, and closed keywords. Use Up/Down or PageUp/PageDown to navigate, `Ctrl+U` to clear, and Escape once to clear a non-empty query or again to close. Query input and paste remain client-owned and never reach the terminal.

The command palette is a curated trusted subset of this closed registry. It projects shortcut labels from the effective resolved keymap and exposes binding help, reload, copy mode, pane zoom, Dojo/Lair workflows, and Window detach without allowing configuration, plugins, or terminal output to register commands.

Both profiles provide terminal `Ctrl+Shift+C/V` and `Super+C/V`. They accept what Omarchy's universal `Super+C/V` sends terminal-tagged windows: `Ctrl+Shift+C/V` on current Omarchy (`quattro` since `129c67e2`, after 4.0.4), `Ctrl+Insert`/`Shift+Insert` on Omarchy 3.x and 4.0.x. The `omarchy-tmux` profile advertises `Ctrl+Shift+C/V` first so generated help remains usable when the compositor reserves Super chords; `splinterm` keeps `Super+C/V` primary. Omarchy must classify `com.oldjobobo.splinterm` as a terminal. Its terminal rule does not list Splinterm, so run `splinterm integration omarchy enable` once to add the tag; without it, Omarchy's universal copy sends ordinary `Ctrl+C`, which remains terminal interrupt, and the terminal transparency and `Ctrl+Space` input-method rules do not apply. Until the tag is present, the first Splinterm Window under Omarchy 4 shows one desktop notification saying so; set `main.omarchy-integration-hint=no` to silence it. Splinterm-owned command-palette, keybinding-help, search, and rename fields accept effective `clipboard.copy` / `clipboard.paste` bindings such as `Ctrl+Shift+C/V` and retain local `Super+C/V/X/Z`; terminal-pane `Super+X/Z` remain application-owned. These Super shortcuts work only when the compositor delivers the chord to the Splinterm Window. In copy mode, move with `h/j/k/l`, arrows, Home/End, or PageUp/PageDown. Press `v` to begin selecting, `y` or `Super+C` to publish to the Wayland clipboard and exit, or Escape to cancel. `Super+V/X/Z`, pointer input, paste, and IME text are consumed locally and never forwarded to the terminal application.

While a focused pane is viewing historical output, plain Enter or keypad Enter returns it to live output without sending terminal input. A later Enter pressed while already live submits normally.

A strict overlay can unbind and replace closed actions:

```toml
version = 1
inherits = "splinterm"

[[unbind]]
sequence = ["Ctrl+Shift+P"]

[[binding]]
sequence = ["Ctrl+Alt+P"]
action = "app.command-palette"
```

Unknown actions, malformed chords, duplicate bindings, and semantic conflicts fail startup with source context rather than partially applying. A conflict with a built-in binding also names the `[[unbind]]` sequence that frees the chord.

## Dojo presets

The optional `[presets]` file describes complete named pane trees. Splinterm always ships the bounded `omarchy.t`, `omarchy.tdl`, `omarchy.tds`, `omarchy.tdlm`, and `omarchy.tsl` workflows. Inspection and dry-run are local; real runs commit the complete layout atomically.

Read [Dojo presets](/docs/presets/) for catalog syntax, commands, parameters, and the optional collision-safe `s`, `sdl`, `sds`, `sdlm`, and `ssl` Bash helpers.

## Remote profiles

SSH endpoints use a separate strict TOML file at `${XDG_CONFIG_HOME:-~/.config}/splinterm/remotes.toml`. Profile configuration cannot inject arbitrary SSH options or remote commands.

```bash
splinterm remote list
splinterm remote inspect PROFILE
splinterm remote check PROFILE
```

Read [Remote access](/docs/remote/) for the profile schema and graphical versus automation authority.

## Moving from Foot

Copy supported values rather than copying a complete `foot.ini`:

- Foot `font` maps to `main.font`.
- Foot `pixelsize=N` maps to `main.font-pixelsize=N`.
- Foot `size=N` maps to `main.font-point-size=N`.
- Foot `initial-window-size-chars` maps to columns and rows.
- Foot `shell`, scrollback lines, cursor style, alpha, and blur have bounded mappings.

Arbitrary bindings, notifications, server mode, URL modes, and advanced rendering controls are not currently supported through Splinterm configuration.

Theme alpha and blur are native Wayland presentation features. Blur is requested only when translucency is active, the theme enables it, and the compositor advertises compatible protocol support. Unsupported compositors continue without blur.
