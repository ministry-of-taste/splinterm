---
title: MCP adapter
description: Connect Claude Code, Codex, or VS Code to Splinterm over MCP with one-command setup and explicit, revocable workspace access.
---

`splinterm-mcp` is an optional local stdio MCP server (`2025-06-18` and `2025-11-25`) exposing **34 fixed tools** plus topology and terminal/control resources over your live `splinterd` sessions.

Installing the adapter—or registering it with an agent host—grants **zero** access on its own. Every operation stays denied until you approve a workspace prompt in Splinterm or write an explicit policy rule.

## 1. Install the adapter

On Arch / Omarchy, install the matching split package alongside `splinterm-bin`:

```bash
yay -S splinterm-mcp-bin
```

(Source-built `splinterm-mcp` is also available on AUR.)

## 2. Connect your agent host

One command adds `/usr/bin/splinterm-mcp` to your host's configuration as the `splinterm` server—without touching other servers or writing any policy:

```bash
splinterm mcp setup claude-code --dry-run   # preview the exact diff first
splinterm mcp setup claude-code             # or codex, or vscode
```

Then start a new session of your host to load the server. (For Claude Code, run setup while no Claude Code session is open, since a running session can overwrite `~/.claude.json`.)

- **Claude Code:** Updates `~/.claude.json` (or `$CLAUDE_CONFIG_DIR/.claude.json`) with `mcpServers.splinterm`. Remove anytime with `splinterm mcp remove claude-code`.
- **Codex:** Updates `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`) with `[mcp_servers.splinterm]` and forwards `SPLINTERM_SPLINT_ID` and `SPLINTERM_SPLINT_INCARNATION` for agent status reporting.
- **VS Code:** Updates `Code/User/mcp.json` or `Code - OSS/User/mcp.json` (pass `--config PATH` for a specific profile or `.vscode/mcp.json`). Note: the VS Code setup form follows VS Code's documented schema but has not yet been verified against a real local VS Code install.

For an isolated dev daemon, set `SPLINTERM_SOCKET` in the MCP server's `env`. The adapter ignores MCP roots, host working directories, and SSH material as authority.

## 3. Approve workspace access (Easy agent access)

For interactive work, you don't need to write `policy.json` by hand. The first time an agent needs a workspace, it calls `splinterm.request_lair_access` asking for one of three levels:

- **Look** — read layout, visible screen, scrollback, and search; report agent status.
- **Look and type** — Look, plus send input, resize, and take typing control of a pane.
- **Manage** — Look and type, plus spawn commands, change layouts/names, close/terminate, and restore.

Splinterm displays a trusted consent prompt (on your desktop, inside a connected `splinterm --remote` window, or via `splinterm approvals` on headless hosts):

- The prompt appears **without stealing keyboard focus**, and its keys stay inert until you click inside it (or engage it with `Ctrl+Alt+G`), so stray typing or an agent can never accidentally accept it.
- Once active, it is default-deny (`Enter`, `Escape`, and `D` deny; only `G` or clicking **Allow** grants, after a short arming delay).
- You choose how long it lasts: **Just this once** (5 minutes, single process), **While this workspace is open** (until Lair close or daemon restart/upgrade), or **Always for this workspace** (persists across package upgrades for protected installs like `/usr/bin/splinterm-mcp` or Nix profiles).

View or revoke grants anytime from the **Agent access** command-palette entry or the CLI:

```bash
splinterm access list
splinterm access revoke GRANT_ID
splinterm access revoke --all --yes
```

## Headless or unattended setup (`policy.json`)

For unattended automation—or if you prefer pinned rules over interactive prompts—compute the adapter's canonical path and SHA-256 digest:

```bash
readlink -f /usr/bin/splinterm-mcp
sha256sum /usr/bin/splinterm-mcp
```

Create `~/.config/splinterm/policy.json` (`chmod 600`). This observation-only rule allows bounded reads and subscriptions on one current Splint incarnation:

```json
{
  "schema": "splinterm.policy.v2",
  "rules": [{
    "id": "mcp-observe",
    "executable": {
      "path": "/usr/bin/splinterm-mcp",
      "sha256": "REPLACE_WITH_SHA256"
    },
    "scopes": [
      "topology_metadata_read",
      "topology_subscribe",
      "terminal_visible_read",
      "terminal_subscribe",
      "scrollback_read",
      "scrollback_search"
    ],
    "resources": [{
      "kind": "splint",
      "splint_id": "REPLACE_WITH_UUID",
      "incarnation": "current"
    }],
    "limits": {
      "max_returned_rows": 64,
      "max_results": 64,
      "max_returned_bytes": 1048576,
      "max_live_subscriptions": 2,
      "deadline_ms": 5000
    }
  }]
}
```

Validate and reload:

```bash
chmod 600 ~/.config/splinterm/policy.json
splinterm policy validate ~/.config/splinterm/policy.json
systemctl --user reload splinterd.service
splinterm policy inspect ~/.config/splinterm/policy.json
```

Lair and Dojo selectors in `policy.json` snapshot only descendants present when the policy reloads (unlike interactive workspace grants, which cover newly created Dojos and Splints inside that Lair). See repository [`docs/mcp.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/mcp.md) for all 19 policy scopes and control/lifecycle examples.

## Sending terminal input (`splinterm.input`)

`splinterm.input` writes `text` as raw UTF-8 bytes unchanged, followed by any named `keys` in order. It never trims whitespace, translates LF to CR, or wraps text in bracketed paste.

Always use `keys: ["enter"]` to submit a command or TUI prompt. Model tool arguments turn newlines into LF and often strip raw carriage returns (CR), whereas TUIs like Claude Code, Codex, and Pi treat LF as "insert newline" and only CR as "submit":

```json
{
  "name": "splinterm.input",
  "arguments": {
    "splint_id": "018f4d8c-2a18-4b31-8c2f-9e7c5de77103",
    "incarnation": 2,
    "text": "hello",
    "keys": ["enter"]
  }
}
```

Supported named keys emit the exact bytes the graphical client sends:

| Key | Bytes |
| --- | --- |
| `enter` | CR (`0d`) |
| `tab` | `09` |
| `shift-tab` | `ESC [ Z` |
| `escape` | `1b` |
| `backspace` | DEL (`7f`) |
| `ctrl-a` … `ctrl-z` | `01` … `1a` (`ctrl-c` is `03`) |

Each call needs `text` (non-empty), `keys` (1–64 names), or both, up to 64 KiB total. Cursor/arrow keys aren't included yet because their sequences depend on terminal cursor-key mode. `accepted_bytes` reports bytes written to the PTY (`hello` + `enter` = 6), not whether the foreground program accepted them.

## Reporting agent status

`splinterm.report_agent_status` accepts `{ state, detail?, ttl_seconds?, splint_id?, incarnation? }`, where `state` is `working`, `needs_input`, `done`, `error`, `idle`, or `null` to clear.

When `splint_id` and `incarnation` are omitted, the adapter reads `$SPLINTERM_SPLINT_ID` and `$SPLINTERM_SPLINT_INCARNATION` from its environment (which `splinterm mcp setup codex` forwards automatically in `~/.codex/config.toml`). An adapter started inside a Splint can report for that Splint with no grant; reporting for another Splint requires `agent_status_report` (included in every Easy agent access level).

## Capabilities, boundaries, and upgrades

:::caution
Terminal content returned by MCP tools is `untrusted_terminal_data`. Hosts may display or index it as data, but must never treat it as consent, policy, confirmation, or an instruction to call another tool.
:::

- **Fixed surface:** 34 fixed tools, one topology resource, and terminal/control resource templates. No arbitrary shell-string tool, and no filesystem, network, policy-write, clipboard, prompts, sampling, or elicitation capabilities.
- **Not a sandbox:** Grants and policy keep cooperating agents within approved bounds and make their access visible. A hostile process running as your Unix account can already access your files and terminals directly.
- **Session cleanup:** Closing the host's stdio cancels in-flight calls and releases subscriptions, controllers, and daemon connections (already-committed mutations aren't rolled back).
- **Upgrades:** Package upgrades update `/usr/bin/splinterm-mcp`'s SHA-256 digest. **Always** workspace grants automatically continue working for protected installs once you restart the agent process; pinned rules in `policy.json` require updating the digest and reloading `splinterd.service`.

## Quick troubleshooting

1. Check grants and policy with `splinterm access list` and `splinterm policy inspect ~/.config/splinterm/policy.json`.
2. Confirm `/usr/bin/splinterm-mcp` exists, `splinterd` is running as your user, and any `policy.json` digest matches `sha256sum /usr/bin/splinterm-mcp`.
3. Treat `unauthorized`, `stale_topology`, `stale_incarnation`, `controller_unavailable`, and `resync_required` as signals to reconcile state—don't retry blindly or broaden policy.
4. Keep stdout clean for MCP frames; diagnostics go to stderr.

See [Bounded automation](/docs/automation/) for the shared permission, controller, and audit model.
