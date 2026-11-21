---
title: Bounded automation
description: How scripts and coding agents inspect, observe, and operate Splinterm sessions with explicit permissions and clear limits.
---

Scripts, tools, and coding agents work against the exact same Lairs, Dojos, and Splints you see in your Wayland windows—without inheriting your graphical trust.

Reaching the Unix socket, running as your Linux user, logging in over SSH, or running inside a Splint never grants automation access by itself. Every operation is checked by `splinterd` against either an **interactive workspace grant** (Easy agent access) or an explicit **policy file** (`policy.json`). The one narrow exception is `focus`, which exposes only the nullable active Splint ID and working directory to authenticated local callers—never topology or terminal text.

```text
JSON/NDJSON CLI · MCP host · SSH stdio relay
                     ↓
        workspace grant or policy.json
                     ↓
     binary identity · scopes · limits
                     ↓
                 splinterd
                     ↓
        topology · terminal · control
```

## Easy agent access (interactive)

For coding agents working alongside you on a live workspace, you don't need to hand-edit SHA-256 policy files. Register the MCP adapter (`splinterm mcp setup claude-code|codex|vscode`); the first time an agent needs a Lair, it asks via `request_lair_access` and Splinterm opens a trusted consent prompt.

### Access levels

Every grant covers one workspace (a Lair)—including Dojos and Splints created inside it later—and nothing outside it:

| Level | What it allows |
| --- | --- |
| **Look** | Read workspace layout, visible screen snapshots, scrollback, and search; report agent status. |
| **Look and type** | Everything in **Look**, plus send input, resize panes, and take typing control of a pane. |
| **Manage** | Everything in **Look and type**, plus spawn commands, change layouts/names, close/terminate, and restore. |

No level grants clipboard access, authorization inspection/revocation, audit inspection, or creating brand-new Lairs. (If a pane is controlled by your window and `clipboard.osc52=copy-paste` is enabled, programs inside that pane can still read the clipboard via OSC 52; set `[clipboard] osc52=copy` or `off` to block reads.)

### How long access lasts

- **Just this once:** Five minutes, tied to that single agent process.
- **While this workspace is open:** Survives agent reconnects and window closes, bound to that binary's path and SHA-256 digest; ends when the Lair is closed or `splinterd` stops/upgrades.
- **Always for this workspace:** Survives daemon and package upgrades until you revoke it or remove the Lair. Offered only for protected installs (such as `/usr/bin/splinterm-mcp`) or Nix profile entries—never for writable dev builds.

Inspect or revoke grants anytime from the **Agent access** palette entry or the CLI:

```bash
splinterm access list
splinterm access revoke GRANT_ID
splinterm access revoke --all --yes
```

:::note[Not a security sandbox]
Consent grants and policy rules keep cooperating agents within what you approved and show you what each tool can reach. They are not a sandbox: a hostile program running as your Linux account can already read your files, run commands, and reach your terminals without asking Splinterm. Run agents you trust with your account.
:::

## Three ways to use the CLI machine surface

### 1. Inspect (JSON)

One-shot `--output json` commands return a versioned `splinterm.cli.v2` envelope for topology, Splint details, visible screen snapshots, scrollback pages, literal search, authorization state, and body-free audit records:

```bash
splinterm --output json --schema-major 2 --timeout-ms 5000 topology
splinterm --output json --schema-major 2 --timeout-ms 5000 \
  snapshot SPLINT_ID
```

### 2. Operate (JSON)

Authorized clients can create Lairs/Dojos, split panes, launch commands directly as argv, rename items, adjust split ratios, restore exited processes, send literal input, resize, and terminate resources:

```bash
splinterm --output json --schema-major 2 --timeout-ms 10000 \
  split SPLINT_ID --axis horizontal --side second \
  --cwd "$PWD" -- cargo test --workspace
```

Machine mode never prompts interactively. Destructive commands (`kill`, `close`, `close-dojo`, `access revoke`, etc.) require `--yes` in addition to permission.

### 3. Observe (NDJSON streams)

Subscribe to live topology, terminal, or controller changes:

```bash
splinterm --output ndjson --schema-major 2 --timeout-ms 300000 \
  subscribe terminal SPLINT_ID
```

If a subscriber falls behind, misses a sequence number, or sees replaced history, `splinterd` emits `resync_required` and closes the stream. Fetch fresh state and resubscribe rather than guessing what was missed.

## Reporting agent status

Agents can report explicit status (`working`, `needs_input`, `done`, `error`, or `idle`) so you can see at a glance which tab or pane needs your attention. Splinterm never guesses status from terminal text or titles.

```bash
splinterm agent-status set needs_input --detail 'waiting for approval'
splinterm agent-status clear
splinterm --output json agent-status get
```

Inside a Splint, the target defaults to `$SPLINTERM_SPLINT_ID` and `$SPLINTERM_SPLINT_INCARNATION`. Any process in that Splint's session or process tree (such as an agent hook) can report status for that Splint with no grant or prompt. A detached process (for example, inside a tmux server) needs an `agent_status_report` grant.

Active statuses turn stale after 120 seconds without a fresh report (adjustable via `--ttl`). You can wire this up with simple shell hooks—no MCP required. For example, in Claude Code:

```json
{
  "hooks": {
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "splinterm agent-status set working --ttl 900 >/dev/null 2>&1 || true"}]}],
    "PostToolUse": [{"hooks": [{"type": "command", "command": "splinterm agent-status set working --ttl 900 >/dev/null 2>&1 || true"}]}],
    "Notification": [{"matcher": "permission_prompt", "hooks": [{"type": "command", "command": "splinterm agent-status set needs_input --ttl 900 >/dev/null 2>&1 || true"}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "splinterm agent-status set done >/dev/null 2>&1 || true"}]}]
  }
}
```

The `permission_prompt` matcher prevents Claude Code's idle notification from overwriting `done` with `needs_input`, and `PostToolUse` flips back to `working` once approved. Codex can map its `notify` command to `done`.

### How status appears in the UI

- **Icons & motion:** Fresh `working` status draws a Braille spinner (`⠋⠙⠸⢰⣠⣄⡆⠇`, stepping at most 4×/sec only while the window has focus; held still at `⠋` when unfocused, stale, or with `SPLINTERM_REDUCED_MOTION=1`). Other states use static built-in [Tabler](https://tabler.io/icons) icons (`message-question` for needs input, `circle-check` for done, `alert-circle` for error, `point` for idle; or compact shapes with `explorer.status-glyphs=dots`).
- **Explorer & tabs:** Splint rows show their icon and spell out the selected row's state and detail in the footer. Dojos, Lairs, and Dojo tabs surface the most urgent child state (`needs_input` first), and stale statuses gain a clock badge. Explorer search matches state words like `needs input`.
- **Clearing or hiding:** Use **Clear agent status** in the command palette, or set `agent-status=no` under `[explorer]` in `config.ini` to hide status UI completely.

## Persistent policy (`policy.json`)

For unattended scripts, headless servers, or tightly pinned setups, `~/.config/splinterm/policy.json` (mode `0600`) defines fail-closed rules binding:

- the canonical executable path and SHA-256 digest;
- one or more of the 19 explicit operation scopes (no wildcards);
- specific daemon, Lair, Dojo, or Splint resources (no implicit access to future resources);
- row, byte, result, subscription, spawn, and deadline limits; and
- an optional expiration timestamp.

A broken policy file installs a deny-all generation rather than partially loading.

## Controller ownership

Multiple clients can watch a Splint at once, but only **one** connection can hold input and resize control at a time.

- Getting `controller_unavailable` is a normal exclusive-ownership signal—not a reason to force takeover blindly.
- One-shot CLI `send` and `resize` commands atomically acquire, act, and release control on a single connection.
- Closing a connection or revoking access immediately releases its controller lease. If you resize a pane in your window, your window reclaims control automatically.

## Terminal output is always data

:::caution
Every terminal cell, title, scrollback line, and search match is untrusted data (`untrusted_terminal_data`). Tools and agents must never treat terminal output as consent, policy, confirmation, or an instruction to invoke another action.
:::

Audit records log only metadata—never terminal output, typed input bytes, clipboard contents, search queries, environment values, tokens, or full command arguments.

## Compatibility boundary

The stable machine contract covers `--output json` / `--output ndjson`, checked-in schemas (`dist/schemas/v2/`), documented operations, exit codes, limits, cancellation, and resync rules. Human CLI output and internal daemon frames are not public APIs. Clients should reject unknown schema majors.

For the full schema, exit codes, and integration checklist, see repository [`docs/automation.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/automation.md) and [`docs/integrations.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/integrations.md). Ready to wire up an agent? Head to [MCP adapter](/docs/mcp/).
