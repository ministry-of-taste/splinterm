# Command-line reference

This page is the repository authority for the human command inventory and common
CLI workflows. The executable's `--help` is authoritative for exact flags in the
current build. The stable JSON/NDJSON schema, policy, limits, cancellation, and
exit contracts are owned by [Automation](automation.md).

## Invocation and output modes

```text
splinterm [OPTIONS] [COMMAND]
```

Global options accepted by command parsing are:

```text
--output human|json|ndjson
--schema-major MAJOR
--timeout-ms MILLISECONDS
--remote PROFILE
```

Human output is the default. `--output json` emits one versioned machine
document for supported one-shot operations. `--output ndjson` is reserved for
subscriptions. Human-only graphical and local-administration commands reject
machine-contract combinations at execution rather than pretending their output
is stable automation.

Machine stdout is reserved for protocol records; diagnostics go to stderr. Use
schema major 2 unless a later checked-in contract is explicitly documented.

## First human workflows

```bash
splinterm dojos                    # trusted recent-Dojo picker
splinterm reopen                   # most recent still-running Dojo
splinterm list                     # active Lairs
splinterm list --all               # include exited-only history
splinterm launch                   # fresh graphical Lair
splinterm launch --splint-id ID    # attach an exact Splint
splinterm window --lair-id L --dojo-id D
```

`dojos`, `reopen`, `window`, and `launch` are graphical human workflows.
`sessions` remains a compatibility alias for `dojos`. A
Window is disposable and may hold multiple client-local Dojo tabs; these commands
do not make compositor operations part of the machine API.

## Topology inspection

| Command | Purpose |
| --- | --- |
| `ping` | Check daemon reachability. |
| `list [--all]` | Summarize active Lairs, optionally including complete exited history. |
| `topology` | Inspect reviewed metadata for every Lair, Dojo, and Splint. |
| `inspect SPLINT_ID` | Inspect one stable Splint and its lifecycle metadata. |
| `focus` | Machine-only narrow projection of the keyboard-focused graphical Splint ID and safe current working directory. |

Examples:

```bash
splinterm ping
splinterm topology
splinterm inspect SPLINT_ID
splinterm --output json topology
splinterm --output json focus
```

`focus` never exposes terminal content, commands, PIDs, titles, or private state
paths. With no focused Splinterm Window, its two projection fields are null.

## Creation and layout mutation

| Command | Purpose |
| --- | --- |
| `launch` | Open an ordinary unnamed graphical Lair using `multiplexer.persistent-by-default`. |
| `launch --name NAME` or `launch -- ARGV...` | Open an explicitly persistent graphical Lair. |
| `launch --picker` | Open the Recent Dojos picker without creating a terminal; **New** uses the launch working directory. |
| `new NAME [--cwd DIR] [-- ARGV...]` | Create a persistent Lair, one Dojo, and one live Splint. |
| `new-dojo LAIR_ID [--name NAME] [--cwd DIR] [-- ARGV...]` | Add a Dojo with one live Splint without changing that Lair's lifetime. |
| `preset run NAME [--cwd DIR] [--param NAME=VALUE]... [--no-open]` | Run a packaged or user preset using one exact invoking/focused context and atomic topology transaction. |
| `preset shell-init omarchy --shell bash` | Print guarded `s*` Bash functions without writing or sourcing anything. |
| `preset shell-install omarchy --shell bash` | Create the dedicated integration file without replacing it or editing shell startup files. |
| `integration omarchy enable\|status\|disable` | Manage the complete user-level Omarchy default-terminal, terminal-tag, and screensaver setup as one reversible lifecycle. |
| `integration omarchy-screensaver enable\|status\|disable` | Manage only the legacy screensaver component. |
| `split TARGET_SPLINT_ID --axis horizontal|vertical --side first|second [--ratio N] [--cwd DIR] [-- ARGV...]` | Split a leaf and launch its sibling. |
| `ratio TARGET_SPLINT_ID RATIO` | Set the selected leaf's parent split ratio in thousandths. |
| `rename-lair LAIR_ID NAME` | Rename a Lair. |
| `rename-dojo DOJO_ID NAME` | Rename a Dojo. |
| `rename-splint SPLINT_ID TITLE` | Rename a Splint title. |
| `dojo-focus-hint DOJO_ID SPLINT_ID` | Persist a presentation hint; it does not focus a client or Window. |

`ARGV` after `--` is executed directly, never through a shell. With no explicit
argv, creation uses the configured shell. Graphical tab creation or explicit tab
naming can promote a Window-owned transient Lair when
`multiplexer.persist-on-tab-organization=yes`; non-graphical CLI mutation does
not acquire or promote a transient owner lease. A new Lair starts with `Dojo 1`. When
`new-dojo` omits `--name`, Splinterm uses one greater than that Lair's highest
existing exact `Dojo N` name; numeric gaps are not reused. Explicit names are
preserved.

```bash
splinterm new project --cwd "$HOME/src/project"
splinterm new build -- /usr/bin/ninja -C /tmp/build
splinterm new-dojo LAIR_ID --name logs -- /usr/bin/journalctl -f
splinterm preset run personal-review --cwd "$HOME/src/project"
splinterm preset run omarchy.tdl --param ai=opencode
splinterm preset run omarchy.tdl --param ai=c --param ai2=cx
splinterm preset run omarchy.tsl --param count=4 --param command='codex -a never'
splinterm split SPLINT_ID --axis vertical --side second --ratio 600
splinterm ratio SPLINT_ID 500
```

Non-dry preset runs are private trusted-local human operations. The client
compiles direct argv locally, verifies exact managed-Splint context, and asks the
daemon to persist the complete tree in one revision before any process launch.
Successful non-dry runs open the first created Dojo in a new native Window and
use its committed default-focus identity. `--no-open` keeps the operation
headless after full response reconciliation; `omarchy.t` always opens and rejects
that option. Use `--dry-run` for side-effect-free inspection. Packaged unrestricted aliases
`c`, `cx`, and `cy` fail before mutation unless
`[presets] allow-unrestricted-commands=yes` is set. See
[Dojo presets](presets.md).

Optional Bash integration uses `s`, `sdl`, `sds`, `sdlm`, and `ssl`, leaving
Omarchy's existing tmux names untouched. Generate it for inspection or install a
new dedicated file:

```bash
splinterm preset shell-init omarchy --shell bash
splinterm preset shell-install omarchy --shell bash
source "${XDG_CONFIG_HOME:-$HOME/.config}/splinterm/shell/omarchy.bash"
```

Installation never edits `.bashrc` and refuses an existing destination. Sourcing
performs one all-name preflight and defines none of the functions if any proposed
name is already an alias, function, builtin, or executable command. See
[Dojo presets](presets.md#optional-bash-integration) for mappings and argument
rules.

The ratio range is 1–999; it is the share assigned to the first child.
In-Splint automation should add `--expected-incarnation N` where offered so a
concurrent relaunch cannot silently retarget the operation.

## Process and topology lifecycle

| Command | Purpose |
| --- | --- |
| `kill SPLINT_ID` | End the live process while retaining its Splint leaf. |
| `restore SPLINT_ID` | Start one exited Splint from saved launch metadata. |
| `restore-dojo DOJO_ID` | Restore every exited Splint in a saved Dojo. |
| `restore-lair LAIR_ID` | Restore every exited Splint in a saved Lair. |
| `relaunch SPLINT_ID [--cwd DIR] [-- ARGV...]` | Relaunch an exited Splint as a new incarnation. |
| `close SPLINT_ID` | Remove an exited leaf and collapse its parent branch. |
| `close-dojo DOJO_ID` | Remove a Dojo only when all its Splints have exited. |
| `reset` | Stop the daemon, move all session state to a backup, and restart cleanly. |
| `upgrade [--now]` | Report whether a live daemon upgrade could run, or start one with `--now`; see [Daemon upgrade](#daemon-upgrade). |

Human `kill` and `reset` prompt unless `--yes` is supplied. Human `close` and
`close-dojo` remove only already-exited topology and do not add another
interactive prompt. Machine `kill`, `close`, and `close-dojo` require `--yes`;
`reset` is local human administration and has no stable machine output contract.
Restore operations are explicit—saved argv is never executed automatically.

A stable Splint ID names the persistent leaf. Every process start increments its
positive **incarnation**. Machine clients use incarnation preconditions to reject
stale targets rather than acting on a replacement process.

## Daemon upgrade

```bash
splinterm upgrade         # report only; changes nothing
splinterm upgrade --now   # start a live upgrade
```

A live upgrade hands every running terminal from the running `splinterd` to the
installed build without stopping them. It needs an installed package build
(Arch or AUR) on both sides: the running daemon and the newly installed one.
Nix installations are refused for all of Splinterm 0.2.0, and so is a running
daemon built before live upgrade was enabled: that daemon must restart once into
a build that has it. A 0.1.x daemon cannot answer the command at all; see
[Bootstrap boundary from 0.1.x](packaging.md#bootstrap-boundary-from-01x).

Whenever `splinterm upgrade` reports live upgrade as unavailable, restarting the
daemon is the upgrade path. Run these commands from a terminal Splinterm does
not own (a text console or another terminal emulator), because stopping the
daemon ends every running terminal:

```bash
systemctl --user stop splinterd.service
systemctl --user daemon-reload
systemctl --user start splinterd.service
```

A daemon that is not an installed package build, such as one started from a
development checkout, is restarted the way it was started. The command never
restarts anything by itself.

Without `--now`, the command prints the running daemon's generation, build and
provenance, its session counts, whether it kept a rollback copy, its
compatibility ranges, and every reason a live upgrade cannot run now: open
transient Lairs, a daemon that is not an installed package build, a missing
rollback copy, or a build without live-upgrade support. A daemon that was itself
started by a live upgrade or a rollback keeps no rollback copy, so it must
restart before another live upgrade.

With `--now`, open Windows that cannot take part in the upgrade are listed with
their process ID, the reason, and their Dojos, and the command asks whether to
cancel or to proceed without them. Proceeding closes those Windows; their
terminals keep running in the upgraded daemon and reopen from the Recent Dojos
picker (`splinterm dojos`). Input stays paused in every Window until the answer.

- When stdin and stderr are terminals, the question is asked on stderr as
  `[c]ancel / [p]roceed`. Only `p` or `proceed` proceeds; an empty or
  unrecognized answer, end of input, or no answer before the daemon's deadline
  cancels.
- Without a terminal, or when the command runs inside a Splinterm terminal
  (whose input the upgrade pauses), a small trusted dialog titled
  "Splinterm — Upgrade", in the style of the access consent dialog, asks
  instead. `D`, `Escape`, `Enter`, or the red left **Cancel** button cancels;
  only `G` or the green right **Proceed** button proceeds. Its keys stay inert
  until you click inside it or
  press Ctrl+Alt+G, and under Omarchy it can appear without focus (see
  [Trusted dialogs](usage.md#trusted-dialogs)); keys and the proceed click
  count only about 600 ms after that, so typing meant for another Window cannot
  answer it. Once armed, a key acts when it goes down: a key held down since
  before arming never counts, and key repeats never do, but a key pressed after
  arming counts as a press even if it is held. Closing the dialog or not
  answering cancels; with no answer in time the command prints "No answer about
  the Windows within N seconds; cancelling." If the list of Windows is too long for the dialog, the last
  line counts the rest. Without a graphical session either, the upgrade is
  cancelled.

A committed upgrade never answers the request: the old daemon closes the
connection and starts the installed build. A connection that closes for another
reason, such as a stopped service or a crash, looks the same, so the command then
reconnects for up to 15 seconds and reports which daemon serves. A live upgrade
and a rollback both replace the daemon's program in place, so the daemon keeps
its process ID and start time, which the command records before the upgrade
and checks after it. A restarted daemon is a new process, even when it reports
the same generation and build. In the same process, an upgrade serves the next
generation with the new build and a rollback serves the same generation with
the previous build. Only once an upgrade or a rollback is confirmed this way
does the command say that the dropped Windows' terminals keep running;
otherwise it says that what happened to them is unknown.

Only the installed `splinterm` beside the running `splinterd` may ask: on
packaged systems `/usr/bin/splinterm`, matched by device and inode. A
development build or a copy earlier on `PATH` is refused as unauthorized. The
command has no machine output and rejects `--output`, `--schema-major`,
`--timeout-ms`, and `--remote`.

| Exit status | Meaning |
| --- | --- |
| 0 | The report was shown, or the upgrade completed: the same daemon process serves the next generation with the new build. |
| 1 | Error: no daemon, not the installed trusted client, a refused request, an invalid daemon response, or, after the connection closed, a restarted daemon (a new process, whose terminals most likely ended) or the same process with neither an upgrade nor a rollback. |
| 2 | Usage error. |
| 3 | Cancelled by the answer, by closing the dialog, by no answer in time, or because nothing could ask. Nothing changed. |
| 4 | Live upgrade is unavailable now. Nothing changed. |
| 5 | Another upgrade is already in progress. |
| 6 | The upgrade did not complete: the same daemon process serves the same generation with the previous build, after a rollback or because it never left. Terminals were kept. |
| 7 | The connection closed as a committed upgrade's does, but the result is unconfirmed: no daemon answered within 15 seconds, the daemon process could not be identified (for example from another PID namespace), or the daemon no longer accepts this `splinterm`, for example because the installed file was replaced while the upgrade ran. Run `splinterm upgrade` to check. |

### Desktop launch with another daemon build

The desktop launcher (`splinterm-xdg-terminal-exec` and its Dojo aliases)
never restarts a daemon that may still run shells without asking, and never
starts a live upgrade. Before opening a Window it reads the same report as
`splinterm upgrade`, through a private command, and compares the running
daemon's build identity with the installed client's:

- the installed build opens the Window; no daemon starts the service;
- another build that reports no live terminal restarts without asking;
- another build with live terminals, or a daemon it cannot read (a 0.1.x
  daemon, another protocol, or no answer, after one retry), opens a trusted
  dialog titled "Splinterm — Upgrade". **Not now** is the default and opens
  nothing: `Enter`, `Escape`, `D`, the red left **Not now** button, closing the
  dialog, or no answer within 120 seconds. Only `R` or the green right button
  chooses **Restart now, end N shells**, with the daemon's exact count; for a
  daemon it cannot read, "end about N shells" from a best-effort count of
  running workload units, or "end all running shells". The dialog takes no
  keyboard focus;
  keys count only about 600 ms after a click inside it or Ctrl+Alt+G. Once the
  dialog is armed, `R` acts when it goes down, so a hold that began before
  arming is ignored but one that begins after arming counts as a press; key
  repeats never count. With no answer within 120 seconds the launcher prints
  "splinterm: no answer within 120 seconds, so nothing was restarted." When the
  daemon reports that nothing blocks a live upgrade, the dialog says to choose
  **Not now** and run `splinterm upgrade`;
- a daemon it cannot read whose executable is the installed `splinterd` is
  busy, not incompatible: the launch fails and says to try again;
- an unattended launch, with an explicit app ID (such as the Omarchy
  screensaver's) or a command, never shows the dialog and opens nothing
  instead; an idle restart stays automatic.

A restart happens only while the daemon that answered is
`splinterd.service`'s main process, and concurrent launches take a lock, so
nothing is restarted twice.

See [Packaging](packaging.md#desktop-launcher-and-the-running-daemon) for the
complete state table and the 0.1.x bootstrap boundary.

### Commands during an upgrade

While an upgrade prepares, the daemon refuses requests that would change
something, and a committed upgrade ends every client connection as the daemon
starts the installed build. Commands, in human and machine mode, never retry a
mutation, because whether it took effect is unknown:

- A refused command did not run. It fails with "splinterd is upgrading, so it
  refused this command, which did not run". Run it again once the upgrade
  finishes.
- A command whose connection ends while it runs fails with an error that names
  the upgrade, for example "splinterd was upgraded while this command ran
  (generation 3 → 4); the command was not retried and may or may not have taken
  effect, so check the current state before running it again". To say which
  daemon serves now, the command reconnects once for up to 10 seconds; only the
  handshake runs, never a request.
- `ping`, `list`, `topology`, `inspect`, and `focus` only read and are safe to
  repeat, so after a lost connection they are sent once more on that fresh
  connection. If that is interrupted too, the error ends "the read was retried
  once and was interrupted again, so run it again". Reads that carry
  continuation state (`snapshot`, `scrollback`, `search`) and `subscribe`
  streams are not repeated; they fail with the same kind of error, ending "the
  read was not retried, so run it again".

In machine mode a refused command reports `resource_limit`, retryable, and exits
5. A lost connection reports `internal`, retryable only for a read, and exits 4.
The reconnection fits within what is left of `--timeout-ms`; with nothing left,
the command does not reconnect and says its deadline left no time to look for
the daemon.

## Terminal observation and input

| Command | Purpose |
| --- | --- |
| `snapshot SPLINT_ID` | Read one bounded live semantic terminal snapshot. Development mode only. |
| `scrollback SPLINT_ID [--max-rows N] [--cursor CURSOR]` | Read one bounded history page. |
| `search SPLINT_ID QUERY [--case-sensitive] [--max-results N] [--cursor CURSOR]` | Search history without echoing the query in machine output. |
| `send SPLINT_ID TEXT` | Send literal UTF-8 through an atomic controller workflow. Development mode only. |
| `resize SPLINT_ID COLUMNS ROWS` | Resize through an atomic controller workflow. Development mode only. |

```bash
splinterm snapshot SPLINT_ID
splinterm scrollback SPLINT_ID --max-rows 32
splinterm search SPLINT_ID 'failed' --max-results 20
splinterm send SPLINT_ID $'printf "ready\\n"\n'
splinterm resize SPLINT_ID 120 40
```

Terminal reads carry exact Splint, incarnation, terminal revision, and history
generation. Continuation cursors are opaque. Public machine records contain
semantic Unicode cells, not raw daemon frames. Input and search bodies are never
copied into bounded audit metadata.

## Agent status

| Command | Purpose |
| --- | --- |
| `agent-status set STATE [--detail TEXT] [--ttl SECONDS]` | Report `working`, `needs_input`, `done`, `error`, or `idle` for the current Splint. |
| `agent-status clear` | Clear the current Splint's reported status. |
| `agent-status get [SPLINT_ID]` | Read one Splint's reported status. |

```bash
splinterm agent-status set needs_input --detail 'waiting for approval'
splinterm agent-status clear
splinterm --output json agent-status get
```

Inside a Splint the target defaults to `$SPLINTERM_SPLINT_ID` and
`$SPLINTERM_SPLINT_INCARNATION`; `set` and `clear` accept `--splint ID
--incarnation N` together to override it. Without either, the command fails with
the usage error `not inside a Splint` and exits 2. A process in the Splint's
session or process ancestry reports without a grant; others, including agents
that fully detach from the Splint, need `agent_status_report`. Status is display
data only and is never inferred from terminal content. See
[Automation](automation.md#agent-status) for authority, staleness, machine
output, and agent hook recipes.

## Subscriptions

Subscriptions require NDJSON:

```bash
splinterm subscribe terminal SPLINT_ID --output ndjson
splinterm subscribe topology --output ndjson
splinterm subscribe control SPLINT_ID --output ndjson
```

Terminal and control subscriptions accept an expected-incarnation precondition.
Each stream begins with a current-state record. Sequence gaps, replaced history,
or a stalled client emit a bounded `resync_required` record and terminate; the
caller must explicitly resubscribe.

## Authorization, policy, and audit

| Command | Purpose |
| --- | --- |
| `authorization status SPLINT_ID` | Inspect effective authority for one Splint. |
| `authorization revoke GRANT_ID` | Revoke a just-this-once grant for one Splint. |
| `access list` | List every agent grant of every duration, pending requests, and matching policy rules. |
| `access revoke ID [--yes]` | Revoke one grant: a runtime grant ID or a remembered `ra_` entry ID. |
| `access revoke --all [--dry-run] [--yes]` | Revoke every agent grant and pending request after showing the exact counts; policy rules are untouched. |
| `approvals` | Answer agent access prompts as text in your own terminal on the daemon's computer, for one without a Window; refused inside a Splinterm pane. See [Usage](usage.md#answering-access-prompts-in-a-terminal). |
| `mcp setup claude-code\|codex\|vscode [--dry-run] [--replace]` | Add the installed `splinterm-mcp` adapter to an agent host's configuration as the `splinterm` server; grants no access and writes no policy. The VS Code form is not yet verified against a real install. |
| `mcp remove claude-code\|codex\|vscode [--dry-run] [--force]` | Remove that entry (`--force` for one setup did not write); revokes no grant. |
| `policy validate PATH` | Validate a policy through the daemon's secure loader without publishing it. |
| `policy inspect PATH` | Print normalized validated policy. |
| `policy reload` | Request reload through the canonical user service. |
| `audit [--after ID] [--max-records N]` | Read bounded daemon-lifetime body-free audit metadata. |

Persistent policy commands are local administration rather than a remote machine
surface. A missing policy grants no third-party persistent authority. Policy
reload is atomic, disconnects automation-role clients, and fails closed to a new
deny-all generation if the configured document is rejected. Read
[Automation](automation.md) and [Headless operation](headless.md) before changing
policy.

## Configuration, keymaps, and remotes

Local inspection does not contact the daemon:

```bash
splinterm config check
splinterm keymap list
splinterm keymap show
splinterm keymap show splinterm
splinterm keymap conflicts
splinterm remote list
splinterm remote inspect PROFILE
```

With the `omarchy-tmux` profile, `Prefix+?` opens a read-only trusted overlay
built from the resolved keymap. `Prefix+[` enters vi copy mode: move with
`h/j/k/l`, arrows, Home/End, or PageUp/PageDown; press `v` to select, `y` to copy
and leave, or Escape to cancel. Selection spans visible and bounded loaded
history without sending keys to the terminal application.

Outside copy mode, both built-in profiles use `Ctrl+Shift+C/V` and `Super+C/V`
to copy the terminal selection and perform safe/bracketed paste. The
`omarchy-tmux` profile advertises `Ctrl+Shift+C/V` as its primary
compositor-safe route. For its universal `Super+C/V`, current Omarchy
(`quattro` since `129c67e2`, after 4.0.4) sends terminal-tagged windows
`Ctrl+Shift+C/V`, while Omarchy 3.x and 4.0.x send
`Ctrl+Insert`/`Shift+Insert`; both profiles accept both. Omarchy must classify
`com.oldjobobo.splinterm` as terminal, which `splinterm integration omarchy
enable` arranges; otherwise Omarchy sends ordinary `Ctrl+C`, which remains
terminal interrupt. In copy mode, `Super+C` copies the active selection and exits,
while `Super+V/X/Z` are consumed locally. These Super shortcuts work only when
the compositor delivers the chord to the Splinterm Window. Splinterm-owned
command-palette, keybinding-help, search, and rename fields accept effective
`clipboard.copy` / `clipboard.paste` bindings such as `Ctrl+Shift+C/V`; they
also support `Super+A/C/V/X/Z` and Shift+Left/Right for bounded local selection,
clipboard editing, and undo. Terminal panes do not claim Super+X/Z as
universal cut/undo because the child application owns its input buffer.

`remote check PROFILE` additionally starts bounded SSH/relay/daemon read-only
probes but does not map a Window. `--remote PROFILE` binds compatible graphical
or CLI operations to that configured endpoint:

```bash
splinterm remote check server
splinterm --remote server dojos
splinterm --remote server --output json topology
```

`splinterm remotes` returns at once with exit status 0 and prints nothing. It
starts the remote launcher detached from the terminal: one Window that lists the
configured profiles, shows the chosen profile's connection, lists that host's
Dojos, and then becomes the remote terminal. The command palette's **Open
Remote…** shows the same profiles as a pop-up and opens the chosen one in
exactly one new Window.

See [Configuration](configuration.md) for strict INI/keymap/profile schemas and
[Remote access](remote.md) for authentication and authority boundaries.

## Relay and service-facing commands

`relay --stdio` is the byte-transparent policy-scoped SSH automation transport.
`relay --graphical-stdio` is the bounded private graphical multiplexer. They are
fixed transport entry points, not interactive shell commands and not public JSON
schemas. `splinterd` itself exposes no network listener.

Service startup, persistence, backups, upgrades, and recovery are documented in
[Headless operation](headless.md) and [Packaging](packaging.md).

## Machine-operation summary

The supported schema-major-2 one-shot operation inventory is frozen in
[Automation](automation.md#one-shot-command-and-operation-inventory). Important
rules:

- JSON one-shots emit exactly one envelope; NDJSON is subscription-only.
- Syntax errors produce no machine stdout and exit 2.
- Parsed failures use stable categories: authorization 3, connection/schema 4,
  invalid/stale/resource 5, timeout/cancellation 6, unexpected internal 70.
- Machine mode never prompts; required confirmations use explicit `--yes`.
- Raw private protocol versions, request IDs, controller IDs, and Rust DTOs are
  not compatibility promises.
- `--remote` changes transport, not the public schema or authority required.

Checked-in public schemas live under [`dist/schemas/v2/`](../dist/schemas/v2/).
