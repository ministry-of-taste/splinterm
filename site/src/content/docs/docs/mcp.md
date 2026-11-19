---
title: MCP adapter
description: Install the optional Splinterm MCP adapter, connect a supported host, authorize the minimum required surface, and revoke it safely.
---

`splinterm-mcp` is a local, bounded MCP `2025-11-25` stdio server. It presents a fixed catalog of 34 tools plus topology and terminal/control resources over the same daemon-owned topology used by the native client.

The adapter is an optional, separately identified third-party client—not a trusted part of `splinterd`. Installing or launching it grants nothing until an owner-controlled policy authorizes its exact executable identity, operations, resources, and limits.

:::note
The optional MCP adapter is included in the stable 0.2.0 release. The host examples below document the currently validated local environment, not broad host compatibility or a stable API promise.
:::

## 1. Verify the installed identity

The optional split Arch package installs `/usr/bin/splinterm-mcp`. Inspect the canonical path and digest before writing policy:

```bash
readlink -f /usr/bin/splinterm-mcp
sha256sum /usr/bin/splinterm-mcp
```

Use the resulting lowercase digest in policy. A basename, MCP server name, Unix UID, build-tree executable, or another Splinterm binary does not identify this adapter.

## 2. Connect an MCP host

A generic local stdio definition is:

```json
{
  "mcpServers": {
    "splinterm": {
      "type": "stdio",
      "command": "/usr/bin/splinterm-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

For a daemon using an isolated nondefault socket, set only `SPLINTERM_SOCKET` in the host environment. The adapter does not use MCP roots, the host working directory, shell configuration, SSH material, or inherited Splinterm context as authority. It reads `SPLINTERM_SPLINT_ID` and `SPLINTERM_SPLINT_INCARNATION` only as the default target of `splinterm.report_agent_status`.

### Claude Code

The locally validated Claude Code CLI accepts:

```bash
claude mcp add --scope user splinterm -- /usr/bin/splinterm-mcp
claude mcp get splinterm
```

Use project scope only in a project you trust. Remove the user entry with:

```bash
claude mcp remove --scope user splinterm
```

### Visual Studio Code

The locally validated VS Code CLI accepts:

```bash
code --add-mcp '{"name":"splinterm","command":"/usr/bin/splinterm-mcp"}'
```

Equivalent `.vscode/mcp.json`:

```json
{
  "servers": {
    "splinterm": {
      "type": "stdio",
      "command": "/usr/bin/splinterm-mcp",
      "args": []
    }
  }
}
```

Workspace MCP configuration executes code. Inspect both the configuration and executable before enabling it.

## 3. Grant the minimum policy

Create `~/.config/splinterm/policy.json` with mode `0600`. This observation-only example authorizes bounded reads and subscriptions for one current Splint incarnation:

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

This rule cannot send input, resize, spawn, restore, terminate, rename, or inspect audit records. Add only the scopes and resources required by the intended workflow. The complete 19-scope inventory and reviewed control/lifecycle examples remain in repository `docs/mcp.md`.

Lair and Dojo selectors snapshot only descendants present when that policy generation is published. New descendants remain denied until a reviewed reload.

## 4. Validate and reload

```bash
chmod 600 ~/.config/splinterm/policy.json
splinterm policy validate ~/.config/splinterm/policy.json
systemctl --user reload splinterd.service
splinterm policy inspect
```

No policy, a different digest, missing scope, wrong resource or incarnation, stale revision, missing destructive confirmation, exceeded limit, or controller owned by another client fails closed. MCP transport access is not authority.

## Fixed capabilities and limits

Policy and consent bound a cooperating agent and show what it can reach. They are not a sandbox: a hostile program running as your account can already read your files and run commands, and can reach your terminals without asking Splinterm. Run agents you would trust with your account.

The adapter exposes:

- 34 fixed tools, including `splinterm.report_agent_status`;
- one topology resource and terminal/control resource templates;
- no arbitrary shell-string tool;
- no filesystem, network, policy-write, clipboard, prompts, sampling, or elicitation capability; and
- no trusted forced controller takeover.

Requests, responses, pages, subscriptions, controllers, cursors, and deadlines are bounded. Handles and cursors expire with the adapter process and are not portable. Closing the host's stdio cancels calls and releases subscriptions, transfers, controllers, and daemon connections; it does not roll back a mutation already committed by the daemon.

:::caution
Terminal content is `untrusted_terminal_data`. An MCP host may display or index it as data, but it must never treat it as consent, authority, confirmation, executable source, or an instruction to call another tool.
:::

## Sending terminal input

`splinterm.input` sends `text` as UTF-8 bytes unchanged, then any `keys` in
order. It does not trim whitespace, remove control characters, translate LF to
CR, or wrap text in bracketed-paste markers. Input requires the same scoped
grant and exclusive controller authority either way.

Press Enter with `keys: ["enter"]`. Agent hosts cannot reliably put a carriage
return (CR, U+000D) in a tool argument: a model's line break arrives as LF, and
a raw CR can be dropped before the call reaches the adapter (#278). TUIs such
as Pi, Claude Code and Codex treat LF as "insert a newline" and only CR as
"submit", so text that ends in a line break does not submit.

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

The keys send the same bytes as the graphical client for the same key press:

| Key | Bytes |
|---|---|
| `enter` | CR (`0d`) |
| `tab` | `09` |
| `shift-tab` | `ESC [ Z` |
| `escape` | `1b` |
| `backspace` | DEL (`7f`) |
| `ctrl-a` … `ctrl-z` | `01` … `1a` (`ctrl-c` is `03`) |

A call needs `text`, `keys`, or both. `text` must not be empty, `keys` holds 1
to 64 names, and text plus keys together are capped at 64 KiB. Arrow and other
cursor keys are not offered yet, because their bytes depend on the
terminal's cursor-key mode. A `\r` inside `text` stays a backslash followed by
`r`; only a JSON-escaped CR that survives the host reaches the terminal as CR.

`accepted_bytes` counts the bytes written, text and keys together. It does not
mean the foreground program interpreted them as a key or accepted the command:
`hello` plus `enter` counts as six bytes. Keep diagnostics to lengths and
synthetic control-byte probes; do not log real terminal input or prompts.

## Report agent status

`splinterm.report_agent_status` takes `{ state, detail?, ttl_seconds?, splint_id?, incarnation? }`. `state` is `working`, `needs_input`, `done`, `error`, `idle`, or `null` to clear. Without `splint_id` and `incarnation`, the adapter uses `SPLINTERM_SPLINT_ID` and `SPLINTERM_SPLINT_INCARNATION` from its own environment, and fails with `invalid_argument` (`not inside a Splint`) when there are none. A host that starts stdio servers with a filtered environment must forward both variables. For Codex, add them to the server entry in `~/.codex/config.toml`:

```toml
[mcp_servers.splinterm]
command = "/usr/bin/splinterm-mcp"
env_vars = ["SPLINTERM_SPLINT_ID", "SPLINTERM_SPLINT_INCARNATION"]
```

An adapter launched by an agent inside a Splint reports for that Splint with no policy rule; reporting for another Splint, or from an agent that fully detached from its Splint, needs an `agent_status_report` rule for the exact Splint. The reported detail is never echoed. `inspect_splint` and `inspect_topology` gain an optional `agent_status` object while a status is set. Agents with command hooks can use `splinterm agent-status` instead; see [Bounded automation](/docs/automation/).

## Upgrade and revoke

An adapter upgrade changes its SHA-256 digest. Stop MCP hosts, inspect the new binary, update the policy deliberately, reload it, and reconnect. The package never edits or broadens user policy.

To revoke immediately:

1. remove or narrow the policy rule;
2. validate and reload policy; and
3. terminate the MCP host if desired.

Reload disconnects affected daemon sessions and invalidates their connection-owned state.

## Troubleshooting

1. Confirm `/usr/bin/splinterm-mcp` exists and its digest matches policy.
2. Confirm the daemon socket belongs to the same user and `splinterd` is running.
3. Validate policy and inspect the published generation.
4. Reconcile `unauthorized`, `stale_topology`, `stale_incarnation`, controller denial, timeout, and `resync_required`; do not retry blindly or broaden policy.
5. Keep stdout reserved for MCP frames. Bounded diagnostics are written to stderr.

Read [Bounded automation](/docs/automation/) for the shared policy, controller, audit, and untrusted-data model.
