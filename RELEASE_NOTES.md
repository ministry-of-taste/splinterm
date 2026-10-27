# Splinterm 0.2.0

Splinterm 0.2.0 is the first feature release after 0.1.1 for the documented
x86_64 Omarchy/Arch Linux target. Its main new feature is live upgrade: from
0.2.0 on, a newly installed build can take over your running terminals without
stopping them. The upgrade from 0.1.x itself still restarts the daemon once,
which ends running shells.

It also adds easy agent access (give an agent one workspace in a single answer,
then see and revoke it), explicitly reported agent status, a startup picker,
remote launch from inside the app, and Omarchy 4 theme integration with support
for its panel transparency, text size, and tab-strip height.

A quick reminder of the names: a **Lair** is a project or persistent session, a
**Dojo** is one terminal layout inside a Lair (shown as a tab), and a **Splint**
is one terminal pane and its process. `splinterd` is the daemon that keeps them
running; a Window is only a view.

0.2.0 changes the private daemon protocol. A 0.2.0 client cannot talk to a 0.1.x
daemon, and the reverse. Read "Upgrading from 0.1.x" below before installing.

## Upgrading from 0.1.x

Moving from 0.1.x to 0.2.0 cannot be live. The running 0.1.x daemon has to
restart once, which ends every running shell and the programs in them. Saved
Lairs and Dojos are kept: their saved format has not changed since 0.1.1.

1. Save your work.
2. Install 0.2.0 from a terminal Splinterm does not own, such as Foot or a text
   console. `./install.sh` and `tools/package/upgrade-local-package.sh` stop the
   daemon before installing, which would also end a terminal inside Splinterm,
   so they refuse to run there. They ask before they stop it and start it again
   afterwards.
3. If you installed with `pacman -U` or an AUR helper, the 0.1.x daemon is still
   running. Restart it from Foot or a text console:

   ```bash
   systemctl --user stop splinterd.service
   systemctl --user daemon-reload
   systemctl --user start splinterd.service
   ```

   Or open Splinterm from the desktop launcher. It cannot read the 0.1.x daemon,
   so it asks first and shows a best-effort count of its shells; choose
   **Restart now**.
4. Open Splinterm. Restore saved Lairs and Dojos from the Explorer or the
   command palette; restoring starts new shells.
5. If you use the Omarchy integration, run `splinterm integration omarchy
   enable` again. A module written by 0.1.x has no rule for trusted dialogs.
6. Upgrade `splinterm-mcp` and every remote host to 0.2.0. Different versions
   do not work together.

If Splinterm then fails at startup with a keymap error, your keymap overlay
binds a chord that is now a default; see "Behaviour changes".

To go back to 0.1.x, stop `splinterd.service` yourself first, or use one of the
installers above. The 0.1 launcher restarts an incompatible daemon without
asking, which ends every running shell.

From 0.2.0 on, installing with `pacman -U` or an AUR helper leaves the running
daemon alone. Run `splinterm upgrade` to see whether it can move to the new
build, then `splinterm upgrade --now` to move your terminals without stopping
them. `./install.sh` and `tools/package/upgrade-local-package.sh` still stop the
daemon, which ends running shells. Package scripts never restart or upgrade a
running daemon by themselves.

## Behaviour changes

- Private protocol 44 (0.1.1 used 36). Old and new clients, daemons, and relays
  do not interoperate. Upgrade both ends of a remote connection, and keep
  `splinterm-mcp` at the same version.
- The agent access prompt no longer grants on Enter: Enter denies, and only `G`
  or the green Allow button grants. Its keys do nothing until you click inside
  it or press the dialog shortcut (see "Trusted dialogs").
- **Revoke all access** (`Ctrl+Shift+R`) asks with exact counts before it acts,
  except in a Window opened on a single Splint (`splinterm launch --splint-id
  ID`), which has no command palette.
- The desktop launcher no longer restarts a daemon that may still run shells
  without asking.
- New Dojos and Lairs start in your home directory, not the focused Splint's
  directory.
- On Omarchy 4 with no font size in `config.ini`, the terminal follows
  Omarchy's text size (9 pt on a stock desktop) instead of Splinterm's 14 px.
  Set a size in `config.ini` to keep your own.
- Window chrome text no longer follows the terminal font size or zoom
  (`Ctrl++`/`Ctrl+-`): it uses Omarchy's text size, or 14 px without Omarchy.
  Set `main.ui-font-pixelsize` to choose it.
- `Ctrl+Shift+1`–`9` are now tab shortcuts: on US layouts with tabs,
  `Ctrl+Shift+2` and `Ctrl+Shift+6` no longer send `Ctrl+@` and `Ctrl+^`
  (`Ctrl+2` and `Ctrl+6` still do).
- A keymap overlay that binds a chord that is now a default fails at startup
  until it adds the `[[unbind]]` the error names. The new defaults are
  `Ctrl+Shift+1`–`9` and, in the `omarchy-tmux` profile, `Prefix Shift+S`,
  `Prefix Shift+F`, `Prefix Shift+V`, and `Prefix Shift+O`.
- The tab strip is shorter on a stock Omarchy desktop.
- The repository moved to `ministry-of-taste/splinterm`; old URLs redirect.

## Live daemon upgrade

- `splinterm upgrade` reports whether the running daemon could hand its
  terminals to the installed build, and changes nothing. It shows the daemon's
  generation, build, session counts, whether it kept a rollback copy, and every
  reason a live upgrade cannot run now.
- `splinterm upgrade --now` starts the live upgrade. Shells, scrollback, and
  running programs keep going; the daemon keeps its process ID.
- Open Windows come with it. Each Window shows "Preparing upgrade…", then
  "Upgrading — input paused". Local Windows that take part relaunch as the new
  client and restore their tabs, active tab, and focused pane. If the Window
  had typing control before, it usually gets it back, and you can keep typing
  without a click. If not, the pane is view-only until you choose Request
  Control. Keys typed while paused are dropped, not queued.
- Remote Windows connected to the upgraded daemon show "Reconnecting…" and
  reconnect over the same SSH session.
- If some Windows cannot take part, the command lists them and asks whether to
  cancel or proceed without them. In a terminal it asks `[c]ancel / [p]roceed`.
  Without one, or from inside a Splinterm terminal, a small trusted "Splinterm —
  Upgrade" dialog asks instead (see "Trusted dialogs"); only `G` or its green
  **Proceed** button proceeds. Closing it or not answering cancels; with no
  answer in time the command prints "No answer about the Windows within N
  seconds; cancelling." Proceeding closes those Windows; their terminals keep
  running and reopen from Recent Dojos.
- If the new build fails before it takes over, the daemon rolls back to the
  previous build in the same process and keeps every terminal. Windows show
  "Rolled back".
- After a live upgrade or a rollback, the next upgrade needs a daemon restart.
  A daemon started by a live upgrade or a rollback keeps no rollback copy.
- Only the installed `splinterm` next to the running `splinterd` may run
  `splinterm upgrade`. The command has no machine output.
- Exit statuses: 0 report shown or upgrade completed; 1 error; 2 usage error;
  3 cancelled, nothing changed; 4 live upgrade unavailable now, nothing changed;
  5 another upgrade in progress; 6 did not complete, previous build still
  serving with terminals kept; 7 result unconfirmed (for example because the
  installed `splinterm` was replaced while the upgrade ran), run
  `splinterm upgrade` to check.
- During an upgrade, commands that change something are not retried. A command
  the daemon refused says it did not run. A command cut off by the upgrade says
  it may have run; check before running it again. Read-only commands (`ping`,
  `list`, `topology`, `inspect`, `focus`) are retried once.
- Nix installations refuse live upgrade in 0.2.0; `splinterm upgrade` says so,
  and restarting the daemon remains the Nix upgrade path.

## Installing a new build

- The desktop launcher now checks the running daemon's build before it opens a
  Window. The installed build opens the Window, and with no daemon it starts
  the service. Another build that reports no live terminal is restarted without
  asking.
- Another build with live terminals, or a daemon the launcher cannot read (such
  as 0.1.x), gets a trusted "Splinterm — Upgrade" dialog naming the running and
  installed versions. **Not now** is the default and opens nothing: `Enter`,
  `Escape`, `D`, the red left **Not now** button, closing the dialog, or no
  answer within 120 seconds. Only `R` or the green right button chooses
  **Restart now, end N shells**, with the daemon's exact count. For a daemon
  it cannot read, the count is a best-effort "end about N shells", or "end all
  running shells" when none can be counted. When the daemon reports that
  nothing blocks a live upgrade, the dialog says to choose **Not now** and run
  `splinterm upgrade`.
- With no answer, the launcher prints "splinterm: no answer within 120 seconds,
  so nothing was restarted." instead of a failure, and exits as for **Not now**
  (status 3).
- Before restarting, the launcher reads the daemon again and asks again if what
  the restart would end has changed. It never starts a live upgrade itself.
- Unattended launches, with an explicit app ID (such as the Omarchy
  screensaver's) or a command, never show the dialog: the question is answered
  **Not now** and nothing opens. An idle daemon is still restarted for them.
- A daemon that cannot be read but runs the installed `splinterd` is treated as
  busy: the launch fails and says to try again shortly. A development build or
  a copy of `splinterm` earlier on `PATH` fails with an explanation and never
  asks.
- `./install.sh` and `tools/package/upgrade-local-package.sh` ask before they
  stop a running daemon and say what installing ends: nothing, the exact number
  of live terminals, or, for a daemon that cannot report it such as 0.1.x,
  every running shell with a best-effort count. Cancelling changes nothing.
  Package scriptlets still never restart user services.
- A Window opened before a package upgrade keeps working, including new tabs and
  panes, until it is live-upgraded or reopened. 0.1.x required reopening every
  Window after an upgrade.

## Easy agent access

Easy agent access gives an agent one workspace (a Lair) in a single deliberate
answer, for agents helping you in your own graphical session. Headless, remote,
and administrative access still uses the policy file, which is unchanged. It
keeps cooperating agents inside what you agreed to; it is not a sandbox against
a hostile program running as your account.

- Three levels, each covering the whole workspace, including Dojos and Splints
  created later, and nothing outside it:
  - **Look**: read the layout, visible terminals, and scrollback, including
    search, and report agent status.
  - **Look and type**: Look, plus type and resize, including taking typing
    control of a pane your Window controls. Your Window shows the change, and
    you can take control back.
  - **Manage**: Look and type, plus start processes, change layout and names,
    close and terminate, and restore.
- No level includes clipboard access, authorization or audit inspection,
  revocation, or creating a new Lair. A request for anything other than one
  level is shown as **Custom** with every scope listed, and is granted exactly
  as asked, never rounded up.
- Three durations:
  - **Just this once**: five minutes, for that one agent process.
  - **While this workspace is open**: until the workspace is closed or the
    daemon stops or upgrades; it survives the agent restarting.
  - **Always for this workspace**: until you revoke it or the workspace is
    removed; it survives Splinterm and the agent restarting and upgrading.
- **Always** is offered only when your account cannot modify the agent's
  executable: a protected install such as a package under `/usr/bin`, or a Nix
  profile entry that resolves into a root-owned multi-user `/nix/store`.
  Scripts run by an interpreter (`python`, `node`, `bash`, and similar) and
  development builds or other programs in places you can write get the two
  shorter durations. The SSH relay, and a Custom request that includes control
  takeover, get just this once. The prompt shows a missing choice disabled with
  the reason.
- A longer grant belongs to the program, not to one process, and the prompt says
  so, for example "Any program that runs `/usr/bin/splinterm-mcp` shares this
  access." Splinterm checks the identity again on every connection, so a
  package upgrade keeps an **Always** grant; restart an agent that was running
  before the upgrade.
- The prompt is a trusted dialog (see "Trusted dialogs"). Enter, Escape, and
  `D` deny; only `G` or a click on the green **Allow** button grants. Earlier
  releases granted on Enter. You choose the duration with the mouse or the
  arrow keys; **Just this once** is preselected, and the Allow button names
  your exact choice, for example **Allow Manage while open**. Closing the
  prompt denies.
- The prompt shows the agent's reported name (as a label only), its exact
  executable path, the workspace name with its short ID, the level, and the
  exact scopes. Names and paths appear as data: a control or bidirectional
  character shows as an escape such as `\u{202e}`, the same in the prompt, the
  **Agent access** list, and `splinterm access list`.
- A request a grant already covers is answered at once, without a prompt.
  Prompts appear one at a time. An unanswered request stays pending for up to
  two minutes, and the agent collects your answer by asking again; the MCP tool
  `splinterm.request_lair_access` returns `status: "pending"` meanwhile instead
  of failing.
- While a request waits, the Explorer marks its workspace **Agent asking for
  access**, the Window title ends with **AGENT ACCESS REQUEST WAITING**, and
  **Agent access** in the palette shows how many wait.
- The Explorer marks, with an icon and in words, which workspaces, Dojos, and
  Splints an agent can reach: **Agent can reach** (no covered agent
  connected) or **Agent connected**. When only some of a workspace is covered,
  its row says **Agent can reach part** or **Agent connected to part**. The
  words appear in the footer for the selected row and in every row's
  accessible text; a waiting request has its own lock icon with a question
  mark.
- `splinterm access list` shows every grant of every duration, pending
  requests, and policy rules that match a connected agent (read-only there),
  with the agent's identity, the access, when it was created, last used or
  expires, and what it is doing now. `splinterm access revoke ID` ends one
  entry; `splinterm access revoke --all` first shows exact counts and asks
  (`--dry-run` only counts, `--yes` skips the question).
- **Agent access** in the command palette shows the same list in a Window. A
  click only selects a row. Enter on a just-this-once or while-open grant
  revokes it at once; on an **Always** grant it asks first, with Cancel
  selected.
- In a Window with a command palette, **Revoke all access** and its shortcut,
  `Ctrl+Shift+R`, now show the exact counts from a dry run first, with Cancel
  selected, and count again before acting.
- Revoking takes effect immediately: requests stop, subscriptions close, and
  control is released. It never removes a matching policy rule; the list and
  the result name the rule to edit in `policy.json`.
- Remembered grants live in `~/.local/state/splinterm/agent-access.json`,
  separate from `policy.json`, written owner-only. A damaged or unsafe file
  disables remembered access until it is repaired; policy and shorter grants
  keep working. `splinterm reset` backs it up with your sessions.
- `splinterm mcp setup claude-code` (or `codex`, or `vscode`) adds the installed
  `splinterm-mcp` to that agent host's configuration in one step, and
  `splinterm mcp remove HOST` takes it out again. `--dry-run` shows the exact
  change, a second run changes nothing, and other entries, settings, and
  comments are kept. Setup grants no access and writes no policy, and it says
  when the adapter it names can never be offered **Always**. The Claude Code
  and Codex entries match what those hosts' own commands write; the VS Code
  entry follows VS Code's documented form but is not yet verified against a
  real install.

## Trusted dialogs

- The agent access prompt, the `splinterm upgrade --now` question, and the
  launcher's restart question ignore every key until you click inside one or
  press the dialog shortcut, even if your desktop gives them focus. Typing meant
  for another Window never answers them. With the Omarchy integration enabled,
  they also appear without taking keyboard focus (see below).
- The dialog shortcut, `Ctrl+Alt+G` by default, engages a dialog as a click
  would and never answers it. Keys and the answer click then count only about
  600 ms later. A key held since before that never counts, and key repeats
  never do. Change or turn off the shortcut with `integration.dialog-shortcut`.
- Under Omarchy, `splinterm integration omarchy enable` now also maps trusted
  dialogs without focus, and `Ctrl+Alt+G` then reaches a waiting dialog from any
  Window. Splinterm enables that Hyprland binding only while a dialog waits,
  writes nothing to your Hyprland files, and binds nothing when your own
  configuration already uses the chord. For a module written by an earlier
  release, the **Dialog focus** row of `splinterm integration omarchy status`
  says `missing`; run `splinterm integration omarchy enable` again to upgrade
  it.
- Trusted dialogs use your configured font and ask the desktop for one fixed
  size, so tiling desktops such as Hyprland float them instead of tiling them.
  Given a smaller size anyway, a dialog rewraps its text and then shrinks it.
  While any text is still hidden, it can only be declined: it never grants,
  proceeds, or restarts, and says so in place of its key hints.
- Trusted dialogs are redrawn as one box: a thin `warning` frame with a
  lowercase title such as "agent access request" set into it, the request as
  one sentence, the details as an aligned table, the durations as list rows
  with the chosen one highlighted, and two labelled buttons below, a red
  **Deny** (left) and a green **Allow** (right) that names exactly what it
  grants, each with its keys under its label. The frame's bottom edge says
  how to engage the keyboard until you click or press the dialog shortcut,
  then which keys answer. The access prompt now says that the agent names
  itself and Splinterm cannot verify that name. A button label too long for
  its half counts as hidden text, so the dialog can then only be declined.
  The box casts the same hard shadow as the in-Window popups. The upgrade and
  restart questions count their time to answer down at the right of the top
  edge, such as `0:42`; the access prompt shows no countdown.

## Agent status

- An agent can report `working`, `needs_input`, `done`, `error`, or `idle` for
  its Splint with `splinterm agent-status set|clear|get`, or through the new MCP
  tool `splinterm.report_agent_status`. An optional short detail and TTL are
  supported; `working` and `needs_input` turn stale after two minutes without a
  fresh report by default.
- Splinterm shows only what was reported. It never guesses from terminal text,
  titles, prompts, process names, or output, and a status cannot grant access,
  approve, type, or take control.
- A process inside the Splint reports without a grant. Anything else, including
  an agent that fully detaches from the Splint, needs an `agent_status_report`
  grant from the policy file or from consent; every easy agent access level
  includes it.
- The Explorer shows an icon for each Splint and the most urgent state for each
  Dojo and Lair, and spells the selected row's state out in its footer. Dojo
  tabs show one badge with a count. A fresh `working` status is a Braille
  spinner that steps at most four times a second, only in the focused Window,
  and holds still with `SPLINTERM_REDUCED_MOTION=1`; every other state is a
  static icon. States differ by shape and by text, never by colour or motion
  alone.
- Status glyphs take herdr's colours from the theme: `working` in warning,
  `needs_input` and `error` in danger (each with its own glyph), `done` in
  info, `idle` in success, and a stale status faint. The access "Asking"
  marker is warning and the remote host tag takes the theme's remote-marker
  colour. Each colour is kept readable against the opaque colour it is
  drawn on: the Explorer, its selected row, the tab strip, or the selected
  tab; translucent glass can lower it, as it does for ordinary text.
  `[explorer] status-colors=no` turns the status colours off, and
  `[explorer] status-glyphs=dots` draws one-cell shapes (◆ ▲ ■ □) instead
  of icons, or the ASCII glyphs when the interface font cannot draw them.
  **Reload configuration** applies both.
- Explorer search matches state words such as `needs input` or `stale`. The
  palette command **Clear agent status** clears the focused Splint's status.
- `inspect` and `topology` machine output carry an optional `agent_status`.
- `[explorer] agent-status=no` hides all of it. `docs/automation.md` includes
  ready-made hooks for Claude Code and Codex CLI.

## Launching and remote Windows

- A startup setting makes the commandless desktop launch open the Recent Dojos
  picker instead of starting a shell. In `~/.config/splinterm/config.ini`:

  ```ini
  [main]
  startup=dojo-picker
  ```

  For one launch, `splinterm-xdg-terminal-exec --picker` opens the picker and
  `splinterm-xdg-terminal-exec --new` creates a terminal, whatever the setting
  says; `splinterm launch --picker` is the direct CLI form. The **New
  Terminal** desktop action always creates a terminal.
- Open a remote from the **Splinterm Remotes** app-menu entry (or the **Remote
  Dojos** action, or `splinterm remotes`). One Window lists your profiles,
  shows the connection and any SSH prompt, lists that host's Dojos, and then
  becomes the remote terminal. **Open Remote…** in the command palette shows
  the profiles as a pop-up and opens exactly one new Window. `splinterm
  remotes` typed in a terminal returns at once and prints nothing.
- Every graphical remote launch first shows the connection status. On failure
  it names the cause, suggests a fix, and shows recent OpenSSH output; press R
  or Enter to retry, or Escape to go back (a direct `splinterm --remote` closes
  and exits 1).
- For graphical remote launches with no controlling terminal and no configured
  external `SSH_ASKPASS`, the status Window asks in a masked field when OpenSSH
  needs a password, passphrase, or PIN that no ssh-agent key can answer. The answer
  is never echoed, logged, or put on the clipboard, and an unanswered prompt
  ends the attempt after 120 seconds. Host keys are still never accepted
  through it.
- A launch whose command ends before its Window opens, such as
  `splinterm-xdg-terminal-exec -- true`, now prints "splinterm: the command
  ended before its Window opened." and succeeds instead of failing.
- A graphical command that fails, such as a desktop launch that fails before
  its Window maps, now prints its full reason on one line after "splinterm
  graphical client failed:" instead of that phrase alone. A configuration that
  cannot be loaded is still reported only as "configuration could not be
  loaded", so no configuration value reaches the journal.

## Dojos, tabs, and the Explorer

- The Lair Explorer shows one line per row: the disclosure chevron and the
  name, then fixed icon columns on the right for agent access, agent status,
  whether the row is live (a filled or hollow dot), and whether a Lair is saved
  or pinned. The footer's first line is an action line, `new · menu · «`:
  **new** makes a new Lair, **menu** opens the selected row's menu (the
  command palette when no row is selected or the row has nothing on its
  menu, and nothing while the menu is unavailable during a refresh), and
  **«** collapses the Explorer to its rail. Clicking one, or anywhere else
  on the footer, leaves the keyboard where it was (a menu that `menu` opens
  has the keys until it closes), and none has a default shortcut; the words
  are sized from the UI font, and a narrow panel drops `menu`, then `new`.
  The second line gives the selected row one short status that says only
  what the row does not already show, a waiting access request and agents
  needing input or in error first, for example *1 needs input · 2 working ·
  saved*. It drops the name, a running row's "Running", a disposable Lair's
  "Disposable", and the current Dojo's "Current". A status too long for the
  panel ends in an ellipsis after its last whole word instead of wrapping.
  Assistive technology still reads the full words for every row. The search
  field, a navigation notice, or a search left behind takes the line first,
  and a row with no status shows who has the keyboard; while the terminal
  does, *Terminal has keys · click to move*. It used to read as if a click
  left typing in the terminal, but a click on a row gives the Explorer the
  keys. The breadcrumb line is gone; the title and the filters share one
  header line.
- The Lair Explorer opens on **Live**, and its filters read Live, Saved, All
  (`f` cycles them in that order). Search looks through every Lair whatever the
  filter, so a saved or stopped Lair is found from Live too.
- The expanded Lair Explorer lists every Splint that reports an agent status
  in an *agents* section under the tree, most urgent first, whatever the
  filter or the search: each row shows its status icon, its name, and its Lair
  and Dojo when they fit. The section takes up to five rows and a third of
  the list, and says *+k more* when more report than it shows. A click
  focuses the Splint and right-click opens its menu, as in the tree. Down
  from the tree's last row moves into the section. It is hidden on the rail,
  with `agent-status=no`, and when nothing reports.
- `[explorer] row-lines=2` makes the expanded Lair Explorer's rows two lines
  tall. A Splint row, in the tree and in the agents section, shows the agent's
  detail muted under its name, or its working directory (`~` for home, the
  middle elided to keep the last components) when no detail is reported. Lair
  and Dojo rows centre their one line and the rail is unchanged. The default
  stays `1`, and a reload applies a change at once.
- The Lair Explorer can collapse to a narrow rail with one row per Lair: its
  initial and its most urgent agent glyph, with the current Lair marked by
  the accent bar and a muted `s` or `a` in its header under the Saved or All
  filter. Click `«` on the footer's action line to collapse it and `»` at
  the top of the rail to expand it, or choose
  **Toggle Lair explorer rail** in the command palette; there is no default
  shortcut. Clicking a rail row switches to that Lair, never restoring one: a
  Lair whose Dojos only restore expands the Explorer to it instead. The
  terminal and the tab strip beside the Explorer widen and narrow with it. A
  Window too narrow to dock the full Explorer still docks the rail, and a
  narrower one shows the rail over the terminal. `explorer.collapsed=yes`
  shows the rail first in new Windows.
- A thin line now runs the full width of the Window along the bottom of the tab
  strip, in the colour and thickness of a `line` Splint divider; the active
  tab's accent underline stays thicker and sits on top. Turn it off with
  `multiplexer.tab-strip-border=no`; Reload configuration applies it.
- `multiplexer.close-unused=yes` makes Close Tab and closing a Window end a
  Dojo that was never used, instead of leaving it running detached. Typing, a
  split, a rename, or a program in the foreground all count as use. Default
  `no`; remote Windows get the same decision from the remote daemon.
- New Dojos and Lairs created inside a Window now start in your home directory;
  splits still start in the focused Splint's directory. Change either with
  `multiplexer.new-dojo-directory` and `multiplexer.new-splint-directory`
  (`inherit`, `home`, or a path).
- **Reload configuration** now also applies these settings, plus
  `multiplexer.persist-on-tab-organization`, `explorer.agent-status`,
  `multiplexer.divider-style` and `multiplexer.frame-title`; a changed divider
  style re-lays the Splints out at once.
- `Ctrl+Shift+1` to `Ctrl+Shift+9` select the first nine tabs by default, also
  on layouts where Shift+digit produces a symbol.
- Inactive tab labels are muted, with automatic contrast adjustment for native
  themes.
- Duplicate tab names from launcher-created Lairs read "Dojo 1 (1)",
  "Dojo 1 (2)" instead of showing raw generated Lair names; pickers use the same
  labels.
- Keybinding help (`Prefix+?`) is searchable. Save, pin toggle, Preview, and
  Restore of the current Lair are bindable actions; the `omarchy-tmux` profile
  binds them to `Prefix Shift+S/F/V/O`.
- Copy mode gains `w`/`b`/`e` word motions and `0`/`$`.
- The Explorer docks beside a half-width terminal instead of covering it, and
  tabs, the Explorer, menus, and dialogs stay readable at maximum zoom.
- A docked Explorer runs the Window's full height, and the tab strip sits
  beside it over the terminal. `[explorer] full-height=no` restores the
  full-width tab strip above the Explorer.
- The Explorer's header is a plain line of text rather than a bar: a muted
  `lairs` title and the filters as `live · saved · all`, the chosen one in
  bold, with no rule or highlight. Beside the tab strip it is exactly the
  strip's height and its text sits on the tab labels' baseline.
- `multiplexer.pane-gap` (0–64 logical px, default 0) leaves empty space
  between neighbouring Splints and around them at the Window's edges: between
  and around whole frames with `divider-style=frame`, around the centred line
  with `line`. The Window background, glassy or not, shows through it. Where
  the Splints cannot fit the gap, it shrinks for that layout, down to none,
  instead of being refused. **Reload configuration** applies it.

## Omarchy 4

- Splinterm reads Omarchy 4's theme location and colour names, and follows
  theme switches without a restart. Non-Omarchy desktops use the built-in
  palette quietly.
- Window chrome takes a theme's semantic colour keys before its named hues
  and ANSI colours, so a theme that deliberately puts another colour in its
  ANSI yellow slot keeps its own `warning`. Pane borders follow `border` and
  the active pane border `active_border_color` when a theme names them, and
  a theme with no `muted` or `dark_foreground` mixes its inactive tab
  labels from its text. An invalid value in a key Splinterm newly reads is
  skipped with one diagnostic instead of rejecting the theme.
- Glassy Windows: the terminal background, padding, tab strip, and docked Lair
  Explorer turn translucent with blur when glass is on, while text and icons
  stay opaque and crisp. The selected Dojo tab stays opaque, so it stands
  clearly apart from the inactive tabs. Toggling applies live.
- Menus, pickers, the command palette, the tab menu, the Dojo prompt and
  other popups sit on the theme's raised surface (Omarchy
  `lighter_background`), with its text, muted text and selection colours, as
  Omarchy's own menus do, and a theme that names no raised surface gets a
  step lighter than its background. Terminate is drawn in the theme's red.
  The docked Explorer's selected row now takes the full selection colour and
  its secondary text the muted colour, a JSON theme without `pane_border`
  draws its pane borders in ANSI 8, and some inactive tab labels shift
  slightly, now mixed in linear light.
- Trusted dialogs (access consent, upgrade, restart) take your theme and its
  `[colors]` keys as they open, always opaque: the frame in `warning`, the
  left "no" button's edge and label in `danger` and the right "yes" button's
  in `success`, each over a light tint of its colour, kept readable. Since
  their text names the buttons red and green, a theme whose
  danger is not red or success not green gets Splinterm's own red and green
  there, and a dialog whose colours cannot be kept red, green and apart
  (on a mid-grey background, say) keeps Splinterm's own palette. Their text
  size stays the configured font's, and they still open without keyboard
  focus.
- New `[colors]` keys in `config.ini` let you choose these yourself:
  `active-tab-background` and `active-tab-alpha` for the selected tab,
  `tab-strip-alpha` for the strip, and `explorer-background` and
  `explorer-alpha` for the docked Explorer.
- `[colors]` can also set any of the chrome's semantic roles by name, such as
  `accent`, `focus-ring`, `text-muted`, `warning` or `status-working`, to
  `#RRGGBB`, a terminal palette colour `color0` to `color15`, or `auto`.
  `[colors-dark]` and `[colors-light]` set them for one appearance. A set
  role is used as given, with one warning if it is hard to read (menus,
  pickers and the Explorer still raise too-faint text on their panels), and
  the roles that follow it follow it. They recolour chrome only, never the
  terminal.
- **Reload configuration** now applies every `[colors]` key, including
  `alpha`, `blur` and the chrome keys above; a key you remove gives the
  theme's own colour back.
- Splinterm follows three settings from the Omarchy shell's `shell.toml`: panel
  alpha for menus and popups (raised as needed to keep text readable), the text
  size set by `omarchy-display-text-size`, and the top bar's height. Open
  terminal Windows apply changes live; standalone pickers and the remote-status
  Window read them when they open.
- The tab strip is now as tall as Omarchy's top bar (26 px by default) and
  scales with the text size.
- Window chrome (tabs, the Explorer, menus, the command palette, pickers, the
  Agent access list, and Dojo prompts) has its own text size, taken from
  Omarchy's text size (12 px on a stock desktop) even when `config.ini` sets
  the terminal's font size. `main.ui-font-pixelsize` overrides it; without
  either, chrome uses 14 px. Chrome text still stops growing where it would
  overflow its row.
- Without Omarchy's `terminal` tag, Omarchy's `Super+C` sends an interrupt. The
  install messages and a one-time desktop notification now recommend
  `splinterm integration omarchy enable`; set
  `main.omarchy-integration-hint=no` to silence the notification.
- The screensaver integration works with Omarchy 4's `PATH` and opens on
  Omarchy's special screensaver workspace.

## MCP

- `splinterm-mcp` also accepts MCP protocol `2025-06-18`, so Codex can connect.
- New tool `splinterm.report_agent_status`; the catalog now has 34 tools.
- `splinterm.request_lair_access` asks for one of the easy agent access levels
  and can return a pending result while you have not answered yet; see "Easy
  agent access".

## Security and robustness

- The agent access prompt could exit before it appeared, because its screen
  failed an internal consistency check; it and the other trusted dialogs now
  open. A dialog that still fails reports why to the command that asked.
- An error while accepting a new connection no longer stops the daemon and
  every terminal. The daemon waits briefly and keeps running.
- Text from an input method never reaches the terminal behind a trusted prompt
  or a picker covering the Window. When terminal search is open, the text goes
  there instead.
- A crashed policy reload installs deny-all and keeps the daemon running.
- Closing, killing, or terminating a stubborn process no longer blocks every
  other topology change for up to a minute.
- Daemon shutdown no longer waits a full minute on a terminal whose process
  group is already gone.
- Hostile terminal output is bounded: a multibyte character can no longer cut an
  OSC title short, huge tab-movement counts no longer stall the terminal, and
  synchronized-update damage is capped. A malformed colour in configuration is
  an error instead of a crash.
- Fonts without horizontal metrics are rejected before measuring.
- Two likely causes of intermittent typing stalls over SSH and in remote tmux
  are fixed: a throttled synchronized frame is now published without waiting
  for more output, and queued keystrokes go to the daemon in one batch.
- The documentation now states plainly what agent permissions do not protect:
  policy and consent bound a cooperating agent, but they are not a sandbox
  against a hostile program running as your account.
- A split that cannot fit is refused, with a short note in the Window title,
  instead of closing the Window. A Window too small for its Splints shows them
  with less gap or none, or shows only the focused one until there is room
  again; it no longer exits.
- A Splint's history no longer gets stuck after you leave it and come back:
  - Taking control back, after another Window or a resize elsewhere had it,
    restores the pane's own size. Before, the terminal kept the other size, so
    the bottom of the output sat below the visible rows.
  - Scrolled-up history keeps its place while a program prints quickly, and
    you can scroll up during a burst. Before, output faster than one update
    could hold pinned the view to the bottom.
  - Scrolling back to live over and over while a pane is slow to refresh no
    longer closes the Window.
- Search finds lines deep in long history. Each daemon request reads only
  what rows contain, not their full width, and the Window continues on its own
  until a match turns up. Before, a wide pane searched only about its last
  thousand lines and then showed no matches. Paging through matches in
  history also keeps working while the terminal prints.
- `Shift+End` at the bottom of a shell no longer types `F`.
- Resizing a pane whose text does not fill it, for example by splitting it,
  keeps the text at the top, as in Foot. Before, the text and the cursor
  moved to the bottom with blank rows above them, and growing the pane again
  left them there.

## Known limitations

- Live upgrade is unavailable:
  - while a transient Lair is open: a Lair that ends with its Window. Ordinary
    launches create one only with `multiplexer.persistent-by-default=no`; a
    command opened with `splinterm-xdg-terminal-exec -- COMMAND` always does;
  - for a daemon not started from an installed package build;
  - on Nix;
  - after a live upgrade or a rollback, until the daemon restarts (one live
    upgrade per daemon start);
  - on systems that forbid executable memory files (`vm.memfd_noexec=2`), where
    no rollback copy is kept.
- Agent status lives only in daemon memory. It is dropped when a Splint's
  process exits or relaunches, and after a daemon restart or live upgrade, until
  the agent reports again. A stale report is dropped 15 minutes after it turns
  stale. Topology subscriptions do not carry agent status.
- Easy agent access bounds cooperating agents only. A program running as your
  account can edit the remembered-grant store, and a Nix profile link is owned
  by you, so any program running as you can repoint it. **Always** has no
  inactivity expiry; the access list shows when each grant was last used.
- `splinterm mcp setup vscode` is not yet verified against a real VS Code
  install.
- In a Window opened on a single Splint (`splinterm launch --splint-id ID`),
  which has no command palette, the Revoke all shortcut ends only the grants
  that Window shows and every pending request; use `splinterm access revoke
  --all` there.
- Dojo tab badges have no accessible description yet; the Explorer carries the
  same status. Splinterm does not read the desktop's reduce-animation setting.
- Splinterm's SSH prompt needs Linux pidfds (kernel 5.3 or later). Remote
  graphical sessions still exclude terminal image transfer.
- Persistence still follows the daemon lifetime: a crash, logout without
  lingering, or reboot ends running processes, and saved layouts do not
  checkpoint applications.

See `docs/cli.md`, `docs/packaging.md`, `docs/configuration.md`,
`docs/remote.md`, `docs/automation.md`, `docs/mcp.md`, and `docs/usage.md` for
details.
