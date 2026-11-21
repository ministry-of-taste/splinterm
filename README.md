<div align="center">
  <img src="assets/icons/splinterm-icon-glyph.svg" width="112" height="112" alt="Splinterm glyph">

# Splinterm

**A native Wayland terminal where closing the window never kills your shells.**

[Website](https://splinterm.com/) · [Documentation](https://splinterm.com/docs/) · [Quickstart](https://splinterm.com/docs/quickstart/) · [Release notes](RELEASE_NOTES.md) · [Roadmap](docs/product-roadmap.md) · [Status](docs/status.md)

</div>

Splinterm pairs a lightweight Wayland terminal with a background daemon (`splinterd`) that owns your shells, pane layouts, and scrollback. Close a window whenever you want, come back later, and your work is still running.

You get built-in tabs, split panes, presets, and SSH-backed remote windows without wrapping `tmux` around every terminal. When you want scripts or coding agents to work alongside you, they connect to the same live sessions through a scoped JSON/NDJSON CLI, SSH relay, or optional MCP server—with explicit permissions you can inspect and revoke at any time.

Splinterm is written in Rust, grounded in [Foot](https://codeberg.org/dnkl/foot)'s terminal emulation, and built first for **Omarchy and Arch Linux** (`x86_64`, native Wayland under Hyprland).

> [!NOTE]
> **Current release: 0.2.0** for x86_64 Omarchy and Arch Linux. Live daemon upgrades work from 0.2.0 onward (upgrading from 0.1.x to 0.2.0 is a one-time restart that ends existing shells). For exact platform limits, validation status, and NixOS notes, see [Current status](docs/status.md).

## Why Splinterm

- **Your shells outlive their window.** `splinterd` owns your terminal processes, layouts, and scrollback. A Wayland window is just a disposable view—closing it detaches, it doesn't kill your work.
- **Native Wayland, Foot-class footprint.** Direct Wayland input, scaling, clipboard, IME, and damage-driven rendering built from Foot's battle-tested terminal kernel ([`THIRD_PARTY.md`](THIRD_PARTY.md)), without stacking a nested multiplexer on top.
- **One workspace for people and tools.** Native windows, the CLI, remote SSH windows, and the optional MCP adapter all share the same persistent sessions.
- **Explicit permissions, never silent trust.** Scripts and agents don't inherit graphical access. You choose what they can read or control—via workspace consent or explicit policy—and terminal output is always treated as untrusted data.

## Install

On **x86_64 Omarchy / Arch Linux**, install the prebuilt AUR package with `yay` or `paru`:

```bash
yay -S splinterm-bin
# Optional MCP adapter:
yay -S splinterm-mcp-bin
```

Source-built `splinterm` and `splinterm-mcp` packages are also on AUR. Because the AUR channel can also carry release candidates between stable releases, check the version your helper offers before confirming—or install an exact release tag with [docs/packaging.md](docs/packaging.md).

### Recommended on Omarchy

Run this once so Hyprland tags Splinterm as a terminal (fixing `Super+C`/`Super+V` copy/paste and terminal transparency rules) and wires up default-terminal and screensaver integration:

```bash
splinterm integration omarchy enable
```

Installation itself never edits your Omarchy or Hyprland config, changes your default terminal, or installs the MCP adapter behind your back. `splinterm integration omarchy disable` reverses everything cleanly.

### Other install methods

- **Release installer (from a terminal not owned by `splinterd`):**
  ```bash
  git clone https://github.com/ministry-of-taste/splinterm.git
  cd splinterm
  ./install.sh          # newest published release (may include prereleases)
  ./install.sh --source # build and package the current committed HEAD
  ```
- **NixOS:** Includes a source-built flake and module for desktop and headless setups; see [docs/nixos.md](docs/nixos.md) (Arch graphical validation does not automatically extend to NixOS).

For upgrade details, recovery snapshots, and exact-tag installs, see the [installation guide](https://splinterm.com/docs/install/) and [`docs/packaging.md`](docs/packaging.md).

## Quickstart

Open a terminal from your app launcher or run:

```bash
splinterm-xdg-terminal-exec
```

Start something recognizable (like `htop` or a dev server), then close the window. Your shell keeps running in `splinterd`. Bring it back anytime with the native picker:

```bash
splinterm dojos   # pick a running Dojo or start a new terminal
splinterm reopen  # jump straight back to the last running Dojo
```

(`splinterm sessions` works as an alias for `splinterm dojos`.)

### Essential shortcuts

| Action | Shortcut |
| --- | --- |
| Command palette | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>P</kbd> |
| Recent Dojos | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> |
| Split horizontally | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Enter</kbd> |
| Split vertically | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>\</kbd> |
| Move between panes | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Arrow</kbd> |
| Cycle Dojo tabs | <kbd>Ctrl</kbd>+<kbd>Tab</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd> |
| New Dojo tab | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>D</kbd> |
| Detach active tab | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Q</kbd> |
| Search scrollback | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> |
| Copy / paste | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> |

If you prefer tmux-style chords, set `profile=omarchy-tmux` under `[key-bindings]` in `config.ini` for `Ctrl+Space` / `Ctrl+B` prefixes, searchable key help (`Prefix+?`), vi copy mode (`Prefix+[`), and current-Lair save/restore bindings. Bundled **Dojo presets** and optional `s*` Bash helpers also mirror Omarchy's `t`, `tdl`, `tds`, `tdlm`, and `tsl` workflows without colliding with your existing aliases.

> [!TIP]
> **Reopening vs. restoring:** Reopening attaches to a shell that is still running. If a process has exited, starting it again from saved launch metadata is always an explicit **restore**—Splinterm never re-runs saved commands automatically.

For window-owned transient terminals (`persistent-by-default=no`), tab controls, and full workflows, see the [quickstart](https://splinterm.com/docs/quickstart/), [sessions guide](https://splinterm.com/docs/sessions/), or [docs/usage.md](docs/usage.md).

## How it works

```text
                         native windows
                    ┌──────────┴──────────┐
                    │ disposable views    │
                    └──────────┬──────────┘
                               │ attach / detach
                         ┌─────▼─────┐
                         │ splinterd │
                         └─────┬─────┘
                               │ owns
Topology
└── Lair: project or workspace
    ├── Dojo: split pane layout
    │   ├── Splint: running shell or command
    │   └── Splint: running shell or command
    └── Dojo: another layout
        └── Splint: running shell or command
```

- **Lair** — a named project or workspace grouping related Dojos.
- **Dojo** — a persistent split-pane layout inside a Lair.
- **Splint** — an individual terminal pane and its process lifecycle.
- **Topology** — the full catalog of Lairs, Dojos, and Splints owned by `splinterd`.
- **Window** — a native Wayland view that displays one or more Dojos as local tabs.

Closing a window or tab detaches your view; terminating a running Splint or Dojo is always a separate, confirmed action. Read [Core concepts](https://splinterm.com/docs/concepts/) or [`docs/architecture.md`](docs/architecture.md) for the deeper design.

## Automation, remote access, and configuration

- **Automation & MCP:** Scripts and agents can inspect sessions, stream events, report live agent status, or send input via the versioned JSON/NDJSON CLI, SSH stdio relay, or optional MCP adapter. Machine clients never inherit human graphical trust; access is controlled through explicit workspace consent or policy rules. See [`docs/automation.md`](docs/automation.md), [`docs/mcp.md`](docs/mcp.md), [`docs/integrations.md`](docs/integrations.md), and [`dist/schemas/v2/`](dist/schemas/v2/).
- **Remote sessions:** Open native remote Wayland windows over your own OpenSSH setup without running a daemon network listener—no automation policy required for human SSH sessions. See [`docs/remote.md`](docs/remote.md).
- **Configuration & Omarchy themes:** Configured in `${XDG_CONFIG_HOME:-~/.config}/splinterm/config.ini`. On Omarchy, Splinterm automatically tracks your active theme (`foot.ini`, `colors.toml`, `shell.toml`) and live monospace font changes without restarting windows or shells. See the [configuration guide](https://splinterm.com/docs/configure/configuration/).

## Documentation

| Looking for… | Start here |
| --- | --- |
| Installing or upgrading | [Installation](https://splinterm.com/docs/install/) |
| First two minutes with Splinterm | [Quickstart](https://splinterm.com/docs/quickstart/) |
| Lairs, Dojos, Splints, and windows | [Core concepts](https://splinterm.com/docs/concepts/) |
| Persistence, restore, and reset | [Sessions and persistence](https://splinterm.com/docs/sessions/) |
| Fonts, themes, keymaps, and Foot migration | [Configuration](https://splinterm.com/docs/configure/configuration/) |
| What works today and platform limits | [Current status](docs/status.md) |
| Full daily-use reference | [Human usage guide](docs/usage.md) |
| CLI subcommands and JSON output | [CLI reference](docs/cli.md) |
| Fixing local issues | [Troubleshooting](https://splinterm.com/docs/troubleshooting/) |
| Contributing to the codebase | [Contributing](CONTRIBUTING.md) |

## Development

Splinterm is a Rust workspace. Standard checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

To hack on Splinterm without touching your live desktop daemon, use the isolated test wrapper:

```bash
./splinterm-test          # build, start or reuse the isolated daemon, and open a window
./splinterm-test restart  # rebuild and restart after daemon or protocol changes
./splinterm-test ping     # verify the isolated daemon
./splinterm-test stop     # stop it
```

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) and the [development guide](https://splinterm.com/docs/development/) before modifying domain, protocol, renderer, or Foot-derived code.

## Lineage & license

Splinterm's terminal emulator kernel is ported from [Foot](https://codeberg.org/dnkl/foot), preserving source provenance and MIT attribution in [`docs/adr/0001-foot-rust-port.md`](docs/adr/0001-foot-rust-port.md) and [`THIRD_PARTY.md`](THIRD_PARTY.md).

If Splinterm is useful to you, you can [support development on Ko-fi](https://ko-fi.com/oldjobobo). Released under the [MIT License](LICENSE).
