# Splinterm configuration and Foot migration

Phase 8 intentionally supports a small, explicit configuration surface. The
default path is `${XDG_CONFIG_HOME:-~/.config}/splinterm/config.ini`; set
`SPLINTERM_CONFIG` to test another file. Start from
[`config/splinterm/config.ini`](../config/splinterm/config.ini).

## Clipboard PNG saving

Available in the 0.1.1 source; publication is pending. The published `v0.1.0`
release does not include this action.

`[clipboard] image-directory` opts into a persistent destination for explicit
clipboard-image saves. Use **Save clipboard image and insert path** in the command
palette, or bind `clipboard.save-image` in a custom keymap. Neither built-in
profile assigns a shortcut. Ordinary clipboard/primary text paste is unchanged,
even when the clipboard offers both text and PNG. The explicit image action
selects `image/png`; other formats are not converted. Remote sessions are disabled
because a local saved path is not a remote file.

The focused pane must hold control, be at live output, and have keyboard focus,
without copy mode, an input modal, a pending session switch, or a divider drag.
Unavailable palette rows are disabled; invoking a custom binding reports why the
save is unavailable. Focus, control, topology, pane incarnation, input generation,
and the configured destination are checked again before publication and insertion.
Opening another modal or changing the target cancels the original request rather
than redirecting it. Configuration reload also cancels in-flight saves.

There is at most one active image save per Window and one image worker per client
process. The pipe has a two-second read deadline; each worker/UI authorization
wait is also limited to two seconds. UI queues contain at most two small events
and one decision, never PNG bodies or filesystem descriptors. Validation, file
writes, sync, publication, verification, and descriptor cleanup stay on the worker.
A stalled filesystem syscall can hold that worker slot until the kernel returns;
it cannot block Wayland dispatch or cause additional image workers to accumulate.

```ini
[clipboard]
image-directory=/home/your-user/Private clipboard images
```

Create the directory yourself with private permissions (`0700`). It must already
exist, be owned by your user, and grant no group/other access. Every path component
must be a real directory (no symlinks); ancestors must be owned by you or root and
must not be group/other writable. Relative paths, `~`/environment expansion, dot
components, repeated/trailing slashes, control characters, non-UTF-8 paths, and
paths longer than 4000 bytes are rejected. Parsing validates syntax only; a save
worker validates current filesystem identity and permissions. A malformed setting
rejects the entire configuration candidate rather than partially applying it.

The storage limit is **16 MiB of encoded PNG**, **8192 pixels per edge**, and
**16,777,216 pixels total**. Validation fully decodes static image data and checks
the end marker; APNG, truncation, malformed data, and trailing bytes are rejected.
Decoded output is capped at **64 MiB**, with a separate **64 MiB PNG decoder
allocation budget**. A streaming zlib check rejects excess inflated image data,
incomplete streams, and bad checksums; it stops at the declared filtered frame
size plus one byte rather than expanding an arbitrary compressed payload. Its
compressed copy is at most 16 MiB and is released before pixel-output allocation.
Text and ICC metadata are not decompressed by validation;
original bytes, including metadata, are preserved in the saved file. Saving is
not metadata stripping or sanitization for sharing with other applications.

Validation and disk I/O run off the Wayland event loop. Staging uses Linux
`O_TMPFILE`; unsupported filesystems fail with guidance to select another private
directory, never a less-safe temporary-file fallback. Publication also requires
accessible `/proc/self/fd`. The completed file is private (`0600`, or more
restrictive under your umask), named `clipboard-<random UUID>.png`, and linked
without overwriting any existing name, with at most 32 collision retries. File
contents are synced before publication; crash/power-loss durability of the new
directory entry is not promised.

Cancelled or failed **unpublished** saves leave no named file. Once published,
the image is retained until you delete it—even if subsequent path insertion
fails or its original pane becomes stale. That failure must report the saved
path instead of redirecting input or silently deleting a file. There is no age
cleanup and no image-body logging. Unlinking by pathname after checking its inode
still races replacement, so the storage API never automatically deletes a
published name. Input is admitted only once, nonblockingly, when no earlier
terminal input is pending. A busy/full/closed command channel retains the image
and reports its path; saved-path input is never deferred. The payload is an
absolute POSIX-shell-quoted path (including literal apostrophes/spaces/Unicode),
wrapped only if the current pane enables bracketed paste, with no trailing space,
newline, or Enter. Admission is not a daemon acknowledgment or a guarantee that
a terminal application consumed the path.

Progress/failure feedback uses a temporary Window title and stderr. Retained-path
notices escape control/format characters for readable identification without
logging image bytes. Normal title updates can replace the notice. If the Window
closes after publication, the worker reports the retained path to stderr on
channel disconnect or timeout; abrupt process termination cannot guarantee any
notice. Successful saves also persist until manually deleted.

Descriptor-relative access and identity rechecks detect ordinary
substitution, but do not sandbox hostile processes running as your own user.

## Supported keys

| Section/key | Meaning | Range/default |
| --- | --- | --- |
| `main.font` | explicit fontconfig pattern; when unset, follow Omarchy's effective `monospace` family | unset |
| `main.font-ligatures` | startup-only cross-cell shaping: `off`, `on`, or `cursor` | off |
| `main.font-features` | startup-only comma-separated OpenType `tag=value` settings | empty; at most 64 unique tags |
| `main.font-pixelsize` | configured terminal pixel font size; any size key stops the terminal following Omarchy's text size (chrome keeps following it; see `main.ui-font-pixelsize`) | 6–96; Omarchy's text size, otherwise 14 |
| `main.font-point-size` | mutually exclusive point-size alternative | 6–96; unset |
| `main.font-size` | deprecated alias for `main.font-pixelsize` | unset |
| `main.font-sizing-policy` | `output-scale` or `physical-dpi` (no auto mode) | output-scale |
| `main.ui-font-pixelsize` | Window chrome text size in logical px (tabs, Explorer, menus, command palette, pickers, Agent access list, Dojo prompts), independent of the terminal font size and runtime zoom and always sized by output scale; capped where chrome rows end ([Chrome text size](#chrome-text-size)); configuration reload applies it | 6–96; Omarchy's text size, otherwise 14 |
| `main.padding-left`, `padding-right`, `padding-top`, `padding-bottom` | independent logical padding edges | 0–10000; 12 each |
| `main.initial-columns`, `initial-rows` | requested initial grid | 2–480, 2–128; 80×24 |
| `main.startup` | what a commandless `splinterm-xdg-terminal-exec` launch opens: `new-dojo` or `dojo-picker` | new-dojo |
| `main.shell` | shell executable used for an empty launch | login shell from the account |
| `main.login-shell` | use login-style argv[0] for the shell | yes |
| `main.title` | fixed window title; otherwise OSC title | unset |
| `main.app-id` | diagnostic only | fixed to `com.oldjobobo.splinterm` |
| `main.resize-delay-ms` | idle debounce before terminal reflow and PTY resize | 0–1000; 100 |
| `main.dpi-aware` | deprecated **legacy Splinterm** key: `yes` maps only to `output-scale`; `no` fails with migration guidance | unset |
| `main.theme` | explicit Splinterm JSON palette override; disables native Omarchy discovery | unset |
| `main.omarchy-integration-hint` | under Omarchy without the `terminal` tag, show one desktop notification recommending `splinterm integration omarchy enable` ([why](integrations.md#why-the-terminal-tag-matters)) | strict boolean; yes |
| `colors.alpha` | optional Foot-compatible override for theme background translucency | 0.0–1.0; unset (theme-owned) |
| `colors.blur` | optional native background-blur request | strict boolean; unset (theme-owned, otherwise `no`) |
| `colors.active-tab-background` | selected Dojo tab body colour, `#RRGGBB` or `0xRRGGBB`; its label colour is chosen again for contrast ([Chrome backgrounds](#chrome-backgrounds)) | unset (theme-owned) |
| `colors.active-tab-alpha` | selected Dojo tab body coverage over the Window's backdrop | 0.0–1.0; 1.0 |
| `colors.tab-strip-alpha` | tab strip coverage, behind the inactive tabs | 0.0–1.0; unset (terminal background alpha) |
| `colors.explorer-background` | docked Lair Explorer panel colour, `#RRGGBB` or `0xRRGGBB`; its text colours are chosen against it | unset (terminal background) |
| `colors.explorer-alpha` | docked Lair Explorer panel coverage | 0.0–1.0; unset (terminal background alpha) |
| `colors.<role>` | a semantic chrome role's colour, such as `colors.accent` or `colors.status-working`; `[colors-dark]` and `[colors-light]` take the same keys for one appearance ([Role overrides](#role-overrides)) | `#RRGGBB`, `0xRRGGBB`, `color0`–`color15` or `auto`; unset (theme-owned) |
| `scrollback.lines` | daemon terminal history budget | 0–1,000,000; 1000 |
| `clipboard.image-directory` | explicit absolute private directory for user-requested local clipboard PNG saves | unset; no temporary fallback |
| `cursor.style` | `block`, `beam`, or `underline` | block |
| `cursor.blink` | permit cursor blink | yes |
| `multiplexer.persistent-by-default` | ordinary unnamed local graphical terminals create persistent (`yes`) or Window-owned transient (`no`) Lairs | yes |
| `multiplexer.persist-on-tab-organization` | creating or explicitly naming/renaming a Dojo promotes its Window-owned transient Lair atomically; configuration reload applies it | yes |
| `multiplexer.tab-strip-border` | draw a full-width line along the bottom of a shown tab strip, in the colour and stroke thickness of a `line` Splint divider (always at least 1 buffer px thinner than the active tab's accent underline, which is drawn over it); configuration reload applies it | strict boolean; yes |
| `multiplexer.new-dojo-directory` | where a Dojo or Lair created inside a Window starts: `inherit`, `home`, an absolute path, or `~/path`; configuration reload applies it | home |
| `multiplexer.new-splint-directory` | where a Splint split inside a Dojo starts; same values; configuration reload applies it | inherit |
| `multiplexer.close-unused` | Close Tab and Window close terminate a Dojo that was never used instead of detaching from it; configuration reload applies it to the next Close Tab or Window close | no |
| `multiplexer.divider-style` | `line`, `frame`, or `none` pane chrome | line |
| `multiplexer.pane-gap` | empty space between adjacent Splint panes and around the pane area at the Window's edges, in logical px scaled by the output scale (see below); shrinks, down to none, where the Splints cannot fit it; configuration reload applies it | 0–64; 0 |
| `multiplexer.frame-title` | top-frame title source: `splint` or `none`; inert outside frame style | splint |
| `explorer.full-height` | a docked Lair Explorer runs the Window's full height and the tab strip begins at its right edge, above the terminal only; `no` puts the Explorer below a full-width tab strip. An Explorer too wide to dock opens over the terminal below the tab strip either way; configuration reload applies it | strict boolean; yes |
| `explorer.collapsed` | a new Window's Lair Explorer shows as its collapsed rail, one row per Lair, instead of the full tree; `«`, `»` and **Toggle Lair explorer rail** still switch it while the Window is open ([Lair Explorer](usage.md#lair-explorer)). A reload that changes the value collapses or expands the Explorer at once; one that keeps it leaves the Window's own choice alone | strict boolean; no |
| `explorer.agent-status` | show explicitly reported agent status in the Explorer, on Dojo tabs, and in navigation accessibility; `no` hides it all; configuration reload applies it | yes |
| `explorer.status-colors` | colour agent status glyphs in the Explorer and on Dojo tabs by state, and the access "Asking" marker; see [Status colours](#status-colours). `no` draws them in the colour of the text around them; the remote host tag is not a status and keeps its colour. Configuration reload applies it | strict boolean; yes |
| `explorer.status-glyphs` | `icons` draws two-cell icons; `dots` draws one-cell shapes: ◆ needs input, ▲ error, ■ done, □ idle, the Braille spinner for working, and `~` after a stale status. Every state keeps its own shape in both, so colour is never the only signal; configuration reload applies it | icons |
| `integration.dialog-shortcut` | the chord that engages a waiting trusted dialog's keyboard, as a click inside does, without answering it: `ctrl+alt+` and one letter or digit (modifiers in any order; no `shift`, and no Super chords), or `none` to turn it off. When Hyprland maps dialogs without focus (the Omarchy integration), Splinterm also binds it while a dialog waits, so it reaches the dialog from any Window ([Trusted dialogs](usage.md#trusted-dialogs)); read when a dialog starts | ctrl+alt+g |

`multiplexer.pane-gap` puts empty space between neighbouring Splints, outside
their chrome. With `divider-style=frame` each Splint keeps its whole frame and
the gap separates the frames. With `line` the divider line keeps its one-cell
lane and sits in the middle of the gap, half of it on each side; lines
still meet at their junctions. With `none` it is plain spacing. The gap shows
the Window background at the theme's background alpha, so Glassy Windows show
through it. The same gap is also kept around the pane area at the Window's
edges, below the tab strip and beside a docked Explorer, like a tiling window
manager's outer gap, so even a lone Splint's frame stands clear of the edge.
`main.padding-*` is different: it is the space inside each Splint, between its
chrome and the terminal text. Terminal cells, the PTY size of each Splint, focus
clicks, file drops, and frame titles all stay inside the Splint; the gap between
Splints is the divider's drag handle, and the outer gap answers no click. `0`,
the default, is the tiled layout without a gap.

The gap is a preference, never a reason for a layout to fail. When the Splints
cannot all keep their two-cell minimum with the configured gap, as in a small
Window, a tightly split Dojo, or after leaving a zoomed Splint, the gap shrinks
for that layout to the largest one that fits, down to none, between the Splints
and at the edges alike, and grows back when there is room again. When the
Splints cannot fit even without a gap, the Window shows only the focused Splint
until there is room again ([Panes and layouts](usage.md#panes-and-layouts)).

The protocol grid is bounded at 480×128. The shipped 14 px output-scale profile
uses the complete natural grid on validated 2560×1440 and 3840×2160 surfaces.
A smaller font or larger surface can exceed that envelope; Splinterm then keeps
the actual terminal top-left anchored, leaves remaining right/bottom pixels
outside terminal hit-testing, and emits one bounded `grid_capped` diagnostic for
the pane instead of silently treating the residual as terminal cells.

**Reload configuration** (`app.config-reload`, also in the command palette)
reads `config.ini` and its keymap file again for the Window it runs in and
applies exactly these settings: the `[key-bindings]` keymap (profile, overlay
file, and `prefix-timeout-ms`), `clipboard.image-directory` (an in-flight save
is cancelled), `main.ui-font-pixelsize` and `multiplexer.tab-strip-border`
(chrome is drawn again at once), every `[colors]` key, including `alpha`,
`blur`, the [chrome background](#chrome-backgrounds) keys and the
[role keys](#role-overrides) of `[colors]`, `[colors-dark]` and
`[colors-light]` (the theme is resolved again at once, and a key the file no
longer sets gives the theme's own value back),
`explorer.agent-status`, `explorer.status-colors`, `explorer.status-glyphs`,
`explorer.full-height`, `explorer.collapsed`, `multiplexer.divider-style`,
`multiplexer.frame-title`, `multiplexer.pane-gap`, `multiplexer.close-unused`,
`multiplexer.persist-on-tab-organization`, `multiplexer.new-dojo-directory`,
and `multiplexer.new-splint-directory`. A changed `explorer.full-height`,
`explorer.collapsed`,
`multiplexer.divider-style`, `multiplexer.frame-title`, or
`multiplexer.pane-gap` lays the Splints out again
at once and resizes each one's PTY; a gap they
cannot fit shrinks for that layout, as described above, rather than being
refused. The other four multiplexer settings take effect at the Window's next
close, tab organization, or Dojo, Lair, or Splint creation; one already under
way keeps the value it started with. A file with errors is rejected and changes
nothing. Every other setting keeps the value the Window started with.

Malformed supported values fail startup. Unknown sections and keys print
line-numbered diagnostics. When `main.font` is unset, each graphical client
follows Fontconfig's effective `monospace` family and watches that complete
regular/bold/italic/bold-italic plus CJK/emoji generation for live changes. An
explicit `main.font` remains authoritative and disables native live family
following, including when its literal value is `monospace`. Native startup tries
the documented JetBrains Mono Nerd Font pattern only if initial `monospace`
resolution fails, then fails startup if that fallback is also unavailable; a
live-update failure never switches to the fallback and retains the complete last
valid generation.

Splinterm resolves regular, bold, italic, and bold-italic from the selected
regular family. Fontconfig-selected named instances in variable fonts are
preserved across FreeType metrics and rasterization and Swash shaping, metrics,
and outline rasterization. When a styled face is missing, substituted to another
family, or has incompatible terminal-cell metrics, Splinterm warns and
reuses the regular face.

Set `SPLINTERM_FONT_TRACE=1` to print each resolved face (family, style, face
index, and path) and the fallback count on standard error; by default nothing
about fonts is printed.

A valid native family change preserves configured size and sizing policy,
padding, output DPI, runtime zoom, topology, focus, history and copy-mode
anchors, modal input, visible IME preedit, and running processes. Splinterm
stages immutable bounded primary, CJK, and emoji font bytes plus ordered
fallback metadata off the Wayland dispatch path, publishes the generation
atomically, rebuilds active and hidden pane frames, and lets only an
existing pane controller issue the final PTY resize. Observer panes prepare a
future size without acquiring control. Font changes do not imply live reload of
`config.ini` font size, padding, shell, scrollback, cursor, or keymap settings.
While no size key is set, the size instead follows Omarchy's text size, live
in terminal windows through the same path; see "Omarchy shell settings" below.

### Opt-in terminal font shaping

```ini
[main]
font-ligatures=cursor
font-features=calt=1,zero=1,ss01=2
```

`off` (the default) retains per-cell rendering: no joining across terminal
cells. Features still apply within each cell, including combining/emoji
clusters; `off` does not disable required intra-cell shaping. `on` shapes
compatible runs regardless of the cursor. `cursor` breaks runs immediately
before and after the reported visible cursor cell, restoring neighboring
context when it moves away. Blink and focus presentation do not reshape text.
The cursor's logical span is never enlarged to the ligature's ink or source span.

This first live slice joins **only adjacent single-width printable ASCII cells**,
including spaces, with identical attributes and selected face. Style, colors,
decorations, conceal, generated box drawing, non-ASCII/wide/grapheme cells,
missing cells, and row ends are hard boundaries. Other text retains existing
per-cell fallback, with the configured features. Run shaping failure falls back
to that same per-cell path. Terminal widths, original text, selection/copy, and
daemon/protocol semantics are unchanged. Selection colors clip glyph ink to
selected cells, even where contextual glyphs originate in neighboring cells.

Feature tags are case-sensitive, exactly four printable ASCII bytes; settings
are comma-separated, with explicit unsigned decimal values from 0 to 65535.
Whitespace around comma-separated entries is ignored. No bare tags, duplicate
tags, signed/boolean values, or more than 64 settings are accepted; invalid
values fail startup with a line-numbered diagnostic. Empty `font-features=`
uses font/engine defaults. Tags and selectors unsupported by the selected font
have no effect; numeric selectors do not imply that a font provides that variant.
For example, `font-ligatures=off` with `font-features=zero=1` requests a slashed
zero without enabling cross-cell joining.

Both settings require a new client process; configuration reload and the native
font-family watcher do not change them. They remain attached to immutable
renderer resources through font-family changes. Named font instances retain
their existing normalized coordinates. Per-face settings, arbitrary variation
axes, general script/bidi run shaping, and Kitty/Ghostty parity are not provided.
History scroll-copy is retained for cursor-free rows; transitions involving a
visible cursor in `cursor` mode use the existing full-frame rebuild fallback to
avoid copying stale context breaks.

By default, palette roles come directly from the
active Omarchy theme's `colors.toml` and effective `foot.ini`; `[colors] alpha`
and `[colors] blur` are explicit user overrides. Whichever alpha source wins follows Foot's default
alpha mode: only cells whose background source is default are translucent;
explicit and reverse-video backgrounds remain opaque. When blur resolves to
`yes`, alpha is translucent, and the compositor advertises
`ext-background-effect-v1` blur capability, Splinterm requests native
compositor blur for the finite native Window region. Missing protocol support
or capability falls back to ordinary transparency with one bounded diagnostic;
opaque alpha and `blur=no` own no effect object. The protocol is still staging;
the validated initial target is Hyprland 0.56.1 or newer, while other
compositors require compatible version-1 blur capability. `alpha-mode=matching/all`
remains unsupported. Other `[colors]` options, such as Foot's terminal palette
keys, direct users to the active Omarchy palette or an explicit `main.theme`
JSON override; the [role keys](#role-overrides) recolour Splinterm's own
chrome, never the terminal. `[key-bindings]`
selects the typed built-in `splinterm` profile and an optional strict TOML
overlay, so keyboard dispatch and command-palette shortcut labels share one
action registry. This avoids claiming arbitrary `foot.ini` compatibility.

Built-in local bindings include Ctrl+Shift+C/V and Super+C/V for copy/paste,
Ctrl+Shift+S to open the native Recent Dojos picker, and Ctrl+Shift+P to open the
searchable command palette inside any focused managed terminal Window. The
palette is a curated trusted subset of the same closed action registry used by
the effective keymap. It includes resolved keybinding help, transactional
configuration reload, copy mode, focused-pane zoom, Dojo and Lair choosers,
Dojo reordering, Lair rename/navigation/termination, and clean Window detach in
addition to recent Dojos, tab navigation, splits, pane focus/closing/resizing,
tab-strip and font zoom, history/search, and bounded control actions. Force
control is trusted-local only and remains visibly disabled in remote Windows.
Typing filters
titles, categories, and keywords; arrows skip unavailable commands, Enter runs,
and Escape closes. Actions without an available captured target remain visible
but disabled. Right-clicking a visible Dojo tab opens a compact trusted menu
targeted to that tab without activating it first. Its six rows are Rename Tab,
Activate Tab, New Dojo, detach-only Close Tab, detach-only Close Other Tabs, and
Terminate Dojo. Rename opens a bounded prefilled editor. Termination opens a
named confirmation showing the captured pane count with Cancel selected by
default. Arrows navigate, Enter runs, and Escape or an outside click closes the
active surface.
Ctrl+Tab and Ctrl+Shift+Tab cycle Window-local Dojo tabs;
Ctrl+Shift+1 through Ctrl+Shift+9 activate the first nine tabs in strip order,
and a number with no tab is consumed without effect;
Ctrl+Shift+D creates and opens a Dojo in the active tab's Lair; Ctrl+Shift+Q
detaches the active tab and closes the Window when it was the final tab. These
application-owned chords are consumed on press, repeat, and release rather than
forwarded to the terminal process. Directional Ctrl+Shift+Arrow remains pane
navigation. In managed multi-Splint windows, Ctrl+Shift+W terminates and closes
the focused Splint; legacy direct single-Splint attachments leave that chord to
the terminal.
Ctrl+Shift+R revokes active access, and Ctrl+Shift+L releases control.
Ctrl+Shift+T requests transfer from the current controller; its trusted UI uses
Ctrl+Shift+Y/N to accept/deny, while Ctrl+Shift+U opens separate trusted
confirmation for forced takeover. Ctrl+Shift+F opens local literal scrollback
search; Enter submits, Ctrl+N/P navigates, and Escape closes the trusted search
surface. These control/search bindings are trusted application actions rather
than terminal-controlled callbacks. Foot-compatible runtime zoom uses
Ctrl+plus/equal/KP_Add and Ctrl+minus/KP_Subtract in 0.5-point steps; Ctrl+0/KP_0
resets the configured size. Terminal key mappings otherwise follow the
implemented Foot/xterm behavior.

## Keymap configuration

The top-level INI selects a built-in profile and optional overlay:

```ini
[key-bindings]
profile=splinterm
file=keybindings.toml
prefix-timeout-ms=1000
```

`file` is optional. Relative paths resolve beside the selected `config.ini`;
`~/` paths resolve through `HOME`. The timeout accepts 250–5000 milliseconds and
bounds an armed prefix sequence. Packaged profiles are `splinterm` and
`omarchy-tmux`; selecting an unknown profile is a startup error that lists both
available names. The Omarchy profile provides `Ctrl+Space` and `Ctrl+B` prefixes,
its direct and prefixed pane controls, exact five-cell directional resize,
client-local pane zoom, Dojo/Lair creation and selection, numeric (`Alt+1`–`9`,
plus the default `Ctrl+Shift+1`–`9`) and reordered Window-local tabs,
stable-ID trusted choosers, confirmed rename/termination,
Lair navigation, clean Window detach, a per-Window `Prefix+B` Dojo tab-strip
toggle, a generated searchable trusted `Prefix+?` help overlay, vi copy mode,
transactional config reload, and current-Lair lifecycle bindings:
`Prefix Shift+S` saves, `Prefix Shift+F` toggles pinned state,
`Prefix Shift+V` previews, and `Prefix Shift+O` opens the existing restore
confirmation path. These are Splinterm additions; the checked stock Omarchy tmux
profile defines no actions on those four shifted prefix keys.

Where a Splint, Dojo, or Lair created within a Window starts is configurable.
`[multiplexer] new-splint-directory` applies to splits inside a Dojo and defaults
to `inherit`; `new-dojo-directory` applies to new Dojos **and new Lairs** and
defaults to `home`. Each accepts:

- `inherit`: the exact focused/source Splint's live root-process working
  directory (normally its shell). Foreground descendants and terminal text are
  not consulted. If that cwd is deleted, inaccessible, or unavailable, creation
  uses the same source's valid saved launch directory, then the daemon owner's
  valid home directory.
- `home`: the daemon owner's home directory.
- an absolute path, or `~/path` below the daemon owner's home. An unusable path
  falls back to that home directory.

Every choice is resolved on the daemon that owns the source Splint, so a remote
Window uses remote paths and the remote owner's home; `~` is never expanded on
the client. When nothing usable remains, creation fails with "source working
directory is unavailable" for `inherit`, or "configured launch directory and
home directory are unavailable" for the other choices. A missing source is
rejected rather than replaced by another Window's focus. This is a point-in-time
default: the shell can change directory again before launch. A Window reads
the settings when it starts, and **Reload configuration** applies changes to
its later creations.
Explicit `--cwd`, fresh desktop and CLI launches, presets, and saved layouts are
unchanged; shell, login-shell, scrollback, and command settings are unchanged.

`Prefix+[` enters copy mode at the live cursor or current history viewport.
`h/j/k/l` and arrows move over visible and loaded historical rows; `w`, `b`, and
`e` move by Vim-style words; `0`/`$` (or Home/End) move to line edges; and
PageUp/PageDown page within bounded loaded history while
requesting older bounded pages when needed. `v` anchors a selection, `y` copies
it to the Wayland clipboard with the triggering keyboard serial and exits, and
Escape cancels. Copy mode isolates terminal input, paste, pointer actions, IME,
and application mouse reporting, and cancels safely on focus, topology, pane
identity, or history-generation changes.

`Prefix+?` opens help for the effective resolved keymap. Type to search action
labels, configuration names, shortcuts, sources, and closed keywords; use
Up/Down or PageUp/PageDown to navigate, `Ctrl+U` to clear, and Escape once to
clear a non-empty query or again to close. The owned query never reaches the
terminal.

Outside copy mode, both built-in profiles map `Ctrl+Shift+C/V` and `Super+C/V`
to terminal-selection copy and the existing safe/bracketed paste. The
`omarchy-tmux` profile advertises `Ctrl+Shift+C/V` first so its generated help
remains usable when the compositor reserves Super chords; `splinterm` keeps
`Super+C/V` primary. Into windows carrying its `terminal` tag, current
Omarchy's universal binding (`quattro` since `129c67e2`, after 4.0.4) injects
`Ctrl+Shift+C/V`, while Omarchy 3.x and 4.0.x inject
`Ctrl+Insert`/`Shift+Insert`; Splinterm accepts both. Omarchy must classify
`com.oldjobobo.splinterm` as a terminal, which
`splinterm integration omarchy enable` arranges
([why](integrations.md#why-the-terminal-tag-matters)). Without that
classification its universal copy branch injects ordinary `Ctrl+C`, which
remains terminal interrupt. In copy mode, `Super+C` copies the active
selection and exits; `Super+V/X/Z` are consumed locally without pasting or
sending PTY input. These Super shortcuts work only when the compositor delivers
the chord to the Splinterm Window. Splinterm-owned command palette, keybinding
help, search, and rename fields accept effective `clipboard.copy` and
`clipboard.paste` bindings such as `Ctrl+Shift+C/V`; they also support
`Super+A/C/V/X/Z` selection/copy/paste/cut/undo and Shift+Left/Right selection. Their Unicode-safe undo
history is limited to 16 states and disappears with the field. Terminal-pane
`Super+X` and `Super+Z` are not claimed as universal cut or undo actions because
the running application owns its input buffer.

The overlay is versioned TOML and inherits one built-in profile:

```toml
version = 1
inherits = "splinterm"

[[unbind]]
sequence = ["Ctrl+Shift+P"]

[[binding]]
sequence = ["Ctrl+Alt+P"]
action = "app.command-palette"
```

Every table rejects unknown fields. A sequence contains one direct chord or
`["Prefix", "CHORD"]`; prefix sequences require a profile that defines prefixes.
Modifier names are `Ctrl`, `Shift`, `Alt`, and `Super` (`Control` and `Logo` are
accepted aliases). Letter case never implies Shift. Supported keys are letters,
Tab, Enter/KP_Enter, Escape, Space, slash/question, arrows, PageUp/PageDown, End,
backslash, brackets, ampersand, Plus/Equal/Minus, digits 0–9, and KP_0. Empty or
duplicate modifiers and unsupported keys fail with source context.

Keys match the symbol the layout produces. With Shift held, a press on the
number-row 1–9 key that matches no binding as a symbol is retried as that digit,
so `Ctrl+Shift+1` works where Shift+1 produces `!`, while symbol bindings such
as `Prefix &` keep priority. The 0 key has no such fallback. A symbol binding
also wins where a layout's Shift+digit produces it: zoom-in (`Ctrl++`, which
also matches `=`) takes Swiss `Ctrl+Shift+1`, Turkish Q `Ctrl+Shift+4`, and
Hungarian `Ctrl+Shift+3`/`7` instead of selecting a tab. With managed tabs, the
default `Ctrl+Shift+2` and `Ctrl+Shift+6` no longer send `Ctrl+@` (NUL) and
`Ctrl+^` to the terminal on US layouts; `Ctrl+2` or `Ctrl+Space` and `Ctrl+6`
still do. Legacy direct single-Splint attachments still forward them.

An overlay applies unbinds before bindings. An unmatched unbind is a diagnostic;
duplicate or semantically overlapping chords are errors naming both sources. A
conflict with a built-in binding also names the `[[unbind]]` sequence that frees
the chord, for example `sequence = ["Ctrl+Shift+1"]`.
Only closed application actions can be selected—configuration cannot register
shell commands or callbacks. Bindable action IDs are:

```text
app.command-palette       session.recent
clipboard.copy            clipboard.paste
clipboard.save-image
dojo.new                  dojo.previous
dojo.next                 dojo.close-tab
dojo.close-other-tabs      dojo.rename
dojo.terminate-confirmed
dojo.choose               dojo.select-1
dojo.select-2             dojo.select-3
dojo.select-4             dojo.select-5
dojo.select-6             dojo.select-7
dojo.select-8             dojo.select-9
dojo.move-left            dojo.move-right
lair.new                  lair.rename
lair.save                 lair.pin-toggle
lair.preview              lair.restore
lair.terminate-confirmed  lair.previous
lair.next                 lair.choose
window.detach             pane.split-below
pane.split-right          pane.focus-left           pane.focus-right
pane.focus-up             pane.focus-down
pane.close                pane.resize-smaller
pane.resize-larger        pane.resize-left-5
pane.resize-right-5       pane.resize-up-5
pane.resize-down-5        pane.zoom-toggle
view.toggle-tab-strip     app.binding-help
app.config-reload
copy-mode.enter           terminal.send-prefix      history.search
history.page-up           history.page-down
history.return-live       view.zoom-in
view.zoom-out             view.zoom-reset
control.request           control.release
control.force             control.accept-transfer
control.deny-transfer     access.revoke-all
```

Local inspection never contacts `splinterd`:

```text
splinterm config check
splinterm keymap list
splinterm keymap show [splinterm|omarchy-tmux]
splinterm keymap conflicts
```

`config check` parses the INI and selected overlay together. `keymap show` without
a name displays the effective keymap with source locations; with a name it shows
the packaged profile. Invalid configuration is never partially applied: Window
startup fails before mapping rather than falling back to a half-resolved keymap.

## Dojo presets

Splinterm always includes the bounded `omarchy.t`, `omarchy.tdl`, `omarchy.tds`,
`omarchy.tdlm`, and `omarchy.tsl` catalog. Select an optional strict user overlay
beside `config.ini`:

```ini
[presets]
file=presets.toml
allow-unrestricted-commands=no
```

A configured file must be readable and valid; it is never silently ignored.
Relative paths resolve beside `config.ini`. User preset names shadow packaged
names. User command aliases shadow packaged aliases only inside user-owned
presets; they cannot rewrite bundled Omarchy layouts.

`allow-unrestricted-commands` defaults to `no`. Setting it to `yes` explicitly
enables only the packaged `c`, `cx`, and `cy` aliases, whose full direct argv is
documented in [Dojo presets](presets.md). It never enables shell evaluation.

Version-1 catalogs define direct command aliases and bounded named node trees.
`columns` means left/right; `rows` means top/bottom. Ratios are the first child's
thousandths in `1..=999`. Panes may use a static alias, a typed command parameter,
or `shell=true`. Optional panes collapse their unary parent branch. A bounded
`grid` expands deterministically after integer/command parameter validation.
Trees reject cycles, reuse, orphans, invalid focus, depths over 32, and more than
32 final panes.

```text
splinterm preset list
splinterm preset show NAME
splinterm preset check [PATH]
splinterm preset run NAME --cwd PATH [--param NAME=VALUE]... --dry-run
splinterm preset run NAME --cwd PATH [--param NAME=VALUE]... [--no-open]
```

The optional Bash integration is generated from the packaged presets rather
than selected by another INI field:

```text
splinterm preset shell-init omarchy --shell bash
splinterm preset shell-install omarchy --shell bash
```

The installer uses
`${XDG_CONFIG_HOME:-$HOME/.config}/splinterm/shell/omarchy.bash`, creates it only
when absent, and never edits `.bashrc` or another startup file. The generated
file must be sourced explicitly and refuses to define any function when `s`,
`sdl`, `sds`, `sdlm`, or `ssl` already resolves to an alias, function, builtin,
or executable. It does not define or replace Omarchy's tmux shell names.

Inspection and dry-run are local and do not connect to `splinterd`. Dry-run
compilation checks final cwd directories and launch bounds and prints a topology
preview without showing full argv. A non-dry run verifies its exact invoking or
focused Splint context, then sends one trusted-local atomic preset request; it
never emulates a preset with a sequence of partial splits. Successful runs open
the first committed Dojo by stable ID unless `--no-open` is explicit. Remote, automation,
and MCP clients cannot invoke this private materialization request. See
[Dojo presets](presets.md) for the complete schema, bundled layouts,
parameter rules, direct-execution rules, and failure semantics.

## Remote profile configuration

Remote SSH endpoints use a separate strict TOML file rather than the
Foot-compatible INI parser:

```text
${XDG_CONFIG_HOME:-~/.config}/splinterm/remotes.toml
```

```toml
version = 1

[remotes.wintermute]
host = "wintermute"
user = "operator"                    # optional; OpenSSH default otherwise
port = 22                            # optional; OpenSSH config/default otherwise
identity_files = ["~/.ssh/id_ed25519"]
known_hosts_file = "~/.ssh/known_hosts"
connect_timeout_seconds = 15
```

The schema rejects unknown fields at every level. Profile names, host/user
tokens, counts, document size, paths, ports, and timeouts are bounded. Explicit
files must be readable regular local files; path expansion never invokes a
shell. There is deliberately no arbitrary option, forwarding, environment,
proxy-command, local-command, or remote-command field. Ordinary safe alias,
identity, certificate, agent, and proxy routing configured in OpenSSH remains
available, while Splinterm supplies fixed safety overrides and the fixed remote
command.

Use `splinterm remote list` and `splinterm remote inspect PROFILE` for local-only
validation. `splinterm remote check PROFILE` additionally starts SSH and performs
bounded non-mutating relay/daemon reachability probes. `splinterm --remote
PROFILE` authenticates once and opens that profile's native Recent Dojos
picker; explicit `dojos`, `reopen`, `window`, and `launch` forms are also
available. Recency is namespaced by validated local profile identity. See
[remote.md](remote.md) for authentication, host-key, human authority,
remote-path, no-image, and disconnect behavior.

## Daily launch and Dojo reopening

The normal desktop/XDG command remains `splinterm-xdg-terminal-exec`. Its
commandless form, bare `splinterm launch`, picker **New**, and in-Window **New
Terminal** use `multiplexer.persistent-by-default`. The compatibility default is
`yes`. With `no`, an ordinary unnamed local graphical launch is owned by its
Window: closing that Window terminates its processes and removes the Lair unless
it was promoted first.

An explicit Lair name, `splinterm new NAME`, `splinterm launch --name NAME`, a
command-bearing native launch, a preset, restore/relaunch, remote creation, or
automation/MCP creation remains persistent. Generated collision-resistant Lair
names and the generated initial `Dojo 1` label do not count as explicit naming.
When an application supplies `splinterm-xdg-terminal-exec -- COMMAND...`, the
adapter retains its existing client-bound lifetime regardless of the setting;
these Windows start with the tab strip hidden.

With `multiplexer.persist-on-tab-organization=yes`, creating another Dojo tab or
explicitly naming/renaming a Dojo atomically promotes the complete transient
Lair before the tab mutation commits. Promotion is permanent: closing the
Window afterward detaches instead of terminating it. With `no`, organized and
multi-tab Lairs remain Window-owned and are removed together when that Window
closes. Unpromoted transient Dojos never enter Recent Dojos.

With `multiplexer.close-unused=yes`, **Close Tab** (including closing the other
tabs) and closing the Window from the compositor terminate a Dojo that was
never used, instead of detaching from it. The daemon decides at the moment of
the close. A Dojo counts as unused only while all of these hold:

- it still holds only the Splint it was created with; a split counts as use,
  even after the extra pane is closed again;
- that Splint never received client input: keys, paste, or mouse. Focus
  reports the graphical client marks as its own and other replies the
  terminal generates do not count; the same bytes sent by automation or
  pasted do;
- it started as a plain default-shell terminal in the running daemon.
  Command-bearing launches, presets, restored or relaunched Splints, and Dojos
  from before the daemon last started count as used. The Lair's name does not
  matter: Dojos in explicitly named Lairs qualify too;
- the Dojo was never named: a name chosen when creating it (even one equal to
  the default label), or renaming the Dojo or its Splint later, counts as use.
  The client tells the daemon whether Splinterm generated the name;
- no other Window is attached to it;
- the shell itself holds the terminal foreground, so a program started from a
  shell startup file counts as use.

Input that races the close either reaches the shell, which keeps the Dojo, or
is refused because the close won. Likewise a Window attaching during the close
either attaches first and keeps the Dojo, or is refused. A used Dojo is
detached as before, and so is every Dojo when the daemon gives no answer.
**Detach Window** (`window.detach`) always detaches.

**Reload configuration** (`app.config-reload`) applies a changed
`close-unused` to the Window's next Close Tab or Window close, including for
tabs that were already open. A close already in progress keeps the value it
started with.

When the closed Dojo was the last one in a Lair the user never named, the Lair
is removed in the same step. Such a Lair was created in the running daemon
under a name Splinterm generated for a graphical launch (for example
`terminal-1787899189123456789-4132`), as the client records when it creates the
Lair, and was never renamed since. The name's shape alone never decides: a
Lair with a name the user chose stays, even one that looks generated, and stays
empty, as it does when its last Dojo is closed explicitly. A remote Window gets
the same decision from the remote daemon.

Dojo reopening is deliberately separate:

```text
splinterm-dojos     → native Recent Dojos picker
splinterm-reopen    → last locally remembered running Dojo
```

`splinterm-sessions` and `splinterm sessions` remain compatibility aliases.

Set `main.startup=dojo-picker` to make the commandless desktop/XDG launch open
the Recent Dojos picker instead of creating a terminal; no shell starts until
**New** or a Dojo is chosen. The default `new-dojo` keeps the existing behavior.
For one launch, `splinterm-xdg-terminal-exec --picker` opens the picker and
`--new` creates a fresh terminal regardless of the setting; `splinterm launch
--picker` is the direct CLI form. The **New Terminal** desktop action passes
`--new`, so it always creates a terminal. Picker **New** uses the launch working
directory. Command-bearing launches (`-- COMMAND...`) never open the picker and
reject `--picker`.

The in-window Ctrl+Shift+S shortcut paints a trusted modal overlay over dimmed
live panes without creating another Wayland Window or replacing an existing tab.
Escape removes the overlay and presents the newest valid pane state. Choosing a
running Dojo opens or activates its tab; New Terminal creates a fresh
Lair and opens its initial Dojo as a tab. One Window accepts at most 32 distinct
Dojo tabs, may mix Lairs, and does not restore tab order after exit. Tabs use a
sanitized Dojo label unless ambiguity requires sanitized `Lair / Dojo` context.
Closing a tab never closes its Dojo or Splints. The overlay adapts to compact and minimal
sizes, and vertical wheel or touchpad scrolling navigates hidden actions without
reaching terminal history or mouse reporting.

A suitable Omarchy convention is Super+Enter for the normal terminal command
and Super+Shift+Enter for `splinterm-dojos`. Splinterm does not modify the
user's Hyprland configuration automatically. The picker opens only Dojos whose
complete pane layout is still running; restoring exited processes
remains an explicit lifecycle command.

## Migrating from Foot

Copy values rather than copying a whole `foot.ini`:

- Foot `font` → `main.font`. Foot `pixelsize=N` →
  `main.font-pixelsize=N`; Foot `size=N` → `main.font-point-size=N`.
  A `size=` or `pixelsize=` embedded in `main.font` is rejected because the
  face/style pattern cannot become a second sizing authority.
- Foot `dpi-aware=no` → `main.font-sizing-policy=output-scale`: a 96-DPI font
  is scaled with compositor output scale. Foot `dpi-aware=yes` →
  `main.font-sizing-policy=physical-dpi`: points use the most recently entered
  Wayland output's mode/physical-size DPI and pixel sizes remain fixed. Missing,
  invalid, or unreasonable output data falls back to 96 DPI with provenance.
  Splinterm intentionally has no `auto` value.
- Foot `initial-window-size-chars` → `main.initial-columns` and
  `main.initial-rows`.
- Foot `shell` → `main.shell`; Splinterm never evaluates it as a shell command.
- Foot `scrollback.lines` and cursor style/blink map directly.
- Foot `alpha` and `blur` are imported together from `[colors-light]` when
  Foot's `main.initial-color-theme=light`; otherwise from `[colors-dark]`, or
  legacy `[colors]` when no dark section exists. Use `[colors] alpha` and
  `[colors] blur` only for explicit Splinterm overrides.
- Convert colors through the Omarchy generator below instead of pasting Foot's
  complete `[colors]` section.

The Foot mapping above is separate from migration of Splinterm's old key.
Legacy Splinterm `main.dpi-aware=yes` meant “follow compositor scale” and maps
only to `output-scale`. Legacy Splinterm `dpi-aware=no` forced the whole surface
to 1×, has no behavior-preserving mapping, and fails with a targeted message.
Using the legacy and new policy keys together is rejected.

Wayland `surface_scale_120` always follows compositor output geometry and is
never disabled by font policy. Font resolution records the configured unit,
policy, observed/sizing DPI provenance, compositor scale, effective 26.6 size,
and final pixel size.

Foot options outside the table—server mode, pad geometry, URL modes, arbitrary
bindings, notifications, and advanced rendering controls—are unsupported in
this MVP and produce diagnostics when represented as unknown keys.

## Native Omarchy theme integration

With `main.theme` unset, Splinterm reads the active Omarchy theme directly. It
considers these directories in order:

1. `${XDG_STATE_HOME:-~/.local/state}/omarchy/current/theme/`.
2. `~/.local/state/omarchy/current/theme/`, when `XDG_STATE_HOME` names a
   different directory. Omarchy 4 always writes here, whatever
   `XDG_STATE_HOME` says.
3. Omarchy 3.x's `~/.config/omarchy/current/theme/`, read-only.

Splinterm uses the first directory that exists, or whose theme Omarchy is
staging as `current/next-theme`, and it re-evaluates that choice on every poll,
so it follows the Omarchy 4 migration without a restart. A theme switch stages
`current/next-theme` before it replaces `current/theme`, so during that brief
gap Splinterm keeps the directory it was reading rather than falling back to an
older Omarchy 3.x leftover, and an empty `omarchy/current` left behind in a
higher-priority root never hides the live theme. If a higher-priority root vanishes entirely,
the live watcher keeps it for four more polls (2 s) before using a lower one.

The effective `foot.ini` supplies the terminal foreground/background, ANSI 16,
cursor, selection background and foreground, alpha, and blur. `colors.toml`
supplies the colours of Splinterm's own chrome, its semantic roles. For each
role Splinterm takes the first key of its cascade that has a valid value, so
a theme's semantic key wins over the named hue, which wins over its ANSI
`colorN` alias, and the role falls back to the Foot palette only when the
theme names none. An empty value counts as absent. Canonical Omarchy 4 names
come before their legacy aliases, in `omarchy-theme-color`'s order:

| Role | Used for | Keys, in order | Without one |
| --- | --- | --- | --- |
| surface-raised | menus, pickers, popups, selected Dojo tab body | `lighter_background`, `lighter_bg` | the foreground mixed 8% over the background |
| surface-sunken | (not yet painted) | `dark_background`, `dark_bg` | the background mixed 6% toward black |
| selection | the selected row's fill | `selection_background`, `selection` | Foot `selection-background` |
| divider | pane borders, frames | `border`, `muted`, `color8` | Foot `bright0` |
| focus-ring | active pane border, focused frames | `active_border_color` | the accent |
| text-muted | inactive tab labels, menu secondary text | `muted`, `dark_foreground`, `dark_fg` | the foreground mixed 30% toward the background |
| accent | active tab underline, selection rails | `accent`, `cursor`, `blue`, `color4` | Foot cursor colour |
| danger | needs input, error, Terminate, a trusted dialog's no area | `red`, `color1` | Foot `regular1` |
| warning | working, "Asking", a trusted dialog's frame | `warning`, `yellow`, `color3` | Foot `regular3` |
| success | idle, a trusted dialog's yes area | `success`, `green`, `color2` | Foot `regular2` |
| info | done | `cyan`, `color6` | Foot `regular6` |
| remote-marker | the remote host tag | `signal`, `magenta`, `color5` | Foot `regular5` |

Surface, text and the faint text of a stale status come from the Foot
background and foreground; faint text is the foreground mixed 55% toward the
background. A theme whose ANSI yellow slot is deliberately another colour
keeps its chrome warnings in its `warning` colour. The keys Splinterm read
before 0.2 (`lighter_background`, `muted`, `accent` and their cascades)
still reject a theme with an invalid value. An invalid value of any other
key is skipped, the cascade goes on, and Splinterm reports it once on
standard error. The theme's appearance, dark or light, is `colors.toml`
`mode`, else Foot's `[main] initial-color-theme`, else whichever of black or
white text contrasts more with the background.

Omarchy itself falls back from `lighter_background` to `color0`, which is the
terminal background. That would make the selected tab and the popups
invisible, so Splinterm mixes the foreground 8% over the background instead.
The selected-tab body never borrows the terminal selection background. Its
label and close affordance use whichever effective Foot background or
foreground has higher WCAG contrast against that resolved tab body, preferring
the foreground on an exact tie.

Inactive tab labels and close affordances are muted. Splinterm starts from the
resolved text-muted hint and keeps it when it has at
least 4.5:1 WCAG contrast against the tab-strip background without exceeding
the foreground's own contrast. Otherwise it tries fixed mixes of that color
with 25, 50, and 75 % foreground and takes the first that qualifies, falling
back to the plain foreground. Contrast ignores alpha. Native Omarchy themes do
not need Splinterm-specific palette roles.

The tab-strip background inherits the terminal alpha while preserving its
resolved color. The selected-tab body stays opaque, so it remains clearly apart
from the strip and the inactive tabs when the terminal is translucent. The
selected-tab underline remains the opaque UI accent. `config.ini` can change
both; see [Chrome backgrounds](#chrome-backgrounds). Terminal selections continue to use Foot's independent
`selection-foreground`, falling back to the terminal foreground when that Foot
role is absent. Foot's `[main] initial-color-theme=light` selects the light
palette, `[colors-light]` together with the deprecated `[colors2]`. A missing or
`dark` setting selects the dark palette, `[colors-dark]` together with the
deprecated `[colors]`. Splinterm follows Foot 1.27.0 and merges those section
pairs key by key in file order, with the last assignment winning. A legacy
`[colors]`-only theme therefore keeps its palette when a later `[colors-dark]`
adds only `alpha` and `blur`. The dark and light palettes are never mixed with
each other. A missing or incomplete selected palette is rejected. Absent alpha
defaults to opaque and absent blur defaults to off. Theme switches re-read this
selector along with the palette. Foot's separate runtime color-theme
keybindings are not imported.

Splinterm fingerprints the selected directory plus both source files every 500 ms.
This detects Omarchy's atomic current-theme directory replacement and applies a
valid palette through the existing live theme channel without restarting the
daemon, shell, or Wayland window. A transiently incomplete replacement or
malformed live theme retains the last valid palette and reports one bounded
diagnostic. If Omarchy state is absent at startup, Splinterm uses its bundled
safe fallback and stays quiet while that optional state remains absent. This
includes non-Omarchy desktops such as NixOS with Noctalia Shell; Omarchy is not
a required dependency. The watcher still discovers an Omarchy theme that appears
later. A present but incomplete or malformed theme, a staged theme switch, or the
loss of a previously loaded theme still takes the strict live-reload path and
retains the last valid palette.

### Status colours

Agent status glyphs take herdr's colours: working in warning, needs input
and error in danger (each with its own glyph), done in info, idle in
success, and a stale status, its marker included, in faint text. The access
"Asking" marker is warning and the remote host tag is remote-marker. A tab
badge colours its glyph, not its count. Each colour is checked against the
opaque colour it is drawn on, the Explorer panel or its selected row, the
tab strip or the selected tab's body, and mixed toward readable text until
glyphs reach 3:1 and text, the remote tag and the ASCII `A?` marker,
4.5:1. Under translucent glass the backdrop shows through and can lower
that contrast, as it does for ordinary chrome text. Status is never colour
alone: every state has its own glyph shape in both `explorer.status-glyphs`
styles, and accessible text and search always use state words.
`explorer.status-colors=no` draws status glyphs and the "Asking" marker in
the colour of the text around them; the remote tag is not a status and
stays remote-marker. JSON and built-in themes take the same roles from
their ANSI colours. A colour a [role key](#role-overrides) sets is drawn as
given, without that mixing.

### Role overrides

`config.ini` can set any of Splinterm's semantic chrome roles itself, over
whatever the theme gives. Each role has a `[colors]` key of its own name:

```ini
[colors]
accent=#e0af68
focus-ring=color4
status-working=auto

[colors-light]
accent=#8c6c3e
```

| Key | Painted today | Without the key |
| --- | --- | --- |
| `surface` | the tab strip and the Explorer panel; darkened 80% toward black, the scrim behind a modal picker | the terminal background |
| `surface-raised` | menus, pickers, the command palette, the tab menu, the Dojo prompt, the history overlay and other popups, and the selected Dojo tab body | the theme's ([role table](#native-omarchy-theme-integration)), else `text` mixed 8% over `surface` |
| `surface-sunken` | nothing yet | the theme's |
| `selection` | the selected row of menus, pickers and the Explorer | the theme's |
| `divider` | pane borders and frames, menu and picker frames, the tab strip border | the theme's |
| `focus-ring` | the active pane border, focused frames and focused menu frames | the theme's, else `accent` |
| `text` | tab labels, menu, picker and Explorer text, and trusted dialog text, which those panels and dialogs raise to 4.5:1 | the terminal foreground |
| `text-muted` | inactive tab labels, and the secondary text of menus, pickers and the Explorer, raised to 4.5:1 there | the theme's, ramped from `text` |
| `text-faint` | through `status-stale` only | `text` mixed 55% toward `surface` |
| `accent` | the active tab underline, selection rails, the history overlay | the theme's |
| `danger` | Terminate in the command palette, the tab menu and the Dojo prompt, raised to 4.5:1 there; a trusted dialog's red (no) area; and through `status-needs-input` and `status-error` | the theme's |
| `warning` | the "Asking" marker, a trusted dialog's frame, and through `status-working` | the theme's |
| `success` | a trusted dialog's green (yes) area, and through `status-idle` | the theme's |
| `info` | through `status-done` only | the theme's |
| `status-needs-input` | the needs-input status glyph | `danger` |
| `status-error` | the error status glyph | `danger` |
| `status-working` | the working status glyph | `warning` |
| `status-done` | the done status glyph | `info` |
| `status-idle` | the idle status glyph | `success` |
| `status-stale` | a stale status and its marker | `text-faint` |
| `remote-marker` | the remote host tag | the theme's |

A value is `#RRGGBB` or `0xRRGGBB`; `colorN`, the terminal palette's ANSI
colour N from `color0` to `color15`, which follows theme switches; or `auto`,
the theme's own value, the same as leaving the key out. Anything else fails
startup with the line number, as for the other `[colors]` keys.

For each colour Splinterm takes the first of:

1. a [chrome background](#chrome-backgrounds) key, `active-tab-background`
   over `surface-raised` and `explorer-background` over `surface`;
2. the role key;
3. the theme's own colour for the role, including a JSON theme's
   `active_tab_background` and `inactive_tab_foreground`;
4. Splinterm's derivation from other roles;
5. the built-in palette.

An overridden role is used as given, with two exceptions, which apply to
every theme. Menus, pickers and the Explorer raise their text, muted text
and Terminate labels to 4.5:1 against their own panel and selected row, so a
`text`, `text-muted` or `danger` override too faint for a panel is raised
there; on a translucent panel, muted text and Terminate labels move toward
the text as far as they need to read over any backdrop. Trusted dialogs
(access consent, upgrade, restart), which take the theme as they open,
with its `[colors]` keys, and are always opaque, raise their text to 4.5:1
and their frame and answer areas to 3:1 against their background. Their
text tells you to click the red area to say no and the green one to
say yes, so a `danger` that is not red or a `success` that is not green,
whether the theme's or set here, gives way to Splinterm's own red or green
there. Everywhere else the role is never mixed toward readable text or
raised to a contrast floor, and a status role that follows it is drawn the
same way. When it falls below its floor against `surface`, 4.5:1 for `text`,
`text-muted`, `text-faint`, `remote-marker` and `warning` (which also paints
the ASCII `A?` marker as text) and 3:1 for the other hue and status roles,
Splinterm says so once on standard error, naming the section that set it,
and still uses it. A `focus-ring` that is set itself is also always drawn,
even where one too close to the frame colour would otherwise give way to the
accent. A role that follows an overridden one follows the new value unless
it is set itself: `accent` moves `focus-ring` when the theme names no focus
ring of its own or names its accent as one (the shipped JSON theme does),
`text` moves `text-muted`, `text-faint` and the floors of the derived hues,
and `warning` moves `status-working`. A role key spelled with underscores,
such as `focus_ring`, is reported with the name it means and ignored.

`surface` and `text` recolour chrome only. The terminal keeps the theme's
background, foreground and palette.

`[colors-dark]` and `[colors-light]` take the same role keys and apply over
`[colors]` to a theme of that appearance only (see
[Native Omarchy theme integration](#native-omarchy-theme-integration) for how
the appearance is read), so `auto` there clears a `[colors]` value for that
appearance. They take no other key. **Reload configuration** applies every
role key at once, and a key the file no longer sets gives the theme's own
value back.

### Glassy Windows

Splinterm follows Omarchy's Glassy Windows through the theme's `foot.ini`, as
Foot does, so no Hyprland opacity rule is involved. When Glassy Windows is on,
`omarchy-theme-set-glass` inserts `alpha=0.65` and `blur=yes` between
`# omarchy-glass begin` and `# omarchy-glass end` markers, directly under the
first `[colors-dark]` header. It skips any setting the file already has. If the
file has no `[colors-dark]` header, the script appends a `[colors-dark]` section
holding only those lines. Turning glass off removes the block in place.
Splinterm sees each edit within one 500 ms poll. It applies the change through
`ext-background-effect-v1`, and the palette stays the same:

* A theme that already sets `blur=yes`, as the stock template does, gains only
  `alpha=0.65`.
* A user template without `blur=` gains both settings.
* A theme that declares its own `alpha` keeps it. Glass then adds only `blur`,
  which updates the compositor effect without repainting.

The default terminal background, the padding, and the tab strip become
translucent; Splinterm premultiplies them by the alpha. The selected Dojo tab
stays opaque. Panels follow the shell
alpha described under "Omarchy shell settings" below. Text, explicit cell
backgrounds, and the cursor stay opaque, so text remains crisp over the blur. An
explicit `[colors] alpha` or `blur` in Splinterm's own `config.ini` overrides
the theme value and therefore also overrides Glassy Windows.

For each new Splint, the persistent daemon also reads the active theme's
`gum_env.lua`. It picks the theme directory with the same order and
`omarchy/current` test as above, then removes inherited values in Omarchy's bounded Gum palette
namespace, and supplies the current rendered values to the new PTY. Theme
changes therefore affect Gum in newly created shells without restarting the
daemon or existing sessions. Existing shells retain their original environment.
If the Gum palette is absent or malformed, the daemon preserves its inherited
environment instead of applying a partial update.

No theme hook, generated file, or manual integration step is required. Setting
`main.theme=/path/to/theme.json` explicitly opts out of Omarchy discovery for
portable or isolated use. The strict JSON schema retains optional
`selection_foreground`, `active_tab_background`, `active_tab_foreground`,
`inactive_tab_foreground`, `pane_border`, and `pane_border_active` overrides.
`selection_foreground` falls back to the normal foreground and
`active_tab_background` falls back to the raised surface, the foreground mixed
8% over the background, for older JSON themes. A
missing `active_tab_foreground` uses the same contrast algorithm against the
JSON palette's resolved `background` and `foreground`. A missing
`inactive_tab_foreground` applies the inactive-tab rule above to ANSI color 8;
an explicit value is used verbatim. Malformed explicit values fail theme
loading. The optional `tools/generate-omarchy-theme.py` exporter derives the
other JSON roles from the standard Omarchy background ramp and
foreground/background endpoints. It reads the canonical Omarchy 4 names first,
then the legacy short names and ANSI `colorN` aliases. Like Omarchy, it derives a
missing bright accent by mixing its base colour 20 % toward white. The
selected-tab body comes from `lighter_background`, then `lighter_bg`, then
`color8`. The exporter reads `alpha` and `blur` with the same Foot section
merge as above. It leaves `inactive_tab_foreground` unset, so that role is
derived from ANSI color 8, which the exporter fills from `muted`, when the theme
loads.

### Omarchy shell settings

Splinterm also follows three settings from the Omarchy 4 shell's `shell.toml`:
panel alpha, text size and the top bar's height, which sizes the tab strip.
The shell reads two layers: the active theme's `shell.toml`, rendered from
Omarchy's template or shipped by the theme, and the user's
`~/.config/omarchy/shell.toml`. Omarchy hard-codes that user path under `$HOME`.
Splinterm layers them the same way, key by key: a key in the user file replaces
the theme's value for that key only, and a key in neither file takes Omarchy's
default. Terminal windows poll both files with the theme every 500 ms, so
edits apply live there. Standalone Dojo and session picker windows and
remote-status windows read the theme, palette and shell settings alike, once
when they open.

Splinterm reads values the way the shell's `parseShell` does: a non-empty quoted
string without quotes inside, opened and closed by either `"` or `'`, or a bare
number such as `0.9` or `-1`, a list of two to four numbers, or a word.
The shell drops any other line, such as `background-alpha = .5` or
`base-size = +12`, so that key stays unset and the theme's value applies.

#### Panel alpha

Panels follow the shell's own card alpha:

| Splinterm panel | Follows | Omarchy default |
| --- | --- | --- |
| Action palette, Dojo and session picker | `[menu] background-alpha` | 1.0 |
| Tab context menu, Dojo prompt, search bar, history overlay | `[popups] background-alpha` | 1.0 |

The action palette and pickers are list cards with a selected row, like the
shell's menus. The tab context menu, the Dojo prompt, the search bar and the
history overlay are small flyouts and status cards, which the shell draws with
`[popups]`.

A panel uses the theme background, the same colour as the window's own glass,
so Splinterm draws it as that glass with the shell alpha layered over it. Its
coverage is `1 - (1 - shell alpha) × (1 - terminal alpha)`:

* With Glassy Windows off the terminal is opaque, so panels stay opaque and the
  window never shows holes.
* With Glassy Windows on, a panel shows the blurred backdrop no more than the
  terminal around it does. Omarchy's default of 1.0 keeps panels opaque.

The docked Lair Explorer is not a floating panel but part of the Window, like
the tab strip: its background, selected row and active filter are drawn at the
terminal's own background alpha (foot.ini `alpha`, which Glassy Windows sets),
so the backdrop shows through it exactly as through the terminal beside it.
`[colors] explorer-alpha` and `explorer-background` change that; see
[Chrome backgrounds](#chrome-backgrounds).
Its text, icons, divider, rules and selection rail stay opaque. Like terminal
text, its text is not given the contrast floor below.

Panel borders, dividers, selected rows and text stay opaque. Because the
backdrop is an unknown wallpaper, Splinterm raises a panel's coverage to the
lowest value at which every foreground drawn on it keeps WCAG 4.5:1 contrast
against both a black and a white backdrop: primary and secondary text, text
on a row under the translucent hover or press highlight, and on popups also the
accent the Dojo prompt uses for its destructive choice. Those two backdrops
bound every other one. Row text under a highlight is first corrected to 4.5:1
against every highlight state, so a pressed row stays readable too. A theme
whose secondary text already sits at the 4.5:1 floor on the opaque panel, as
many light themes do, keeps its panels opaque. As in the
shell, a number outside 0.0–1.0 is clamped into that range, and a value that is
not a number is treated as Omarchy's opaque default; both are reported once. An
explicit `main.theme` JSON keeps all panels opaque and ignores these values.
Splinterm has no separate alpha key for these floating panels.

#### Chrome backgrounds

Splinterm draws its own chrome over the Window's glass. These `[colors]` keys
in `config.ini` override how. Every key is optional, and an absent key keeps
the default:

| Key | Default |
| --- | --- |
| `active-tab-background` | the theme's selected-tab colour (see the role table above) |
| `active-tab-alpha` | 1.0, opaque |
| `tab-strip-alpha` | the terminal background alpha |
| `explorer-background` | the terminal background |
| `explorer-alpha` | the terminal background alpha |

```ini
[colors]
active-tab-alpha=0.9
tab-strip-alpha=0.5
explorer-alpha=0.8
```

Alphas take 0.0–1.0 and colours take `#RRGGBB` or `0xRRGGBB`; anything else
fails startup with the line number. The keys apply to every theme source,
including an explicit `main.theme` JSON palette, and survive theme switches and
Glassy Windows toggles. A changed `active-tab-background` gets its label and
close glyph colour chosen again, whichever of the terminal background or
foreground has the higher contrast against it. A changed `explorer-background`
gets its text colours corrected to 4.5:1 against it, as the theme background
does. Alphas give no contrast guarantee against the backdrop, as for the
terminal itself. **Reload configuration** applies these keys at once, as it
does `alpha` and `blur`, and a key the file no longer sets gives the default
back.

#### Text size

`omarchy-display-text-size <9-20>` writes `[font] base-size` to the user
`shell.toml` and sets every terminal it knows to `px × 9/12` points, rounded to
a whole point: 12 px gives 9 pt and 16 px gives 12 pt. While Splinterm's own
`config.ini` sets none of `main.font-pixelsize`, `main.font-point-size` or the
deprecated `main.font-size`, Splinterm follows the same text size:

1. The user `~/.config/omarchy/shell.toml` `[font] base-size`, when set.
2. Otherwise the active theme's `shell.toml` `[font] base-size`. Omarchy's
   rendered template sets 12, so a stock Omarchy 4 desktop gives 9 pt.
3. Otherwise Splinterm's own default of 14 px.

Splinterm uses Omarchy's point size, so it matches Foot at the same text size:
Foot's default `dpi-aware=no` and Splinterm's default
`main.font-sizing-policy=output-scale` both size points at 96 DPI times the
output scale. 9 pt is 12 px at scale 1 and 24 px at scale 2. With
`main.font-sizing-policy=physical-dpi`, points use the output's physical DPI,
as Foot does with `dpi-aware=yes`.

A text size change rescales open terminal windows live, without a restart,
through the same reconciliation as a font family change. Runtime zoom stays a
half-point offset from the followed size, and `omarchy-display-text-size reset`
returns to the theme's size. Window chrome has its own text size, which also
follows `base-size` but never the terminal font size or runtime zoom; see
"Chrome text size" below.
Splinterm reads `base-size` as the shell does, with JavaScript's `parseInt` and
a 1 px floor, so `12.0` and `"12px"` both mean 12 px and any positive size is
accepted, not only the command's 9–20. The
resulting point size is clamped to the 6–96 pt Splinterm renders, which is
reported once. A value with no leading digits, such as `big`, is reported once:
an open window keeps its current size, and at startup Splinterm uses 12 px
(9 pt), as the shell falls back to its 12 px default. An explicit `main.theme`
JSON palette still follows the text size from the Omarchy `shell.toml` layers,
choosing the theme directory with the same order and brief-gap grace as the
native theme.
While `config.ini` sets both a terminal font size and `main.ui-font-pixelsize`,
Splinterm does not report `base-size` problems. While it sets a terminal font
size, the point-size clamp above, which only the terminal uses, is not
reported either. With a JSON palette Splinterm does not report panel alpha
problems. Bar size problems are reported in every case,
because every palette follows the bar.

To opt the terminal out, set a size in `config.ini`, for example:

```ini
[main]
font-pixelsize=14
```

#### Chrome text size

Window chrome is the text Splinterm draws itself: the tab strip (labels, agent
status badges, the close and new-tab glyphs and the remote tag), the Lair
Explorer, the command palette and its lists (key binding help, the Agent access
list, Revoke all access), menus, the Recent Dojos and remote pickers, Dojo
prompts and confirmations, frame titles, the scrollback search bar and the
upgrade banner. It uses the terminal's font family and icons at its own size,
in logical pixels:

1. `main.ui-font-pixelsize`, when `config.ini` sets it.
2. Otherwise Omarchy's text size, `[font] base-size`, read from the same
   `shell.toml` layers as above, in px as the Omarchy shell draws its own text
   (12 px on a stock Omarchy 4 desktop), even while `config.ini` sets the
   terminal's font size. It is clamped to 6–96 px.
3. Otherwise 14 px, the size chrome had with Splinterm's default terminal font.

Chrome is always sized by the output scale (14 px is 28 px at scale 2), whatever
`main.font-sizing-policy` says, and runtime zoom (`Ctrl++`, `Ctrl+-`) resizes
only the terminal. Chrome rows keep fixed heights, so chrome text grows with its
size only until it reaches a 24 px line height (logical pixels), then stays at
that size, so a large UI size is not clipped by its row. Tab text stops earlier
when the tab strip is shorter; see "Tab strip height" below. Frame titles
(`multiplexer.divider-style=frame`) sit in the pane's top border row, which is
one terminal cell tall, so they also stop at the terminal's cell height and
are cut to the chrome cells that fit between the frame's corners.

A `base-size` change redraws chrome in open terminal Windows without a restart,
as it does the terminal. **Reload configuration** applies a changed
`main.ui-font-pixelsize`, and removing the key returns chrome to Omarchy's text
size. Trusted dialogs (access consent, upgrade, restart) are not chrome in this
sense: their text is a terminal grid at the terminal font size, fitted to the
dialog's own Window.

```ini
[main]
ui-font-pixelsize=13
```

#### Tab strip height

The tab strip is as tall as the shell's top bar, computed the way the shell
computes it:

```text
height = round(size-horizontal × base-size / 12)   with scale-with-font on
height = size-horizontal                           with scale-with-font off
```

`size-horizontal` is `[bar] size-horizontal`, 26 in Omarchy's template and by
default. `base-size` is the `[font] base-size` described under "Text size",
12 by default. `scale-with-font` is `[bar] scale-with-font`, on by default. A
stock desktop therefore has a 26 px strip, and `omarchy-display-text-size 16`
makes it 35 px. The height is in logical pixels, so at output scale 1.25 the
default strip is 33 buffer pixels tall. Splinterm then clamps it to 20–48 px.

Each key follows the usual layering, and edits apply live, including the
`base-size` that `omarchy-display-text-size` writes. Open terminal windows move
their terminal grid with the strip, as hiding or showing the strip does. The
bar follows `base-size` even while `config.ini` sets a font size and Splinterm
stops following the text size.

Splinterm reads these keys as the shell does:

* `size-horizontal` uses JavaScript's `parseInt`, so `26.7` gives 26 and
  `"26px"` gives 26. A value with no leading digits, or one that is not
  positive, uses 26.
* `base-size` uses `parseInt` with a 1 px floor. A value with no leading digits
  scales the bar as 12 does.
* `scale-with-font` accepts `true`, `1`, `yes` and `on`, or `false`, `0`, `no`
  and `off`, in any case. Anything else keeps it on.

Rejected values are reported once. A clamp to 20–48 px is reported once when
the configured `size-horizontal` is itself outside that range. A height that
only `base-size` scaling pushes out of the range is clamped without a report,
so a text size change never reports a bar problem. Omarchy's template always
sets `size-horizontal = 26`. `[bar] size-vertical` sizes side bars and is
ignored. The bar height is not a palette setting, so an explicit `main.theme`
JSON palette follows it too, from the same Omarchy `shell.toml` layers it
reads the text size from.

Everything in the strip scales with it. The close and new-tab controls are
square at the strip height, the active-tab underline is 2 px for strips up to
29 px tall (3 px from 30 px, 4 px from 42 px), and the label inset is 10 px
at 34 px and proportional from there. Tab labels, agent status badges, the
close and new-tab glyphs and the remote tag use the chrome text size as other
chrome does (see "Chrome text size"), but their line height also stays within the strip less the
underline thickness above and below. At the default 26 px that is a 22 px
line, so text stays vertically centred and clear of the underline at every
size. Labels that do not fit end in `…` as before.
