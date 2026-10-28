# Human usage

This is the repository authority for operating Splinterm as a person: opening,
detaching, returning to, arranging, and deliberately ending persistent terminal
work. See [CLI reference](cli.md) for the complete command inventory and
[Automation](automation.md) for machine contracts.

## First five minutes

This walkthrough assumes Splinterm is installed, the built-in `splinterm`
keymap is selected without shortcut overrides, and
`multiplexer.persistent-by-default=yes` (the default). Check your
[configuration](configuration.md#keymap-configuration) if you use a different
profile. With persistence disabled, closing an ordinary unnamed terminal's
Window can end its work.

1. **Open a terminal.** Run `splinterm-xdg-terminal-exec` from another terminal.
   This creates a workspace (a **Lair**) containing one layout (a **Dojo**)
   with one terminal pane (a **Splint**).
2. **Split it.** Press `Ctrl+Shift+Enter` to add a second pane.
   Use `Ctrl+Shift+Arrow` to move between panes. Both panes belong to the same
   Dojo; leave their shells running for this walkthrough.
3. **Detach.** Press `Ctrl+Shift+Q` to close the Dojo's tab. In this fresh
   one-tab Window, the Window closes too. With persistence enabled, the daemon
   keeps both shells and their layout running.
4. **Return.** From another terminal, run `splinterm dojos` and select the
   running Dojo you just left. Its panes return with their existing work.
   `splinterm reopen` is a shortcut to the most recently remembered Dojo whose
   entire layout is still running.

Returning attaches to running work; it does not restart an exited shell.
Restoring an exited Dojo is a separate, explicit action that starts new
processes. Persistence here also requires the daemon to keep running; it does
not preserve processes across a daemon restart or reboot.

## The four persistent concepts

- **Lair** — a named project or persistent session.
- **Dojo** — one persistent terminal layout inside a Lair.
- **Splint** — one terminal pane and process lifecycle inside a Dojo.
- **Window** — a disposable native Wayland view. It may display up to 32 Dojos as
  client-local tabs, including Dojos from different Lairs.

`splinterd` owns Lairs, Dojos, Splints, shells, layouts, terminal state, and
scrollback. `splinterm` normally displays and controls that durable state. A
Window may instead own an ordinary unnamed transient Lair when
`multiplexer.persistent-by-default=no`; closing that Window terminates and
removes the unpromoted Lair. Closing a tab alone remains a view operation.

## Start, detach, and return

Open a fresh terminal through the installed desktop entry or XDG launcher:

```bash
splinterm-xdg-terminal-exec
```

The normal launch creates a fresh Lair with one Dojo and one live Splint. The
compatibility default is persistent, so closing the Window leaves the work
running in `splinterd`. Set `multiplexer.persistent-by-default=no` to make
ordinary unnamed desktop, bare launch, picker New, and in-Window New Terminal
Lairs end with their owning Window.

Explicitly named or command-bearing native launches, presets, remote creation,
restore/relaunch, automation, and MCP stay persistent. With
`multiplexer.persist-on-tab-organization=yes`, creating another Dojo or
explicitly naming/renaming a Dojo permanently promotes its transient Lair before
the tab mutation commits.

Return through the native recent-Dojo workflow:

```bash
splinterm dojos
splinterm reopen
```

`dojos` opens the trusted Recent Dojos picker. Select a running Dojo or choose
New Terminal. `sessions` remains a compatibility alias for `dojos`. `reopen`
attaches the most recently remembered Dojo whose complete layout is still
running. Neither command silently restores an exited
process.

To inspect Lairs and Dojos without mapping a Window:

```bash
splinterm list
splinterm list --all
```

`list` emphasizes active Lairs; `--all` includes exited-only history and complete
topology. Stable IDs from list/topology output can select an exact attachment:

```bash
splinterm launch --splint-id SPLINT_ID
splinterm window --lair-id LAIR_ID --dojo-id DOJO_ID
```

A normal `splinterm launch` creates a fresh graphical Lair. `window` requires the
explicit Lair/Dojo pair and renders that saved layout.

## Windows, tabs, and the Dojo picker

A Window holds an ordered, client-local set of distinct Dojo tabs:

- `Ctrl+Shift+D` creates a Dojo in the active tab's Lair and opens it;
- `Ctrl+Tab` and `Ctrl+Shift+Tab` move through tabs;
- `Ctrl+Shift+1` … `Ctrl+Shift+9` jump to the first nine tabs;
- `Ctrl+Shift+Q` detaches the active tab and closes the Window if it was last;
- `Ctrl+Shift+B` toggles the Dojo tab strip without changing the tab set;
- `Ctrl+Shift+S` opens the `dojos` picker (recent) inside the same Window;
- choosing an already-open Dojo activates its tab rather than duplicating it;
- choosing another running Dojo adds it without changing daemon topology;
- choosing New Terminal creates a fresh Lair and opens its initial Dojo; and
- a Dojo an agent or other automation creates appears as a background tab in
  the Window you are using when its active tab is in that Lair, or else in
  another Window showing the Lair. It never becomes the active tab or takes
  keyboard focus, and only one Window gets it.

The command palette keeps topology levels separate: **Choose Lair** lists one
row per available Lair and switches through that Lair's most recently used Dojo,
while **Choose Dojo** lists only Dojos from the active Lair. **Recent Dojos**
remains the global cross-Lair workflow. Their New actions create a Lair, a Dojo
in the active Lair, and a fresh terminal Lair respectively.

Recent Dojos and Choose Dojo are one `dojos` picker with two scopes, shown
as `recent · this lair` in its frame: **Tab** switches between them and keeps
what you typed. Each row is one line: the name, where it is (its Lair and
working directory), and at the end the agent's state with its coloured dot
(`needs input`, `working`, `done`) or how many of its panes run (`1/2`).
Running Dojos list under `running`, stopped ones under `stopped` without a
count. In a narrow Window the location gives way first, then the state's
word; the dot stays.

Typing in any picker (`dojos`, `lairs`, `remotes`) filters at once; there is
no `/` first, so `j` and `k` are letters, not moves. Arrows, Page Up and Page
Down, Home and End move; Enter opens; `Ctrl+N` chooses the `+` row; `Ctrl+U`
clears what you typed; Escape clears it, then closes. While you type, the `+`
row and the headers give way to the matches, and the top match is selected.
A picker still waiting on the daemon after 150 ms shows the spinner and
*loading* alone; one that failed to load says so and `R` asks again.
`splinterm dojos` lists Dojos most recent first, ungrouped.

The trusted tab strip provides activation, a close target, and a `+` picker
action. It is visible by default; hiding it is local to the current Window,
reclaims its height for the pane grid, and does not persist or reappear when a
new tab opens. Right-clicking a visible tab opens a menu titled with that tab's
Dojo: Rename tab…, Activate tab, New Dojo, then detach-only Close tab and Close
other tabs, then confirmed Terminate Dojo… last, in the danger colour. Opening
the menu does not first activate its tab.

Menus open at the pointer, in the same box as the palette and pickers but with
no scrim and no key hints in the bottom edge: the frame names what the menu acts
on, and separators group its actions. When the menu is for the current tab,
Lair or pane, an action with a key binding shows it as chips (`ctrl` `shift`
`q`); a menu for anything else shows none, since those keys act on the current
one. Arrows, Enter and Escape still work.

**Detach and terminate are different operations.** Closing tabs or Windows is
client-local. Terminating a Dojo is a named, confirmed daemon mutation and ends
its pane processes.

## Lair Explorer

The optional Explorer shows Lairs, their Dojos, and individual Splints beside
terminal panes. Open the command palette (`Ctrl+Shift+P` in the built-in keymap)
and choose **Toggle Lair explorer** to show or hide it, or **Focus Lair explorer**
to show it and move keyboard focus there. These Explorer actions have no default
shortcut. **Return focus to terminal** moves focus back without hiding Explorer.

When the Window is wide enough to dock it, the Explorer runs the Window's full
height on the left, and the tab strip starts at its right edge, above the
terminal panes only. `[explorer] full-height=no` in `config.ini` puts it below a
full-width tab strip instead. In a Window too narrow to dock it, the Explorer
opens over the terminal, below the tab strip.

The Explorer can collapse to a rail about four cells wide; it widens with
larger chrome text. Click `«` on the Explorer's footer action line to
collapse it and `»` at the top of the rail to expand it again, or choose
**Toggle Lair explorer rail** in the command palette, which also shows a hidden
Explorer as its rail. There is no default shortcut, and clicking `«` or `»`
leaves the keyboard where it was. The rail has one row per Lair under the
chosen filter: the Lair's initial (when its name has no letter or digit, its
place among all Lairs, which no filter changes) and, while any Splint reports
agent status, the Lair's most urgent status glyph. An accent bar marks the
current Lair. A `!` left of the `»` means navigation has a notice or is
loading; expand the Explorer to read it. A muted `s` or `a` right of the `»`
names the Saved or All filter, so a short rail is not mistaken for every
Lair; under Live, the default, there is no mark. Clicking a rail row switches
to that Lair: it opens the Lair's active Dojo, else one open in a tab here,
else a live one, else the first that opens without asking. It never restores a
Dojo: a Lair whose Dojos only restore expands the Explorer instead, with that
Lair selected and open, so you choose the Dojo to restore there. Like any row
click, it gives the Explorer the keys, which return
to the terminal once a switch completes. Right-click a rail row for the
Lair's menu. With the rail focused, Up/Down, `j`/`k`, PageUp/PageDown and
Home/End move between Lairs, Enter switches, `f` cycles the filter, `r` selects
the current Lair, and Escape returns to the terminal; unfocused, the wheel
scrolls the rail. The rail has no search; a query left in the full Explorer
waits there until it expands. The terminal widens as the Explorer collapses,
and the tab strip beside a full-height Explorer moves with it. A Window too
narrow to dock the full Explorer still docks the rail; when even the rail does
not fit, the rail opens over the terminal's left edge. A hidden Explorer is
shown again collapsed or expanded as it was, for as long as the Window is open.
`[explorer] collapsed=yes` makes new Windows show the rail first.

Right-click a row for actions on that exact item, without selecting or opening it.
With Explorer focused, Menu or Shift+F10 opens the selected row's menu; use
Up/Down, Enter, and Escape to choose, run, or cancel an action:

- **Lairs:** Rename, New Dojo, Save layout, Pin/Unpin, saved-layout Preview,
  Restore, and Terminate.
- **Dojos:** Open, Rename, Restore, Close tab when open, and Terminate.
- **Splints:** Focus, Use app name for a renamed Splint, Split below/right,
  Restore for restorable work, and Close.

Only applicable actions appear. Saved and non-open Lairs/Dojos can be renamed or
managed without switching tabs; Open and Focus explicitly navigate.
New Dojo opens the newly created tab. Save layout retains a disposable Lair;
unpinning leaves it saved. Splitting and closing require an attached running pane
controlled by this window. Saved-only Lairs and Dojos are restored, not deleted;
termination requires at least one live pane and a complete captured incarnation
set, including exited panes. Rename dialogs use current authoritative names, and
Restore/Terminate require confirmation with Cancel selected initially. Escape
cancels a menu or dialog.
Stale targets are rejected rather than redirected to the active item.

Prompts sit centred over the dimmed Window, in the same box as the palette,
with their keys in the bottom edge. A rename is a sunken field holding the
current name, selected so typing replaces it, and the frame names what it
renames: Enter saves, Escape cancels. A confirmation asks its question over
`[ Cancel ]` and the confirming button, right-aligned; the arrows or Tab
choose, Enter takes the chosen button, Escape cancels. A chosen Terminate is
filled in the danger colour. A saved-layout preview lists each pane as a row
of Dojo, launch recipe and directory. A Window too small to show a prompt,
or too narrow for its buttons, cancels it.

Launcher-generated `terminal-<timestamp>-<pid>` names appear as compact **Lair 1**,
**Lair 2**, etc. Generated or empty Dojo and pane labels similarly fall back to
**Dojo N** and **Terminal N**. Meaningful custom names are preserved, and aliases
skip names already used by siblings. These are display labels, not renamed saved
objects or CLI identifiers; numbering follows the current topology and can change
when siblings are added or removed. Search, the navigation pickers, and the
standalone Recent Dojos picker (`splinterm dojos`) use the same labels.

A Splint that still has the default title `shell` is labelled with the program
running in it, such as `nvim`, `claude`, or `build.sh`, once that program has
held the terminal for 2 seconds, so a quick `ls` never shows. The label goes
back to `shell` as soon as the shell has the terminal again. Splinterm names
the job from its command, as a basename only: a script run by a shell or an
interpreter such as `python3` shows the script's name. A renamed Splint keeps
its name, even one renamed to `shell`. To go back to the app name, choose
**Use app name** from the Splint's menu in the Explorer, run
`splinterm rename-splint SPLINT_ID --use-app-name`, or call MCP
`splinterm.rename_splint` with `use_app_name: true`. The name is only a label:
any program can choose its own command line, so no command or agent targets
or trusts a Splint by it, and it is not saved. The agents section and Explorer
search use the same label; pane frames, tabs, and `splinterm list` keep the
title.

Each Explorer row is one line, or two with `row-lines=2` (below). On the left
are an icon marking the current row (an eye unless `[explorer] current-marker`
picks another icon), a chevron icon before each Lair and Dojo (pointing right
when collapsed and down when expanded), and the name. On the right, fixed icon
columns line up on every row:

| Column | Shows |
| --- | --- |
| Agent access | `plug` when an agent can reach the row, `plug-connected` when one is connected, `lock-question` when an agent is asking for access to the Lair |
| Agent status | the agent's state (see [below](#agent-status-in-the-explorer-and-on-dojo-tabs)) |
| Live | a filled dot `●` while a Splint in the row is running or starting, a hollow dot `○` otherwise |
| Retention | `pin` for a saved Lair, `pinned` for a pinned Lair |

A column appears only while some row in the Explorer uses it, except the live
dot, which is always there. A long name is ellipsized before the first icon in
its row; it never runs under one.

**Two-line rows.** With `[explorer] row-lines=2`, every row of the expanded
Explorer and of its agents section is two lines tall. A Splint row's second
line, muted under its name, shows the agent's detail while the Splint reports
one and agent status is shown, else the Splint's working directory with your
home directory written `~`; a remote Window writes the remote host's
directories in full. A long detail keeps its first words. A long directory
keeps its start and its last components, `~/…/crates/splinterm`. The
directory is where the Splint was started or last relaunched; it does not
follow a `cd` in its shell. Lair and Dojo rows centre their one line, and the
rail keeps one line per Lair. A list too short for one two-line row shows
one-line rows instead, so the selection stays in view. The selection, a
click, and a chevron's target cover both lines. Assistive technology reads
the directory with the row, for example *Running · in ~/Projects/splinterm*.
The agents section counts lines: its label is one line and each of its rows
two. A configuration reload that changes `row-lines` lays the rows out again
at once.

The footer has two lines. The first is an action line, `new · menu · «`:
**new** makes a new Lair from the focused Splint's directory, as **New
terminal** does; **menu** opens the selected row's menu, the one a
right-click opens, or the command palette when no row is selected or the row
has nothing on its menu, and does nothing while the row's menu is unavailable
during a refresh or a disconnect; and **«** collapses the Explorer to its
rail. A token runs when the press and the release both land on it. Clicking
one, or anywhere else on the footer, leaves the keyboard where it was (a menu
that `menu` opens has the keys until it closes). The tokens have no default
shortcut, and their width follows the UI font. In a narrow panel `menu` goes
first, then `new`; `«` stays longest.

The footer's second line shows, in this order, the search field while you
search, a navigation notice such as *Refreshing navigation…* when there is
one, a search query left behind (*Search: …*, which still narrows the rows),
the selected row's short status, and otherwise who has the keyboard: *↑↓ move
· F filter · Esc terminal* while the Explorer has focus, *Terminal has keys ·
click to move* while the terminal does. Clicking a row gives the Explorer the
keys. The short status says only what the row does not already show, in
this order: a waiting access request (*asking for access*), agent states
(needs input, then error, working, done, idle), a lifecycle out of the
ordinary, the retention or *open here*, why the row cannot be opened, and
which agents can reach it, for example
*1 needs input · 2 working · saved · agent connected*. It never repeats the
name, which is highlighted just above, and it leaves out the ordinary: a
running row's lifecycle, a disposable Lair's retention (every new Lair starts
disposable), and the current Dojo's attachment, so a plain running row shows
the keyboard hint instead. The status never wraps; what does not fit ends in
an ellipsis after the last whole word that fits, so an agent's detail keeps
its first words, for example *needs input · waiting for…*. Assistive
technology still reads the full words for every row, ordinary states
included, for example *Agent connected · Running · Saved · 1 working*.

Parent rows summarize runtime state independently of retention and window
attachment. A saved Lair can still be running, and a Dojo not open in this
Window can be running too. The current-row marker (an eye unless
`[explorer] current-marker` picks another icon) marks the active Dojo, the
footer says *open here* for another Dojo attached to this Window as a tab,
and it says nothing about attachment otherwise. Assistive technology reads
**Current**, **Open here**, or **Not open here** for every Dojo. A stopped
restorable Dojo opens a restore confirmation rather than reporting a permission
failure. Detached starting or mixed-state Dojos still cannot be attached;
Explorer explains this lifecycle restriction instead of calling it a denial.

The header shows the `lairs` title and the `live · saved · all` filters on
one line, with the chosen filter in bold, initially **Live**. Beside the tab
strip, the header is the strip's height. **Live** keeps
running or starting Splints and their parent context; a mixed parent containing
only stopped or unavailable Splints is not live. **Saved** keeps saved and pinned Lairs,
including any live work inside them. These filters preserve the authoritative
Lair → Dojo → Splint order; pinning adds a retention icon, not refresh-driven
sorting. Branches begin collapsed, and refreshing preserves disclosure choices.
Label search looks through every Lair, whatever the filter, so a saved or
stopped Lair is found while Live is chosen; clearing the search returns to the
chosen filter. The accessibility status names
the active filter and announces the keyboard shortcut.

While Explorer has focus:

- Use Up/Down to select a row; PageUp/PageDown move by a page and Home/End select
  the first/last row. Outside search, `j`/`k` also move the selection.
- Outside search, use Right to expand a Lair or Dojo, then enter its first child.
  Left collapses an expanded Lair or Dojo before moving to its parent. Space
  toggles a Lair or Dojo's disclosure without activating terminal work.
- Press Enter on a Lair to toggle disclosure, on a Dojo to open it, or on a live
  Splint to focus its pane. Selecting a row alone does not activate it: the
  **selected** row is the navigation target, while **current** identifies the
  terminal work already active in the Window.
- Outside search, press `f` to cycle Live / Saved / All, or click a header filter.
  If filtering hides the selected row, selection moves to a visible row without
  activating it. Hidden rows cannot receive navigation actions.
- Press `/` or `Ctrl+F` to search navigation labels, not terminal output or
  scrollback. Matching descendants retain their parent context. Escape clears
  a nonempty query; Escape with an empty query returns focus to the terminal.
- Outside search, `r` reveals the current work, resetting to All and clearing
  the label query, or retries a failed refresh when the Explorer offers retry.

Unavailable targets cannot be activated. Restorable exited work enters the
existing preview/confirmation flow; it is not silently restarted by selection.
Restoring starts new processes, not a checkpoint of the old applications. A
foreground palette or confirmation owns input rather than the Explorer search
field behind it.

[Navigation accessibility](accessibility.md) describes the bounded native AT-SPI
interface and its limitations. The guide is included in the 0.1.1 Arch and Nix
package documentation. This is not whole-terminal screen-reader support.

### Agent status in the Explorer and on Dojo tabs

An agent running in a Splint can report whether it is working, needs you, or has
finished, through `splinterm agent-status`, the MCP tool, or an agent hook (see
[automation](automation.md#agent-status)). Splinterm shows only what was
explicitly reported. It never guesses from terminal text, titles, prompts, or
output, and a status is display data: it cannot approve, type, or take control.

| State | Icon | ASCII fallback | Dot | Colour | Text |
| --- | --- | --- | --- | --- | --- |
| working | a Braille spinner, `⠋` at rest | the same spinner | the same spinner | warning | Working |
| needs input | `message-question` (a speech bubble with `?`) | `?` | `◆` | danger | Needs input |
| done | `circle-check` | `+` | `■` | info | Done |
| error | `alert-circle` (a circle with `!`) | `!` | `▲` | danger | Error |
| idle | `point` (a small ring) | `-` | `□` | success | Idle |
| stale marker | `clock` | `~` | `~` | faint, for the glyph too | stale (no report for N min) |

The icons are a small subset of [Tabler Icons](https://tabler.io/icons) (MIT)
built into Splinterm, drawn at the interface font's ascender height in the
colour of their state's role (see
[Status colours](configuration.md#status-colours)), or in the row's text
colour with `[explorer] status-colors=no`. `[explorer] status-glyphs=dots`
draws the one-cell dots from the interface font instead; when the
interface font itself does not draw all four dots, Splinterm says so once
on standard error and shows the ASCII fallbacks, so needs input and error
never share a missing-glyph box. The access "Asking" marker is drawn in
warning. Every state differs by shape and by text, never by colour or
motion alone. The icons do not depend on the interface font; if the
built-in icon data could not be loaded, Splinterm would show the ASCII
fallbacks instead of a missing-glyph box. Icons are visual only:
accessible text and search use the state words.

A fresh **working** status is a Braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`), the one moving
part, in the Explorer and on Dojo tabs. Splinterm draws Braille itself, so it
needs neither the icon data nor a font. It steps at most four times a second
on the same event-loop wakeup as the cursor blink, and only while the Window
has keyboard focus: in an unfocused Window, with `SPLINTERM_REDUCED_MOTION=1`,
and for a stale working status it holds its first frame, `⠋`. Every other
state is a static icon.

- **Splint rows** show the icon in the agent status column, and the footer's
  status line names the selected row's state, for example *needs input ·
  waiting for approval*, or *working · stale 2 min* for a stale report. The
  agent's free-text detail appears only there and in the row's accessible
  words, ellipsized when the line is too narrow.
- **Dojo and Lair rows** show the most urgent icon of the Splints inside them,
  and the footer counts the states that some Splint is in, most urgent first,
  for example *1 needs input · 2 working*. Their accessible words join the
  counts with ", ", for example *Running · Current · 1 needs input, 2
  working*. Priority is needs input, then error, working, done, and idle;
  stale statuses rank last. The icon column appears while any Splint in the
  Explorer has a status.
- **Agents section.** Below the Lair tree and above the footer, a rule and the
  label *agents* start a list of every Splint that reports a status, whatever
  the filter or the search: most urgent first (needs input, error, working,
  done, idle, then stale statuses), in tree order within a state. Each row
  shows the status icon, the Splint's name (the app's name for a Splint left
  on its default title, as in the tree), and its Lair and Dojo, for example
  *work / editor*, when the panel has room; otherwise the location is
  ellipsized or dropped and the name is kept. The section takes up to five
  rows and at most a third of the list, but at least one, under its label
  line, and the tree scrolls in the rows left above it; a list shorter than
  three rows has no section. When more Splints report than it shows, the
  section scrolls with its selection and its label reads *+k more*; while the
  selection is elsewhere it shows from its top, so the most urgent Splint is
  in view. Clicking a row focuses that Splint, as its row in the tree does,
  even when the tree does not show it, under a closed Lair or past a search,
  and right-click opens the Splint's menu. A click on the *agents* label line
  leaves the keys where they are. With the keyboard, Down from the tree's last
  row moves into the section and Up from its first row moves back; End goes
  to its last row, and Left and Right do nothing there. When a selected row
  leaves the section, the row now in its place is selected; with none left,
  the Splint's row in the tree, else its Lair's. The section is hidden on the
  rail, with `agent-status=no`, while agent status is unavailable, when
  nothing reports, and in a list too short for it; the keys then skip it.
  Assistive technology reads it as an *Agents* group after the tree, whose
  items are its rows, and leaves the group out while the section is hidden.
- **Dojo tabs** show one badge before the label: the most urgent icon, with a
  count when several Splints share it (the needs-input icon followed by `2`).
  The badge takes space from the label, never from the close control, so on a
  narrow tab the label is shortened first and the badge is the last thing
  dropped. Tabs never show the detail text.
- **Stale.** `working` and `needs input` expire unless the agent reports again,
  after two minutes by default. Splinterm then keeps the last icon (a stale
  working spinner stops on its first frame) and adds the clock icon after it
  in the Explorer column and the tab badge; the Explorer's accessible words
  read *Working · stale (no report for 2 min)* and its footer *working ·
  stale 2 min*. A stale status is dropped after 15 more minutes.
- **Unavailable.** While the Explorer's view of the daemon is stale or
  disconnected, the words read *Agent status unavailable* and the column is
  blank instead of showing the last value.
- **Search.** Explorer search also matches state words, so `needs input` (or
  `needs_input`, `error`, `done`, `stale`) filters to those Splints. It does not
  match the detail text.
- **Clear.** The palette command **Clear agent status** clears the focused
  Splint's status. It is offered only while that Splint shows one. Splinterm's
  own interface can clear a status but never set one.

Relaunching or restoring a Splint starts with no status, and so does every Splint
after a daemon restart or live upgrade until its agent reports again. To hide all
agent status presentation, set `agent-status=no` in the `[explorer]` section of
the [configuration](configuration.md); **Reload configuration** applies it.

The Explorer's accessible item states include the state in words, and a polite
announcement is made only when a visible Splint enters needs input, error, or
done, or stops being stale. Changes to and from working are not announced.
`SPLINTERM_REDUCED_MOTION=1` stops the working spinner, as it stops the cursor
blink; Splinterm does not yet read the desktop's reduce-animation setting.

## Panes and layouts

The built-in application controls are:

| Action | Default control |
| --- | --- |
| Command palette | `Ctrl+Shift+P` |
| Recent Dojos | `Ctrl+Shift+S` |
| Split below | `Ctrl+Shift+Enter` |
| Split right | `Ctrl+Shift+\` |
| Focus pane | `Ctrl+Shift+Arrow` |
| Close focused managed pane | `Ctrl+Shift+W` |
| Resize pane smaller/larger | `Ctrl+Shift+[` / `Ctrl+Shift+]` |
| New Dojo tab | `Ctrl+Shift+D` |
| Previous/next Dojo tab | `Ctrl+Shift+Tab` / `Ctrl+Tab` |
| Detach active tab | `Ctrl+Shift+Q` |
| Toggle Dojo tab strip | `Ctrl+Shift+B` (`Prefix+B` in `omarchy-tmux`) |
| Search scrollback | `Ctrl+Shift+F` |
| Page history | `Shift+PageUp` / `Shift+PageDown` |
| Return to live output | `Shift+End`, or plain Enter while viewing history |
| Copy / paste | `Ctrl+Shift+C` / `Ctrl+Shift+V` (`Super+C/V`, `Ctrl+Insert`/`Shift+Insert` aliases) |
| Release control | `Ctrl+Shift+L` |
| Request control transfer | `Ctrl+Shift+T` |
| Accept / deny transfer | `Ctrl+Shift+Y` / `Ctrl+Shift+N` |
| Open forced-takeover confirmation | `Ctrl+Shift+U` |
| Revoke active access | `Ctrl+Shift+R` |
| Zoom terminal text in / out / reset (tabs, menus and other chrome keep their size; see [Chrome text size](configuration.md#chrome-text-size)) | `Ctrl++` / `Ctrl+-` / `Ctrl+0` |

Application-owned chords are consumed rather than forwarded to the PTY. In a
legacy direct single-Splint attachment, `Ctrl+Shift+W` remains terminal input;
it closes only a focused Splint in a managed multi-pane Window.

A split needs room. Every Splint keeps at least two cells of terminal text across
and down, inside its frame or beside its divider. A split that would leave less
in the current Window is refused before it reaches the daemon: the Window title
says so briefly, and nothing opens or takes focus. If a Window becomes too small
for its Splints anyway, after a smaller tile, a larger scale or font, a docked
Explorer, or a split made from another Window, the pane gap shrinks first. When
the Splints cannot fit even without a gap, the Window shows only the focused
Splint over the whole pane area, as if zoomed, until there is room again. Focus
keys still move between the Splints, showing the one focused; the others keep
running at their last size.

The command palette is a curated trusted subset of the same typed action registry
used by keyboard bindings and projects shortcut labels from the effective
resolved keymap. It exposes keybinding help, configuration reload, copy mode,
focused-pane zoom, Dojo and Lair selection/navigation/management, and Window
detach alongside the existing pane, history, view, and control actions. It labels
unavailable actions rather than guessing or retargeting a resource. Type to
filter, use arrows to navigate, Enter to run, and Escape to close.

See [Configuration](configuration.md#keymap-configuration) for the strict built-in
profile and TOML overlay. Configuration can bind only closed application actions;
it cannot register shell commands or callbacks.

With the `omarchy-tmux` profile, `Prefix+B` toggles the Dojo tab strip,
`Prefix+?` opens trusted searchable help generated from the resolved keymap, and
`Prefix+[` enters client-local vi copy mode. In keybinding help, type to search
labels, configuration names, shortcuts, sources, and closed keywords; use
Up/Down or PageUp/PageDown to navigate, `Ctrl+U` to clear, and Escape once to
clear a query or again to close.
Move with `h/j/k/l`, arrows, `w`/`b`/`e` word motions, `0`/`$` (or Home/End),
or PageUp/PageDown; press `v` to begin a selection, `y` or `Super+C` to publish
it to the Wayland clipboard and exit, or
Escape to cancel. Copy-mode `Super+V/X/Z` are consumed locally; copy mode never
forwards these keys, pointer input, paste, or IME text to the terminal
application. Outside copy mode, both built-in profiles provide terminal
`Ctrl+Shift+C/V` and `Super+C/V` copy/paste. The `omarchy-tmux` profile advertises
`Ctrl+Shift+C/V` as its primary compositor-safe shortcuts; the `splinterm`
profile retains `Super+C/V` as primary. When `com.oldjobobo.splinterm` carries
Omarchy's `terminal` tag, current Omarchy's universal shortcuts (`quattro` since
`129c67e2`, after 4.0.4) arrive as `Ctrl+Shift+C/V`, and Omarchy 3.x and 4.0.x
send `Ctrl+Insert`/`Shift+Insert`; both profiles accept both. Run `splinterm integration omarchy enable` once to get
the tag ([why](integrations.md#why-the-terminal-tag-matters)). Without the
terminal classification Omarchy injects ordinary `Ctrl+C`, which remains
terminal interrupt. These Super shortcuts work only when the compositor
delivers the chord to the Splinterm Window. Splinterm-owned command-palette,
keybinding-help, search, and rename fields accept effective clipboard bindings
such as `Ctrl+Shift+C/V` and also offer bounded local selection, cut, paste, and
undo; terminal `Super+X/Z` remain owned
by the running application.

The same profile ships atomic `omarchy.t`, `omarchy.tdl`, `omarchy.tds`,
`omarchy.tdlm`, and `omarchy.tsl` presets. Optional Bash functions use the
separate `s`, `sdl`, `sds`, `sdlm`, and `ssl` names and install only through an
explicit collision-safe workflow. See [Presets](presets.md) for exact behavior.

## Pointer, selection, clipboard, and URLs

- Click a pane to focus it; click a tab to activate it.
- Drag terminal text to select. Selection is client-local presentation state.
- `Ctrl+Shift+C` copies the active selection and `Ctrl+Shift+V` pastes clipboard
  text through the controlled terminal input path.
- Primary selection, URL recognition/activation, and pointer reporting follow the
  implemented Foot-derived terminal behavior and current control ownership.
- Right-clicking trusted tab chrome opens the tab menu; terminal-controlled
  content cannot paint or activate trusted chrome.

Clipboard and primary-selection contents are terminal data, not authority.
Pasting requires the graphical client to hold terminal input control.

### Save a clipboard screenshot as a path

Available in the 0.1.1 source (publication pending): configure a private existing
`[clipboard] image-directory`, then choose **Save clipboard image and insert path**
from the command palette. The bindable action is `clipboard.save-image`; there is
no default shortcut. Only bounded static PNG is supported, and normal text paste
is unchanged. This action works only in a focused, controlled, live local pane—not
remote sessions, copy mode, or historical scrollback.

The saved PNG is private and persistent. Its shell-escaped path is inserted without
Enter. A changed pane or busy input channel never redirects or defers the path;
a PNG already published is retained and its path reported through the temporary
Window title and stderr. Delete saved files yourself when no longer needed.
See [Clipboard PNG configuration](configuration.md#clipboard-png-saving)
for limits, permissions, metadata preservation, cancellation, and filesystem requirements.

## Scrollback and search

While the focused pane is viewing historical output, plain Enter or keypad Enter
returns it to live output and sends no PTY bytes; a later Enter pressed while
already live submits normally. `Ctrl+Shift+F` opens local literal scrollback
search. Enter submits, `Ctrl+N` and
`Ctrl+P` navigate matches, and Escape closes the trusted search surface. Search
queries are not injected into the terminal.

The search box sits at the bottom left of the terminal area. Its frame holds
the current match and the count (*2/17*), and its bottom edge its keys. The
current match is selected and outlined in the accent. The daemon searches a
few milliseconds of history per request, so a busy terminal never stalls.
Until a new query is answered, or while no match has turned up, the frame
shows *searching…* and the Window keeps asking for the next stretch. Paging
through matches already in history keeps working while the terminal prints.
The results clear and Enter searches again when history is replaced, for
example after a resize reflows it or the program clears it, or when a page's
match on the visible screen has changed.

History paging remains bounded and preserves follow-live behavior when
returning with `Shift+End`, which does nothing when the pane is already live.
While a pane shows history, a box at the top right of the terminal area says
how far back it is and how many rows arrived since (*↑ 17/351  +4 new*), with
the return keys as chips. A click anywhere on the box returns to live output.

During an upgrade, a banner at the top centre of the terminal area, or below
the history box in a narrow Window, shows a spinner while the upgrade runs. It
turns to the danger colour if the connection is lost, and settles on `!` in
the warning colour when the upgrade is cancelled or rolls back to the old
daemon.

The CLI can also read bounded history pages and search without mapping a Window:

```bash
splinterm scrollback SPLINT_ID --max-rows 32
splinterm search SPLINT_ID 'literal text'
```

Use opaque continuation cursors returned by machine output rather than inventing
positions. See [CLI reference](cli.md#terminal-observation-and-input).

## Control ownership

Only one connection controls a live Splint at a time. Other Windows may remain
observers. Request/release/accept/deny operations are explicit, and forced
takeover is a separate trusted-local confirmation. A remote graphical Window may
request normal control but cannot use trusted-only forced takeover.

Closing a controlling Window releases its connection-owned lease. A later client
must acquire control explicitly; observation never silently becomes input
authority.

## Trusted dialogs

Three small trusted dialogs ask questions that only a person may answer: the
access prompt for an agent ([Automation](automation.md#the-prompt)), the
`splinterm upgrade --now` question ([CLI](cli.md)), and the desktop launcher's
restart question ([Packaging](packaging.md)). Typing meant for another Window
never answers one: its keys stay inert until you engage it, as described below.
Under Omarchy with `splinterm integration omarchy enable`, each also appears
without taking keyboard focus ([Integrations](integrations.md)).

Each dialog is one box: a thin frame with its title set into the top edge,
its text, and two buttons at the bottom, a red no button on the left and a
green yes button on the right that names exactly what it does, such as
**Allow Look once** or **Restart now, end 4 shells**. Under each button's
label are the keys that give that answer. Only a click inside the green
button says yes; a click anywhere else, even beside it, does not. The frame's
bottom edge says how to engage the keyboard until you do, then which keys
answer. The box casts a hard shadow, as the popups inside a Window do. Each
dialog shows the time left to answer at the right end of the top edge, such
as `denies in 0:19`, `cancels in 0:42`, or `not now in 1:58`. When the
access prompt's time runs out, its Allow button is disabled and nothing
grants, then it closes and the request ends unanswered. The upgrade and
restart questions end as they always have. Trusted dialogs always use the
default, even padding, whatever `[main] padding-*` says.

Each dialog asks for one fixed size: the size at which all of its text fits
above the buttons, but never larger than the screen space the compositor
offers. Its minimum and maximum size are the same, so tiling
compositors such as Hyprland and sway float it instead of tiling it; no window
rule is needed. If a compositor gives the dialog another size anyway, its text
wraps again to fit, at spaces, never inside a word unless the word is wider
than the dialog. If the text still does not fit, the font gets smaller, down to
70% of your configured size.

Only below that does the dialog leave out lines. It then shows "…" where text
is missing, and in place of the key hints it says that part of the request is
hidden. A button label too long for its half of the dialog counts as
hidden text too, and so does the access prompt's countdown when the top edge
has no room for it beside the title. While any text is hidden, the dialog cannot grant, proceed,
or restart: `G`, `R`, and the green button (drawn dimmed) do nothing until you
make the window larger. `Enter`, `Escape`, `D`, and the red button still say
no. Once all of the text shows again, and whenever the dialog is laid out again
for a new size, keys and the green button count only after the same short
delay.

A dialog's keyboard stays inert until you engage it, even if your desktop
gives it focus:

- **Click inside it**, or
- press **Ctrl+Alt+G**. Under Omarchy with `splinterm integration omarchy
  enable` (or its dialog rule in your own Hyprland file; see
  [Integrations](integrations.md)), Ctrl+Alt+G works from any Window while a
  dialog waits: it focuses the waiting dialog and engages it. Anywhere else it
  engages a dialog that already has keyboard focus.

Ctrl+Alt+G never answers. After either one, keys and the proceed or grant
click count only about 600 ms later, and then `G` grants or proceeds (`R`
restarts in the launcher's question) while `D`, `Escape`, or `Enter` deny or
cancel. Once armed, a key acts when it goes down: a key held down since before
arming never counts, but one pressed after arming counts as a press even while
it is held. Key repeats never count, and no key answers while Ctrl, Alt, or
Super is held. Until you engage it, the dialog's bottom edge names the
shortcut.

Splinterm enables its Ctrl+Alt+G binding in Hyprland only while a dialog waits
and disables it when the last one closes, however it closes, so applications
keep the chord the rest of the time. Even while enabled, the binding lets the
chord through to the focused application whenever no dialog is on screen, and
it only ever focuses a Splinterm trusted dialog. When your own Hyprland
configuration already binds Ctrl+Alt+G, in any submap, Splinterm leaves it
alone and binds nothing; the chord then still engages a dialog that has focus.
Choose another Ctrl+Alt chord, or turn the shortcut off, with
[`[integration] dialog-shortcut`](configuration.md#supported-keys).

### Answering access prompts in a terminal

On a computer with no Splinterm Window to show the access prompt, such as a
headless host you reach over SSH, run `splinterm approvals` in a terminal on
that computer:

```bash
ssh my-server
splinterm approvals
```

It prints `Waiting for agent access requests on my-server… (Ctrl+C to quit)`
and then shows each request as text, with the same lines as the dialog: the
agent's reported name and the note that it names itself, the `host` row
(`this computer (my-server)`), the program, the process, the workspace, the
access, and the scopes, escaped in the same way. Below them are your choices:
**Deny**, then only the durations the request offers, such as
**Allow Manage once**, **Allow Manage while open**, and **Always allow Manage**.
It counts down the time left, such as `denies in 1:42`. It can run on a desktop
too; a Window's dialog and the terminal then show the same request, and the
first answer wins. The other closes and says the request was answered
elsewhere.

It is protected the same way as the dialog:

- Each request starts on **Deny**. Up/Down or `j`/`k` choose, and Enter
  answers. Escape, `q`, or Ctrl+C deny.
- Keys typed before a request appears are thrown away, pasted text is ignored,
  and keys count only about a second after it appears. After you move to an
  Allow choice, Enter counts about 600 ms later.
- When the time runs out it prints `Expired: no answer sent` and sends nothing.
  A request that is cancelled, answered elsewhere, or ended by a daemon upgrade
  closes and says why.
- While the terminal is too small to show the whole request it can only deny.
- It answers only on a key read from its terminal: it refuses to start unless
  standard input and output are its own controlling terminal, and it has no
  option that answers for you.

Run it directly in your own terminal, such as an SSH login or a plain terminal
window, not inside a Splinterm pane or a tmux or screen session started from
one. splinterd checks where it runs. It refuses the approver inside a Splint,
inside the requesting agent's process tree or session, and, when workloads run
in systemd units, inside a Splint scope or Dojo slice, which is where a tmux or
screen session started from a pane stays. It also checks each request on its
own: it never offers a request to an approver inside that agent's process tree
or session, and says nothing about it, so that approver simply never sees that
agent's requests. Only the installed `splinterm` beside the running `splinterd`
may answer, as for `splinterm upgrade --now` ([CLI](cli.md#daemon-upgrade)); a
development build is refused. One process may hold at most 2 of the daemon's 16
approver places; when they are full it says too many approvers and remote
Windows are connected. After a lost connection or a daemon upgrade it connects
again by itself; Ctrl+C with no request waiting quits. Restart it after a
package upgrade: the running approver keeps answering until it has to connect
again, for example after the daemon's live upgrade, and then stops with a note
to run `splinterm approvals` again, because its replaced file no longer matches
the installed `splinterm`.

Avoid running it inside tmux or screen, or in a terminal that other programs
can drive by remote control (such as kitty's or WezTerm's): a program you run
there can type into the approver (`tmux send-keys`, `kitty @ send-text`,
`wezterm cli send-text`), and the delays above do not stop that. It prints a
warning when it starts inside tmux or screen. These checks do not stop a
same-user program that deliberately leaves every process tree and cgroup, for
example with `systemd-run --user --pty`, and runs an approver there, nor one
that writes to your terminal. That is the same kind of risk as an agent editing
your policy file, which it can already do as your account; see the residual
risks in [ADR 0015](adr/0015-remote-consent.md#consequences).

## Exit, restore, and relaunch

A Splint has a stable ID across process exits. Each new process under that leaf
has a new positive **incarnation**. This prevents stale clients from silently
retargeting a replacement process.

- `kill SPLINT_ID` ends a live process but retains its Splint leaf.
- `restore SPLINT_ID` starts an exited Splint from saved launch metadata.
- `restore-dojo DOJO_ID` and `restore-lair LAIR_ID` restore all exited leaves in
  the selected saved scope.
- `relaunch SPLINT_ID [-- ARGV...]` starts a new incarnation, optionally with new
  direct argv.
- `close SPLINT_ID` removes an exited leaf and collapses its layout branch.
- `close-dojo DOJO_ID` removes a Dojo only after every Splint has exited.

Restore is never automatic. Saved commands are inert metadata until a person or
properly authorized tool requests restoration. Human `kill` prompts unless
`--yes` is supplied; `close` and `close-dojo` remove only already-exited topology
without another interactive prompt. Machine forms require `--yes` for all three.
Use `--yes` only for an already-approved unattended operation, not as a substitute
for intent.

## Saved Lair layouts and retention

The command palette exposes exact-target actions to **Save current Lair layout**,
**Pin/Unpin current Lair**, **Preview saved Lair layout**, and explicitly restore
a saved Lair or one selected Dojo. The same current-Lair paths are closed keymap
actions: `lair.save`, `lair.pin-toggle`, `lair.preview`, and `lair.restore`.
The `omarchy-tmux` defaults are `Prefix Shift+S/F/V/O`, respectively; the
`splinterm` profile leaves them available to strict overlays without adding
more direct chords. Saving preserves the durable Lair/Dojo/Splint
tree, split axes and proportional ratios, default focus, names, validated launch
working directories, known structured argv, shell policy, scrollback policy, and
bounded last-known rows and columns. Ratios—not prior pixel dimensions—are the
saved size authority; a restored Window projects them into its current cell grid,
and shows only the focused Splint while they cannot all keep their minimum size
([Panes and layouts](#panes-and-layouts)).

Retention state has a narrow meaning:

- **Disposable** is the default. A fully exited low-value Lair may be selected by
  the daemon's bounded retirement policy when capacity is required.
- **Saved** retains an explicit durable workspace recipe and is never selected
  for automatic retirement.
- **Pinned** is also protected from automatic retirement. Pinning does not start,
  stop, resize, or detach processes.
- **Live** and **Detached** describe running daemon-owned work with or without a
  graphical attachment; neither is eligible for automatic retirement.
- **Restorable** means validated launch/layout metadata remains after every
  Splint has exited. It does not mean a process is running or will start
  automatically.

Preview is body-free. It lists each pane as a row: its Dojo, its launch recipe
(the application's file name and arguments, `shell` for a shell, or
`not restorable` when it has none) and its working directory, with `~` for your
home directory, and names the Lair and its Dojos as the Explorer does. Restore
lists only the exited panes it would start, so its count matches its rows; when
every pane is still running, it says there is nothing to restore and offers
only Close. The recipe comes from the saved layout, never from the title a
program set.
Splinterm does not infer foreground applications from `/proc`, titles, prompts,
or terminal output and does not save process memory, PTYs, editor state, shell
state, environment values, secrets, terminal bodies, scrollback, clipboard, or
image bodies.

Restoring more than one process requires explicit confirmation and revalidates
the captured identity, revision, argv, working directory, and topology bounds.
Missing directories and invalid recipes fail instead of silently substituting
another command or cwd. A per-leaf launch failure retains the saved recipe and
does not discard successfully restored leaves. Saving, pinning, previewing,
daemon startup, login, and package upgrades never execute saved commands.

Removing saved or pinned metadata is destructive and uses the existing exact
captured-Lair confirmation path; it must not silently retarget another Lair.
For recovery, inspect complete exited topology with `splinterm list --all`, use
preview before restoring, and back up the owner-only state directory while the
daemon is stopped as described in [Headless operation](headless.md). Splinterm
does not promise live-process checkpointing, reboot-transparent process
continuity, reusable templates, or cross-machine saved-layout synchronization.

## Reset and daemon lifetime

The daemon owns all running shells. Stopping it, installing a private-protocol-
incompatible version, or resetting it ends those processes. A package upgrade
also replaces the trusted client inode. A Window opened before the upgrade
keeps working, including new tabs and panes, while it runs, until it is
live-upgraded or reopened; new launches use the new install. A client that
executes another program loses that trust, and a fresh launch must match the
installed `splinterm` next to the running daemon (or, after a verified live
upgrade, the exact client snapshot that daemon pinned). To back up and clear every
session in one guarded workflow:

```bash
splinterm reset
```

The command confirms, stops the daemon, moves persistent session state to a
reported backup, and restarts cleanly. It does not remove policy or user
configuration. Read [Headless operation](headless.md) before service, backup,
policy, or recovery work.

`splinterm upgrade` shows whether the daemon could hand its running terminals to
a newly installed build without stopping them, and `splinterm upgrade --now`
starts that live upgrade. It needs an installed package build; Nix installations
are refused for all of Splinterm 0.2.0. Whenever it reports live upgrade as
unavailable, restarting the daemon remains the upgrade path. See
[Daemon upgrade](cli.md#daemon-upgrade).

During a live upgrade every Window shows "Preparing upgrade…" and then
"Upgrading — input paused". Typed keys, pastes, and session-changing actions
are dropped, not queued; scrolling, search, help, and tab switching still work.

A local Window that takes part relaunches as the new client when the installed
build takes over. It restores its tabs, the active tab, and the focused pane,
and resumes input. If it had typing control before, it usually gets it back,
and you can keep typing without a click. If not, the pane is view-only until
you choose Request Control. If the new build fails and the daemon rolls back,
the Window shows "Rolled back"; if the attempt is cancelled, it shows "Upgrade
cancelled" and resumes. A local Window that cannot take part is listed before
the upgrade starts, and proceeding closes it; its terminals keep running and
reopen from the Recent Dojos picker.

A remote Window connected to the upgraded daemon shows "Reconnecting…" and
reattaches every pane within about 30 seconds (see [Remote](remote.md)). If it
cannot, it keeps its last screen, stays paused, and says "Lost connection to
splinterd — reopen from the launcher"; its terminals may still be running. A
local Window that has to reconnect this way, but whose client the package
upgrade replaced, gives up at once: a fresh client must match the installed
`splinterm` (or the client snapshot a live-upgraded daemon pinned), which a
Window reopened from the launcher runs.

## Remote human use

Remote profiles are strict local TOML records. Inspect and probe one without
mapping a Window:

```bash
splinterm remote list
splinterm remote inspect PROFILE
splinterm remote check PROFILE
```

Open the profile's native picker or an explicit workflow with:

```bash
splinterm --remote PROFILE
splinterm --remote PROFILE dojos
splinterm --remote PROFILE reopen
```

Without a terminal, use the **Splinterm Remotes** app-menu entry, the **Remote
Dojos** desktop action, `splinterm remotes`, or the **Remote Dojos…** row in
Recent Dojos. One Window lists your profiles, shows the connection and any SSH
prompt, lists that host's Dojos, and then becomes the remote terminal; Escape
steps back one stage. **Open Remote…** in the command palette shows the profiles
as a pop-up and opens the chosen one in exactly one new Window.

OpenSSH authenticates the human account. The graphical relay does not create a
public daemon listener, and machine automation remains separately policy-scoped.
Remote images and trusted-only forced takeover are intentionally unavailable.
See [Remote access](remote.md).

## Configuration and troubleshooting

The normal configuration path is
`${XDG_CONFIG_HOME:-~/.config}/splinterm/config.ini`. Validate it and the selected
keymap without contacting the daemon:

```bash
splinterm config check
splinterm keymap conflicts
```

Use [Configuration](configuration.md) for supported keys and Omarchy theme
integration, [Headless operation](headless.md) for daemon/service failures, and
[Current status](status.md) for validated environment and compatibility limits.
