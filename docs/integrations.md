# Client integrations and in-Splint context

Splinterm integrations consume the documented `splinterm --output json` and
`--output ndjson` contracts. They do not connect to the private daemon protocol,
parse human output, or inherit authority from their logical location. See
[automation.md](automation.md) for the complete command, schema, error, and
policy contract. The optional full-capability stdio adapter and supported host
configuration are documented separately in [mcp.md](mcp.md).

## Unified Omarchy setup

Use one explicit command to make Splinterm behave as Omarchy's complete default
terminal for the current user:

```bash
splinterm integration omarchy enable
splinterm integration omarchy status
```

The lifecycle manages three independently verified components:

1. the `xdg-terminal-exec` preference, preserving the previous
   `xdg-terminals.list` object exactly;
2. an exact user-owned Hyprland module that applies Omarchy's `terminal` tag to
   `com.oldjobobo.splinterm`, so universal `Super+C/V` arrives as copy and
   paste (`Ctrl+Insert`/`Shift+Insert` or `Ctrl+Shift+C/V`, depending on the
   Omarchy revision; see below), and maps Splinterm's trusted dialogs
   (`com.oldjobobo.splinterm.trusted-dialog`: the access prompt, the upgrade
   question, and the launcher's restart question) with `no_initial_focus` and
   `focus_on_activate = false`, so they never take keyboard focus when they
   appear; and
3. the guarded Omarchy screensaver adapter described below.

Trusted dialogs ignore every key until you click inside them or press the
dialog shortcut, Ctrl+Alt+G, while they have focus, with or without this
module; the module also keeps them from taking focus at all. The module has no
rule to float them: each dialog asks Hyprland for one fixed size, which
Hyprland floats on its own ([Trusted dialogs](usage.md#trusted-dialogs)). A module that
an earlier release wrote has only the terminal tag. The **Dialog focus** row of
`splinterm integration omarchy status` then says `missing`, and running
`splinterm integration omarchy enable` again upgrades it in place. With a
terminal tag rule of your own instead of the module, add the same dialog rule to
one of your `~/.config/hypr/*.lua` files; status counts it there too:

```lua
hl.window_rule({
  name = "splinterm-trusted-dialog-focus",
  match = { initial_class = "^com\\.oldjobobo\\.splinterm\\.trusted-dialog$" },
  no_initial_focus = true,
  focus_on_activate = false,
})
```

Enable preflights every destination before mutation, records a versioned pending
journal, verifies terminal resolution and Hyprland reload, and rolls back on a
failed verification. Status performs no reload or mutation. Disable removes only
unchanged Splinterm-managed files, restores the exact previous terminal
preference, and leaves externally configured components untouched:

```bash
splinterm integration omarchy disable
```

Package installation never runs this command automatically. It remains a
separate, explicit user action.

### Reaching a waiting dialog from the keyboard

With the trusted dialog rule above in place, from the module or your own file,
a waiting dialog has no keyboard focus, so Splinterm also makes the dialog
shortcut work from any Window. When a trusted dialog appears, it runs
`hyprctl eval` in the background to enable a binding of Ctrl+Alt+G (or the
chord in [`[integration] dialog-shortcut`](configuration.md#supported-keys))
in Hyprland's running Lua state, creating it the first time with the
description `Splinterm: focus the waiting trusted dialog`. The binding only
focuses the waiting dialog, matched by its process ID and the trusted dialog
app ID, and lets the chord through to it, so the dialog engages as if clicked
and then arms; the chord never answers. Nothing is written to your Hyprland
files.

- The binding is enabled only while a dialog waits. The dialog removes its
  entry once it has answered, and the process that started it removes the
  entry again after reaping it, so a killed or crashed dialog leaves nothing
  behind; `splinterm upgrade` and the desktop launcher also remove their
  dialogs' entries when they are interrupted or terminated. Each new dialog
  drops entries whose process is gone. With the last entry the binding is
  disabled, not removed: removing a Hyprland binding removes every binding
  of the same chord. `hyprctl reload` clears it with the rest of the runtime
  state.
- `hyprctl binds` therefore keeps listing Splinterm's binding after the first
  dialog, enabled or not. A disabled binding does nothing, and while no
  listed dialog has a Window on screen even an enabled one focuses nothing
  and the chord reaches the focused application unchanged.
- When your configuration already binds the chord, in any submap, Splinterm
  enables nothing and your binding keeps working. Splinterm checks
  `hyprctl -j binds` for a binding with the same modifiers and key; a binding
  written by keycode (`code:42`) is not recognized, so choose another chord
  if you bind it that way.
- Set `dialog-shortcut=none` to turn the shortcut off; dialogs then engage only
  by a click.

### Why the terminal tag matters

Omarchy decides which windows are terminals with one Hyprland rule,
`default/hypr/apps/terminals.lua` under `$OMARCHY_PATH`. It matches Alacritty,
kitty, Ghostty, foot, WezTerm and Omarchy's own app IDs, but not
`com.oldjobobo.splinterm`. Omarchy's universal clipboard bindings then treat a
Splinterm Window as an ordinary application:

* `Super+C` sends `Ctrl+C`, which interrupts the foreground program instead of
  copying.
* `Super+V` sends `Ctrl+V` instead of pasting.
* Rules that single terminals out do not apply. These include Omarchy's
  terminal transparency and its `Ctrl+Space` binding, which returns the input
  method to direct input so a multiplexer prefix, such as the `omarchy-tmux`
  profile's, reaches the terminal.

With the tag, Omarchy sends a terminal copy and paste chord instead. Omarchy
3.x and the released 4.0.0–4.0.4 send `Ctrl+Insert`/`Shift+Insert`. Current
Omarchy, on its `quattro` branch since commit `129c67e2` (after 4.0.4), sends
`Ctrl+Shift+C/V`. Both built-in keymap profiles bind both forms to copy and
paste. The tag comes from the
integration's Hyprland module above. Status also counts the same rule, with the
module's exact text, in any other `~/.config/hypr/*.lua` file.

Because the integration stays opt-in, Splinterm points to it in two places:

* The Pacman and AUR install and upgrade messages recommend
  `splinterm integration omarchy enable` and explain what it fixes.
* The first local graphical Window under Omarchy 4 shows one desktop
  notification when no terminal tag rule is present. "Under Omarchy 4" means a
  Hyprland session in which both the Lua entrypoint
  `~/.config/hypr/hyprland.lua` and Omarchy's terminal rule
  (`default/hypr/apps/terminals.lua` under `$OMARCHY_PATH`, or under
  `/usr/share/omarchy` when that is unset) exist. A plain Hyprland Lua setup
  without Omarchy, and Omarchy 3.x with its `hyprland.conf`, which the
  integration cannot extend, never show it. Neither does a client connected
  to a private or development daemon through `SPLINTERM_SOCKET`, as graphical
  test harnesses are, nor a Window with another app ID, such as the
  screensaver. The check only reads files and never delays opening the
  Window. When the notification is handed to `notify-send`, Splinterm creates
  `${XDG_STATE_HOME:-~/.local/state}/splinterm/integrations/omarchy-hint-shown`
  and never shows it again; delete that file to see it once more. If
  `notify-send` cannot start or exits with an error, for example because no
  notification service is running, the file is removed again and a later
  launch tries again. Set `[main] omarchy-integration-hint=no` to never show
  it.

## Omarchy screensaver component

Splinterm's package advertises the desktop-standard XDG app-ID argument and
installs a dedicated profile at:

```text
/usr/share/splinterm/omarchy/screensaver.ini
```

The package also installs a Splinterm-owned launcher helper at:

```text
/usr/lib/splinterm/integrations/omarchy-launch-screensaver
```

The unified command enables this component automatically. The legacy
component-only lifecycle remains available for compatibility:

```bash
splinterm integration omarchy-screensaver enable
```

Omarchy 4 puts its own `bin` directory first on `PATH` for everything Hyprland
starts, including the idle service that launches the screensaver, so a launcher
in `~/.local/bin` is never reached. Activation therefore manages three objects:

```text
~/.local/share/splinterm/omarchy/bin/omarchy-launch-screensaver
  -> /usr/lib/splinterm/integrations/omarchy-launch-screensaver
~/.config/hypr/splinterm-screensaver.lua
~/.config/hypr/hyprland.lua   (one marked require block appended)
```

The Hyprland module runs after Omarchy's bootstrap and moves only that
Splinterm-owned directory ahead of Omarchy's `bin`; it holds no other command.
The command refuses to replace any existing file, different symlink, or edited
module, and rolls back if the reload is rejected or reports configuration errors
that were not already present. The running Omarchy shell keeps the `PATH` it
started with; run `omarchy-restart-shell`, or log in again, before the idle
service uses the new launcher. A launcher link left in `~/.local/bin` by an earlier version is shadowed
on Omarchy 4; status reports it, and enabling again, with either this command or
`splinterm integration omarchy enable`, moves it to
`~/.local/share/splinterm/integrations/omarchy-launch-screensaver.disabled`.
Enabling requires Omarchy 4's `~/.config/hypr/hyprland.lua`. Without it, a link
left in `~/.local/bin` by an earlier version still runs, and status reports it
as enabled. When the command runs outside a Hyprland session it cannot reload
Hyprland and says so; run `hyprctl reload` in the session, or log in again.
Inspect or disable the integration with:

```bash
splinterm integration omarchy-screensaver status
splinterm integration omarchy-screensaver disable
```

When Splinterm is selected, the helper launches one screensaver Window per
monitor. Each one uses the dedicated profile and the normal XDG terminal
adapter:

```bash
env SPLINTERM_CONFIG=/usr/share/splinterm/omarchy/screensaver.ini \
  xdg-terminal-exec --app-id=org.omarchy.screensaver -- omarchy-screensaver
```

The helper follows the launcher on Omarchy's `quattro` branch since commit
`bf44355a` (after 4.0.4). That revision adds a static rule that sends every
`org.omarchy.screensaver` Window to the hidden `special:screensaver` workspace,
and `omarchy-screensaver` exits as soon as its Window loses focus. So the helper
focuses each monitor in turn and starts that monitor's Window through a
Hyprland exec rule, `[workspace special:screensaver-<monitor>]`, which outranks
the static rule. If the monitor already shows a special workspace, such as the
scratchpad, the Window joins it instead, so it is still open afterwards. The
released Omarchy 4.0.0–4.0.4 launcher uses a plain exec without a workspace
rule and has no parking rule; the helper uses the same per-monitor special
workspace there, which is meant to work the same way. Before
moving on, the helper waits up to five seconds for a Window that carries this
launch's private invocation token. If one never appears, it closes only the
Windows this launch opened. Afterwards it focuses the original monitor again,
and once every screensaver Window has closed it hands focus back to that
monitor.

Hyprland 0.56 matches an exec rule through a token it puts in the spawned
environment, and falls back to the process ID. Both survive the launch chain:
`xdg-terminal-exec`, `splinterm-xdg-terminal-exec` and the shell all replace
themselves with `exec`, so the `splinterm xdg-launch` process that owns the
Window is the process Hyprland spawned.

When another terminal is selected, the helper hands over to Omarchy's own
launcher unchanged. It looks for `bin/omarchy-launch-screensaver` under the
session's `$OMARCHY_PATH` when that is an absolute path, as it is for an
Omarchy checkout, and otherwise under `/usr/share/omarchy`. It never hands over
to itself. Enabling either integration command requires that launcher in one of
those two places.

The command-bearing launch remains a transient client-bound Lair. The app-ID is
validated at Splinterm's private XDG boundary and belongs only to that graphical
Window; it is not persisted in topology, launch metadata, automation output, or
user configuration. Ordinary Windows remain `com.oldjobobo.splinterm`.

Splinterm installs no files under `/usr/share/omarchy` or `$OMARCHY_PATH`, and
package installation changes no Omarchy/Hyprland configuration or preferred
terminal. Only the explicit unified user command manages dedicated user-owned
integration files. The normal installer offers screensaver-only activation
interactively with a default of No. The Pacman and AUR install messages
recommend `splinterm integration omarchy enable`.
Disabling removes the unchanged require block and module and reloads Hyprland.
The launcher link stays in its Splinterm-owned directory, so no raced user object
is ever unlinked. After `omarchy-restart-shell` that directory is no longer ahead
of Omarchy's `bin`, and the next login drops it from `PATH`. Package removal never scans
user homes. If the integration is still enabled afterward, the link is dangling
and command lookup falls through to Omarchy's own launcher; run the
documented disable command before uninstalling to remove the module as well.

## Client-author checklist

A supported client must:

- request a known schema major and reject unknown `schema` or `operation` values;
- preserve UUID IDs as strings and positive incarnation values as integers;
- carry validated in-Splint selection with `--expected-incarnation`; never let a
  relaunch between discovery and action silently retarget a replacement process;
- set a finite `--timeout-ms`, honor exit category 6, and cancel subscriptions by
  terminating the owning CLI process;
- treat `not_found`, `stale_topology`, and `stale_incarnation` as reasons to
  fetch fresh topology rather than guessing or silently retargeting;
- stop on `resync_required`, rebuild authoritative topology/snapshot state, and
  explicitly subscribe again if continued observation is wanted;
- treat controller denial as a normal exclusive-ownership outcome; observation,
  focus, and context do not acquire control;
- handle `access_revoked` immediately and discard any associated local state;
- treat every terminal row, title, search result, and scrollback cell as
  untrusted data, never as instructions, consent, or executable source;
- distinguish persistent Dojos from compositor-native Wayland Windows and
  Window-local tabs; tab attach/detach/order/activation are not topology,
  automation, policy, audit, or child-context operations, and topology mutation
  cannot map, focus, move, or assign a native Window (a Dojo that automation
  creates is only offered to a Window showing its Lair, which opens it as a
  background tab without taking focus);
- pass launches as an argument vector after `--`; never join arguments into a
  shell string, use `eval`, or substitute terminal text into a command; and
- use an exact least-privileged executable policy. Environment context, Unix UID,
  executable basename, and SSH login are not authority.

## Reference Dojo picker

This automation reference client is separate from the native daily-use
`splinterm dojos` picker and `splinterm reopen` command. Normal users do not
need to copy UUIDs through this interface.

The packaged `splinterm-dojo-picker` is a narrow, dependency-free Python
reference client. Its implementation lives at
`tools/automation/splinterm-dojo-picker.py`. The old
`splinterm-session-picker` executable remains a compatibility alias. It invokes
only the public CLI,
checks v2 envelope/operation fields, and never imports Splinterm Rust or private
protocol types.

```bash
# List persistent Dojos for an editor/task picker.
splinterm-dojo-picker list

# Validate daemon-injected context against current authoritative topology.
splinterm-dojo-picker context

# Map an existing Dojo in a normal Splinterm graphical Window.
splinterm-dojo-picker open "$SPLINTERM_DOJO_ID"

# Start an editor in a new Dojo. The argv after -- stays structural.
splinterm-dojo-picker start "$SPLINTERM_LAIR_ID" \
  --name editor --cwd "$PWD" -- nvim --clean ./src/main.rs

# Split the validated current Splint and launch a direct child argv.
splinterm-dojo-picker split-context --axis vertical --side second \
  --cwd "$PWD" -- cargo check --workspace

# Observe one bounded snapshot. Literal input uses atomic controller acquisition;
# controller denial is surfaced and never bypassed or forced.
splinterm-dojo-picker snapshot-context
splinterm-dojo-picker send-context $'printf "ready\\n"\n'

# Watch until an explicit resync and rebuild authoritative state.
splinterm-dojo-picker watch-context
```

`open` maps a graphical client with the selected Dojo as its initial tab and is
intentionally separate from logical mutation. Within that client, native picker
selection may open or activate additional Window-local tabs without exposing a
tab API through the public CLI. No graphical test is required for the parser and lifecycle contract;
the command's exact argv construction is covered by the fake-CLI test harness.

The picker supplies the validated incarnation precondition to split, snapshot,
input, and subscription calls. The CLI combines it with its own fresh lookup and
revision-aware daemon request, closing the discovery/action race. The picker
exits with the public CLI category when an operation is denied, stale, missing,
cancelled, or disconnected. It does not retry a denied mutation,
fall back to the development bypass, pick another Splint, or treat context as a
credential. `watch-context` stops at `resync_required`, fetches fresh topology,
revalidates the context, fetches a bounded snapshot, and reports reconciliation.
A long-running host may then explicitly start another subscription.

## Daemon-injected discovery hints

Every PTY child receives these daemon-overridden values:

- `SPLINTERM_LAIR_ID`
- `SPLINTERM_DOJO_ID`
- `SPLINTERM_SPLINT_ID`
- `SPLINTERM_SPLINT_INCARNATION`

The daemon replaces inherited values, and relaunch injects the new incarnation.
They are initial-selection hints only. A child may alter or forward them; the
daemon never reads them back for authentication, policy matching, ancestry,
consent, or controller ownership. Validate them with `topology` before use.
Missing, malformed, stale, moved, exited, or unauthorized context must be
surfaced and must never select an arbitrary replacement.

## Shell and `jq` examples

Keep machine stdout separate from diagnostics and validate the closed envelope
before extracting fields:

```bash
set -euo pipefail

topology=$(splinterm --output json --schema-major 2 --timeout-ms 5000 topology)
printf '%s\n' "$topology" | jq -e '
  .schema == "splinterm.cli.v2" and
  .operation == "inspect_topology" and
  .ok == true and
  (.data.dojos | type == "array")
' >/dev/null
printf '%s\n' "$topology" | jq -r '.data.dojos[] | [.dojo_id, .name] | @tsv'
```

Select the injected context only after matching all authoritative fields:

```bash
printf '%s\n' "$topology" | jq -e \
  --arg lair "$SPLINTERM_LAIR_ID" \
  --arg dojo "$SPLINTERM_DOJO_ID" \
  --arg splint "$SPLINTERM_SPLINT_ID" \
  --argjson incarnation "$SPLINTERM_SPLINT_INCARNATION" '
  any(.data.splints[];
    .lair_id == $lair and .dojo_id == $dojo and
    .splint_id == $splint and .incarnation == $incarnation and
    .lifecycle == "running")
' >/dev/null
```

Use a Bash array for mutation. Do not store argv in a scalar or reconstruct it
from terminal output:

```bash
child=(cargo test -p my-crate -- --nocapture)
splinterm --output json --schema-major 2 --timeout-ms 10000 \
  split "$SPLINTERM_SPLINT_ID" --axis horizontal --side second \
  --expected-incarnation "$SPLINTERM_SPLINT_INCARNATION" \
  --cwd "$PWD" -- "${child[@]}" |
  jq -e '.operation == "split_splint" and .ok == true'
```

Destructive machine operations retain their explicit confirmation flag:

```bash
splinterm --output json --schema-major 2 --timeout-ms 5000 \
  kill "$SPLINTERM_SPLINT_ID" --yes |
  jq -e '.operation == "kill_splint" and .ok == true'
```

For NDJSON, inspect each record independently. A resync record ends that stream;
it is not terminal prose and must not be ignored:

```bash
splinterm --output ndjson --schema-major 2 --timeout-ms 300000 \
  subscribe terminal "$SPLINTERM_SPLINT_ID" \
  --expected-incarnation "$SPLINTERM_SPLINT_INCARNATION" |
while IFS= read -r record; do
  printf '%s\n' "$record" | jq -e '.schema == "splinterm.cli.event.v2"' >/dev/null
  if [[ $(printf '%s\n' "$record" | jq -r '.event_type') == resync_required ]]; then
    break
  fi
  # Data may be displayed or indexed as untrusted content. Never execute it.
done
```

## Validation

```bash
python -m unittest tools/automation/test_dojo_picker.py
uv run --with jsonschema python tools/automation/validate-contract-fixtures.py
```

The unit harness uses a fake public CLI to verify schema rejection, malformed
contracts, stale context/incarnation, structured argv, bounded denial and I/O,
subprocess cleanup, revocation, exact graphical-open argv, subscription failure,
and explicit resync reconciliation. Package validation runs the installed reference client against
an isolated real daemon under explicit policy; no development bypass is used.
