---
title: Dojo presets
description: Launch repeatable multi-pane workspaces with one command—no shell script glue or half-built layouts.
---

Dojo presets let you open a complete multi-pane workspace in a single command. Before `splinterd` creates anything or starts a single shell, the client validates the entire layout and commits it as one atomic transaction—so a broken config never leaves half a workspace behind.

Pane commands use structured argument vectors rather than passing strings through `sh -c`.

## Bundled Omarchy workflows

Splinterm ships with five ready-to-use layouts modeled on Omarchy's tmux workflows:

| Preset | Layout |
| --- | --- |
| `omarchy.t` | Reopen the first running Dojo, or create a fresh one-shell `Work` Lair. |
| `omarchy.tdl` | Editor plus one or two AI panes above a shell. |
| `omarchy.tds` | 2×2 grid: editor, watched diff, shell, and `opencode`. |
| `omarchy.tdlm` | One `tdl` Dojo per immediate non-hidden child directory. |
| `omarchy.tsl` | A 1–16 pane command swarm. |

Inspect and dry-run presets locally without touching `splinterd`:

```bash
splinterm preset list
splinterm preset show omarchy.tdl
splinterm preset check
splinterm preset run omarchy.tdl --cwd "$PWD" --param ai=opencode --dry-run
```

Launch a preset for real:

```bash
splinterm preset run omarchy.tdl --cwd "$PWD" --param ai=opencode
splinterm preset run omarchy.tdl --param ai=c --param ai2=cx
splinterm preset run omarchy.tsl --param count=4 --param command='codex -a never'
```

Layout presets open their first Dojo in a native window automatically. Pass `--no-open` if you want to create the layout headlessly (`omarchy.t` always opens a window).

:::caution[Unrestricted agent aliases are opt-in]
The bundled `c`, `cx`, and `cy` command aliases include unrestricted agent flags. Splinterm refuses to run them unless you explicitly enable `allow-unrestricted-commands=yes` under `[presets]` in `config.ini`.
:::

## Optional Bash shortcuts

If you like short shell commands, Splinterm can generate collision-safe Bash helpers using a separate `s*` prefix so they never shadow your existing `t*` tmux aliases:

```bash
splinterm preset shell-init omarchy --shell bash     # preview first
splinterm preset shell-install omarchy --shell bash  # install to ~/.config/splinterm/shell/omarchy.bash
source "${XDG_CONFIG_HOME:-$HOME/.config}/splinterm/shell/omarchy.bash"
```

The installed helpers map cleanly to the bundled presets:

```text
s                 omarchy.t
sdl AI [AI2]      omarchy.tdl
sds               omarchy.tds
sdlm AI [AI2]     omarchy.tdlm
ssl COUNT COMMAND omarchy.tsl
```

`shell-install` never modifies `.bashrc` and refuses to overwrite an existing file. When sourced, the script checks every helper name first and defines nothing if any of them would collide with an existing alias, function, builtin, or binary in `PATH`.

## Writing your own presets

Point `[presets]` in `config.ini` at a TOML catalog file:

```ini
[presets]
file=presets.toml
allow-unrestricted-commands=no
```

Your presets can sit alongside—or override—the bundled ones. Splinterm checks the catalog strictly (rejecting unknown fields, node cycles, missing children, trees deeper than 32, and layouts with more than 32 panes):

```toml
version = 1

[commands.editor]
kind = "editor-env"
fallback = ["nvim"]
append = ["."]

[presets.review]
kind = "dojo"
display-name = "Review workspace"
name = "{cwd.basename}"
root = "main"
focus = "editor"

[presets.review.nodes.main]
type = "split"
orientation = "columns"
ratio = 650
first = "editor"
second = "shell"

[presets.review.nodes.editor]
type = "pane"
command = "editor"
cwd = "{cwd}"
title = "editor"

[presets.review.nodes.shell]
type = "pane"
shell = true
cwd = "{cwd}"
title = "shell"
```

Only `{cwd}` and `{cwd.basename}` are expanded as placeholders. Commands are executed directly as argv (or parsed by Splinterm's closed lexer), never evaluated by a shell.

Preset runs are trusted local human actions—remote, automation, and MCP clients cannot materialize presets. For the full schema and error behavior, see [`docs/presets.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/presets.md).
