# MCP adapter setup and security

`splinterm-mcp` is a local, bounded MCP stdio server supporting `2025-06-18`
and `2025-11-25` (preferred). It is an
optional split package and a separately authorized third-party client, not a
trusted part of `splinterd`. Installing it, or adding it to an agent host,
grants nothing: the daemon denies every operation until the person grants
access in Splinterm's consent prompt or writes an exact executable path and
digest policy.

## Set up an agent host

One command adds the installed adapter to an agent host:

```bash
splinterm mcp setup claude-code    # or codex, or vscode
splinterm mcp setup codex --dry-run
```

It adds one MCP server, named `splinterm`, that starts the adapter, and changes
nothing else. **Setup grants no access and writes no policy.** The first time
the agent needs a workspace, it asks with `request_lair_access`, and you answer
in Splinterm's prompt (see [Easy agent access](#easy-agent-access)). Start a new
session of the host to load the server.

| Host | File | Entry | Same as |
| --- | --- | --- | --- |
| `claude-code` | `~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json` when that is set (user scope) | `mcpServers.splinterm`: `{"type": "stdio", "command": ADAPTER, "args": [], "env": {}}` | `claude mcp add --scope user splinterm -- ADAPTER` |
| `codex` | `~/.codex/config.toml`, or `$CODEX_HOME/config.toml` when that is set | `[mcp_servers.splinterm]` with `command = ADAPTER` and `env_vars = ["SPLINTERM_SPLINT_ID", "SPLINTERM_SPLINT_INCARNATION"]` | `codex mcp add splinterm -- ADAPTER`, plus the variables Codex must forward for [agent status](#reporting-agent-status) |
| `vscode` | `Code/User/mcp.json` or `Code - OSS/User/mcp.json` under `~/.config` (or `$XDG_CONFIG_HOME`), whichever exists | `servers.splinterm`: `{"type": "stdio", "command": ADAPTER, "args": []}` | The `.vscode/mcp.json` form below, in the default profile |

The Claude Code and Codex entries are byte-for-byte what Claude Code 2.1.308
and codex-cli 0.144.5 write, checked in a temporary home. **The VS Code entry is
not yet verified against a real install:** it follows the documented form
below, and no installed VS Code was available to check it. With neither or
both VS Code directories present, setup asks for `--config PATH`.

Claude Code reads a legacy `.config.json` instead of `.claude.json` when that
file is non-empty: `~/.claude/.config.json`, or `$CLAUDE_CONFIG_DIR/.config.json`.
When it exists, setup names it, writes nothing, and asks you to pass it with
`--config`.

**Which adapter.** `ADAPTER` is the `splinterm-mcp` installed next to the
`splinterm` you run: `/usr/bin/splinterm-mcp` for the package. When that file
is in the Nix store, whose paths change with every upgrade, setup writes the
first ADR 0014 Nix profile entry that resolves to the same file, such as
`~/.nix-profile/bin/splinterm-mcp`; the account's home and user name come from
the password database, as the daemon's do. `--adapter PATH` starts another
adapter. Without an installed adapter, setup stops with `not_found` and
changes nothing.

Setup also says how ADR 0014 will identify that adapter. A protected install
(root-owned and unwritable along its whole path, like the package) or a Nix
profile entry can be given "Always for this workspace". For anything else it
prints one note: a development build or a user-writable install such as
`~/.cargo/bin/splinterm-mcp` is offered only the two shorter durations, and a
raw Nix store path (from `nix run`, for example) is never offered "Always" and
stops working once an upgrade or garbage collection removes it.

**What setup changes, and what it refuses.**

- `--dry-run` prints the exact change as a diff of only the changed lines,
  without context lines, and writes nothing. When a neighbouring entry gains a
  separating comma, that entry's whole last line is shown, as it is in the
  file. A dry run refuses everything the real run would refuse, including a
  missing directory that setup does not create.
- Running it again changes nothing, and the file is not rewritten.
- It edits the file in place. Other servers, other settings, their order and
  spacing, and comments in VS Code's file stay as they are.
- A different existing `splinterm` entry is left alone, and setup stops with
  `confirmation_required`. Review it; `--replace` overwrites it. One exception:
  a Codex entry that `codex mcp add` wrote for the same adapter (the same
  command, no arguments, nothing else) is brought up to date with `env_vars`
  without `--replace`, reported as `updated`.
- It writes nothing to a file it cannot edit safely:
  - malformed JSON or TOML, or duplicate keys at any depth;
  - a server list that is not an object or table;
  - a Codex entry written as an inline table or with dotted keys;
  - a symbolic link or a file with other hard links;
  - a file owned by another user, or over 64 MiB;
  - a file or directory writable by its group or by others, or a directory
    owned by another user.
- A user-private group setup (umask `002`) makes files `0664` and directories
  `0775`, which setup refuses. The refusal names the exact command to run,
  such as `chmod g-w ~/.codex/config.toml`. A shared sticky directory such as
  `/tmp` is refused outright.
- The file is checked again on the opened descriptor before it is read.
- Writes are atomic. The new text goes to a new file in the same directory and
  is synced. It is renamed over the old file only if that has not changed since
  it was read; then the directory is synced. If only that last sync fails, the
  change is made and setup warns that its durability is not confirmed. A
  missing `~/.codex` is created with mode `0700`.
- The rewritten file keeps its mode (`0600` for a new file), but not its group,
  ACLs, or extended attributes.
- `--config PATH` edits another file in the host's format, such as a trusted
  project's `.vscode/mcp.json` or a VS Code profile's `mcp.json`.

`splinterm mcp remove HOST` removes only the `splinterm` entry, with the same
checks and `--dry-run`. If that entry starts anything other than a
`splinterm-mcp`, which setup never writes, remove stops with
`confirmation_required`; review it, then `--force` removes it. Removing revokes
nothing: `splinterm access list` shows the adapter's grants, and
`splinterm access revoke` ends them.

Claude Code rewrites `~/.claude.json` while it runs, so a session that is open
during setup can write its older copy back. Run setup with no Claude Code
session open, or check afterwards with `claude mcp get splinterm`.

**Machine output.** `--output json` writes one `splinterm.cli.v2` document
(`dist/schemas/v2/cli-envelope.schema.json`) with operation `mcp_setup` or
`mcp_remove`. Its data has:

- `host`, `config_path`, `config_format` (`json`, `jsonc`, or `toml`), and
  `server_name`;
- `action`: `created`, `added`, `replaced`, `updated`, or `unchanged` for
  setup; `removed` or `absent` for remove;
- `changed`, `dry_run`, and `written`;
- for setup only, `adapter_path`, `adapter_identity` (`protected_install`,
  `nix_profile`, `nix_store`, or `unprotected`), `adapter_durable` (whether
  "Always" can be offered), and the `entry` it wrote.

`grants_access` is always `false`. A removal never echoes the entry it
removed, which may hold secrets. Failures are:

- `not_found`: no adapter, no VS Code directory, or a missing directory; exit 5.
- `invalid_argument`: a refused file, directory, or path; exit 5.
- `confirmation_required`: a different entry without `--replace`, or a foreign
  entry without `--force`; exit 3.
- `internal`: exit 70, retryable when the file changed during the edit.

Warnings go to standard error, never into the document. `splinterm mcp` never
contacts the daemon and refuses `--remote` and `--timeout-ms`.

## Install identity and launch

The split Arch package installs only `/usr/bin/splinterm-mcp` plus this guide
and license notices. Review its immutable identity before writing policy:

```bash
readlink -f /usr/bin/splinterm-mcp
sha256sum /usr/bin/splinterm-mcp
```

Use the resulting lowercase digest in the policy examples below. The canonical
package path is `/usr/bin/splinterm-mcp`; authorizing a build-tree path, basename,
MCP client name, Unix UID, or another `splinterm` executable does not authorize
it. The host needs only the daemon-discovery environment used by other local
clients. This is the entry `splinterm mcp setup claude-code` writes:

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

### Manual setup

Use these when you prefer the host's own command or edit its file by hand.

#### Claude Code 2.1.218

The installed Claude Code CLI accepts a local stdio server directly:

```bash
claude mcp add --scope user splinterm -- /usr/bin/splinterm-mcp
claude mcp get splinterm
```

Use `--scope project` only in a trusted project; Claude Code asks approval for
project-local MCP configuration. Remove it with `claude mcp remove --scope user
splinterm`.

#### Codex CLI 0.144.5

```bash
codex mcp add splinterm -- /usr/bin/splinterm-mcp
codex mcp list
```

Then add the `env_vars` line from [Reporting agent status](#reporting-agent-status)
to the `[mcp_servers.splinterm]` table it writes in `~/.codex/config.toml`.
Remove it with `codex mcp remove splinterm`.

#### Visual Studio Code 1.125.0

The installed VS Code CLI accepts a stdio definition:

```bash
code --add-mcp '{"name":"splinterm","command":"/usr/bin/splinterm-mcp"}'
```

The equivalent workspace file is `.vscode/mcp.json`:

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

Workspace configuration is code execution: inspect it and the executable path
before enabling it. These examples were checked against the locally installed
host CLIs named above; other hosts are not implied to be supported.

If the daemon uses a nondefault isolated socket, set only `SPLINTERM_SOCKET` in
the host's `env`. `splinterm-mcp` does not read MCP roots, the host working
directory, shell configuration, or SSH material. The only inherited Splinterm
context it reads is `SPLINTERM_SPLINT_ID` and `SPLINTERM_SPLINT_INCARNATION`,
as the default target of `splinterm.report_agent_status`; they are discovery
hints, never authority. Hosts that start stdio servers with a filtered
environment must forward both variables, or the tool needs explicit
`splint_id` and `incarnation` (see [Reporting agent status](#reporting-agent-status)).

## Handshake compatibility

The adapter echoes either supported protocol version; other versions remain
rejected by its bounded profile. Client capability objects
(such as Codex's `elicitation` advertisement) are accepted but never invoked.
They grant no daemon permissions. Malformed capability values and duplicate
capability names remain rejected, and all existing transport limits apply.
The `splinterm.ping` protocol version describes the adapter's preferred version,
not the session's negotiated version.

## Least-privileged policy

Create `~/.config/splinterm/policy.json` mode `0600`, substituting the digest and
real IDs. Lair/Dojo selectors snapshot only descendants present when the
policy generation is published. New descendants remain denied until a reviewed
policy reload.

Observation-only rule:

```json
{
  "schema": "splinterm.policy.v2",
  "rules": [{
    "id": "mcp-observe",
    "executable": {"path": "/usr/bin/splinterm-mcp", "sha256": "REPLACE_WITH_SHA256"},
    "scopes": ["topology_metadata_read", "topology_subscribe", "terminal_visible_read", "terminal_subscribe", "scrollback_read", "scrollback_search"],
    "resources": [{"kind": "splint", "splint_id": "REPLACE_WITH_UUID", "incarnation": "current"}],
    "limits": {"max_returned_rows": 64, "max_results": 64, "max_returned_bytes": 1048576, "max_live_subscriptions": 2, "deadline_ms": 5000}
  }]
}
```

Terminal-control rule (observation plus exclusive input/resize; no process
creation or termination):

```json
{
  "schema": "splinterm.policy.v2",
  "rules": [{
    "id": "mcp-control",
    "executable": {"path": "/usr/bin/splinterm-mcp", "sha256": "REPLACE_WITH_SHA256"},
    "scopes": ["topology_metadata_read", "terminal_visible_read", "terminal_subscribe", "controller_acquire", "controller_transfer", "input", "resize"],
    "resources": [{"kind": "splint", "splint_id": "REPLACE_WITH_UUID", "incarnation": "current"}],
    "limits": {"max_returned_rows": 64, "max_returned_bytes": 1048576, "max_live_subscriptions": 2, "deadline_ms": 5000}
  }]
}
```

Lifecycle-management rule (no terminal observation or control):

```json
{
  "schema": "splinterm.policy.v2",
  "rules": [{
    "id": "mcp-lifecycle",
    "executable": {"path": "/usr/bin/splinterm-mcp", "sha256": "REPLACE_WITH_SHA256"},
    "scopes": ["process_spawn", "process_restore", "process_terminate", "topology_layout_mutate", "topology_name_mutate"],
    "resources": [{"kind": "daemon"}, {"kind": "lair", "lair_id": "REPLACE_WITH_UUID"}],
    "limits": {"max_spawn_count": 4, "max_returned_bytes": 1048576, "deadline_ms": 10000}
  }]
}
```

A full supported-automation profile uses all 19 policy scopes, but is not a
recommended default:

```json
{
  "schema": "splinterm.policy.v2",
  "rules": [{
    "id": "mcp-full-reviewed",
    "executable": {"path": "/usr/bin/splinterm-mcp", "sha256": "REPLACE_WITH_SHA256"},
    "scopes": ["topology_metadata_read", "topology_subscribe", "terminal_visible_read", "terminal_subscribe", "scrollback_read", "scrollback_search", "controller_acquire", "controller_transfer", "input", "resize", "process_spawn", "process_restore", "process_terminate", "topology_layout_mutate", "topology_name_mutate", "authorization_inspect", "authorization_revoke", "audit_inspect", "agent_status_report"],
    "resources": [{"kind": "daemon"}, {"kind": "lair", "lair_id": "REPLACE_WITH_UUID"}],
    "limits": {"max_returned_rows": 64, "max_results": 64, "max_returned_bytes": 1048576, "max_live_subscriptions": 4, "max_spawn_count": 4, "deadline_ms": 10000}
  }]
}
```

Validate and atomically reload:

```bash
splinterm policy validate ~/.config/splinterm/policy.json
systemctl --user reload splinterd.service
splinterm policy inspect
```

No policy, a different digest, missing scope, wrong resource/incarnation, stale
revision, false destructive confirmation, or controller owned by another client
fails closed. A read policy cannot input, resize, spawn, restore, close, kill, or
rename. MCP transport access is not authority.

## Limits, trust, and lifecycle

Policy and consent bound a cooperating agent and show what it can reach. They
are not a sandbox: a hostile program running as your account can already read
your files and run commands, and can reach your terminals without asking
Splinterm. Run agents you would trust with your account.

The fixed catalog contains 34 tools, one topology resource, and terminal/control
resource templates. Bounds include a 256 KiB input line, 1 MiB complete tool
response, four active and 32 admitted requests, 5-second default daemon deadline
(configurable 100 ms–30 s), pages up to 256, 16 resource subscriptions, eight
combined controller/transfer handles, and 256 process-owned cursors. Handles and
cursors expire with the adapter process and are not portable.

### Reading terminal text

`splinterm.read_terminal` and `splinterm.read_scrollback` return plain text by
default: `content_encoding` is `text` and `data.text` holds the rows as lines.
A line the terminal wrapped is one line, trailing spaces and trailing empty
lines are dropped, and a wide character appears once. A full screen is a few
kilobytes instead of a few hundred.

```json
"data": {
  "content_encoding": "text",
  "title": "build",
  "text": "$ cargo test\n   Compiling splinterm v0.2.0\ntest result: ok. 12 passed\n$",
  "continuation_cursor": null,
  "resync_required": false
}
```

Pass `"format": "cells"` for the earlier form: `content_encoding` is
`unicode_scalars` and `data.rows` holds one `{text, width}` object per cell,
for a client that needs column positions. A resync reply has empty `text` or
empty `rows`. Terminal resources keep the cell form.

### Interactive access to a live Lair

Persistent policy remains the unattended/headless authorization path. For an
agent assisting a person who is there to answer, `request_lair_access` instead
asks them through a trusted Splinterm prompt: the consent dialog on the daemon
computer's own display, in a Splinterm Window connected to that computer over
SSH, or in `splinterm approvals` in their own terminal on it. The prompt
identifies the exact requester process and named Lair, shows the level or every
requested scope, and grants nothing unless the person explicitly accepts it.
Give exactly one of
`level` (`look`, `look_and_type`, or `manage`), `scopes`, or `cancel_request_id`.

A "just this once" grant is held only in daemon memory for five minutes. It is
bound to the requester's UID, PID, executable device/inode, and reviewed
executable digest; it dynamically covers current and newly created descendants
of that Lair, never another Lair. The requester or trusted UI can revoke the
returned grant ID, Lair termination revokes it automatically, and replacing or
exiting the requester prevents reuse. The longer durations are described below.

Controller leases remain exclusive. After requesting `input`, `resize`, and
`controller_transfer`, call `acquire_control` with `takeover: true` to use the
approved takeover scope when the graphical client currently controls the pane.
The controller remains bound to the grant that authorized it, so revocation also
releases it. Omitting `takeover` preserves ordinary non-disruptive acquisition
behavior.

### Easy agent access

> **Status:** implemented for 0.2. The contract is fixed by
> [ADR 0014](adr/0014-easy-agent-access.md) and described for users in
> [Easy agent access](automation.md#easy-agent-access). Levels, durations,
> remembered grants, the prompt, pending requests, the access list with
> per-entry revoke (`splinterm access` and **Agent access** in the command
> palette), the Explorer marks, the waiting indicator, and one-step host setup
> ([`splinterm mcp setup`](#set-up-an-agent-host)) work as described here.

**Levels.** `request_lair_access` asks for one of three levels for a workspace
(a Lair): **Look**, **Look and type**, or **Manage**. The person answers once
and picks a duration: **Just this once**, **While this workspace is open**, or
**Always for this workspace**. A request a grant already covers is answered at
once, without a prompt; the result's `source` names the grant
(`ephemeral`, `workspace_session`, `remembered`, `persistent_policy`, or
`development`).

- Every level includes `agent_status_report`, so `splinterm.report_agent_status`
  works for any Splint in the granted workspace without a second request.
- Look and type and Manage include `control_takeover`, so `acquire_control`
  with `takeover: true` works when the person's Window controls the pane.
- A request for any other set of consent scopes is shown as Custom and is
  granted exactly as asked, never rounded up. A Custom request that includes
  `control_takeover` can be granted just this once only.
- Clipboard scopes remain invalid.

**The prompt.** The prompt appears without taking keyboard focus, and its keys
stay inert until the person clicks inside it, so nothing an agent or the person
types elsewhere can answer it. After that click it is default-deny: Enter,
Escape, and `D` deny, and only `G` or a click on the green Allow button
grants, after a
short arming delay. Earlier releases granted on Enter. A person who has not
got to the prompt yet simply leaves the request pending. The prompt shows the
host's reported MCP client name as a label only. Its `host` row names the
computer (`this computer (wintermute)`), and it counts down the time left to
answer. Once that runs out, Allow is disabled and nothing grants, and the
request ends unanswered.

**Pending requests.** A person may take longer to answer than one MCP call
lasts.

- The adapter's daemon deadline is 5 seconds by default and at most 30. The
  adapter sends a wait budget a margin below that deadline (a quarter of what
  is left, at most one second); the daemon caps it at 25 seconds.
- When the budget runs out, `request_lair_access` returns a successful result
  with `status: "pending"`, `committed: false`, the `pending_request_id`, and
  `expires_at`, instead of failing. The prompt stays open, or waits its turn
  behind another prompt, for up to two minutes.
- Calling `request_lair_access` again with the same workspace and access joins
  the pending request, never opens a second prompt, and returns the answer
  once there is one, including a deny (`consent_denied`).
- A different request for the same workspace while one is pending fails with
  the retryable `resource_limit`.
- Cancel with `{"lair_id": ..., "cancel_request_id": "<pending_request_id>"}`;
  the result has `status: "cancelled"`.
- A pending request ends without a grant if it expires, the agent cancels it,
  the adapter exits, the workspace is closed, the person uses Revoke all
  access, or the daemon upgrades.
- A prompt that expires unanswered counts like a deny. The call waiting at
  that moment, or else the agent's next call for it, fails once with
  `cancelled` ("the access prompt expired without an answer"), instead of
  opening a new prompt.
- After a deny or an expiry, a new request for that workspace fails with the
  retryable `resource_limit` for a few seconds.
- If nothing can show the prompt, the request fails with `consent_unavailable`
  at once, with the message "no Splinterm Window or approver can show the
  access prompt".

**Who shares a grant.** A "while this workspace is open" or "always" grant
belongs to the adapter executable, not to one process, connection, or MCP host.

- Every host that starts `splinterm-mcp` shares it. The prompt says so: "Any
  program that runs `/usr/bin/splinterm-mcp` shares this access."
- A restarted adapter, a fresh connection per tool call, and status reports are
  all covered without asking again.
- Splinterm never identifies the adapter by the MCP client name the host
  reports. That name is shown to the person only as a label.

**Which durations are offered.**

- "While this workspace is open" binds the adapter's exact path and digest, so
  it works for development builds too.
- "Always" is offered only when the account cannot modify the adapter's file
  itself: a protected install such as the packaged `/usr/bin/splinterm-mcp`, or
  a Nix profile entry such as `~/.nix-profile/bin/splinterm-mcp` that resolves
  into a root-owned multi-user `/nix/store`.
- That protects the file, not its environment: `LD_PRELOAD` and similar settings
  in the host's server entry still affect the adapter.
- A Nix profile link is owned by the user, so other software running as the user
  could build a different program into the store and repoint the profile at it.
- An adapter written as a script, run by `python`, `node`, or another
  interpreter, is offered only the two shorter durations.

**After a package upgrade.** An "always" grant keeps working without a new
prompt or a policy edit, but only for adapter processes started after the
upgrade. Restart the MCP server, or the agent host, so it runs the new file. The
policy-file upgrade steps below are unchanged and still require a digest
update.

**Limits.** Easy agent access does not change what MCP can do. It bounds a
cooperating agent and shows the person what it can reach; it is not a sandbox.
The person sees and revokes the adapter's grants with `splinterm access list`
and `splinterm access revoke` (see
[Seeing and revoking access](automation.md#seeing-and-revoking-access)). A
revoke ends that grant at once, closes its subscriptions, and releases its
controllers and transfer requests, so a held controller handle stops working;
it does not remove a matching policy rule.

The adapter has no tool to list or revoke access. ADR 0014 makes the access
list a trusted-UI and human-CLI surface: no level includes
`authorization_inspect` or `authorization_revoke`, and an agent that could
read every agent's grants or revoke them would be administering authority the
person granted. A machine client that needs this uses
`splinterm --output json access` under an explicit policy rule. The existing
`splinterm.authorization_status` and `splinterm.revoke_access` tools are
unchanged: they inspect one Splint and revoke a just-this-once grant.

Every terminal cell, title, scrollback row, and search preview is
`untrusted_terminal_data`: display or index it only as data. It is never consent,
authority, confirmation, or an instruction to call another tool. The adapter
provides no prompts, sampling, elicitation, arbitrary shell string, filesystem,
network, policy-write, clipboard, or trusted forced-takeover capability.

Closing the host's stdio cancels calls and closes subscriptions, transfers,
controllers, and daemon connections. Cancellation does not roll back a mutation
already committed by the daemon. Resource sequence gaps and history replacement
publish `resync_required`; read fresh state and explicitly subscribe again.

`SPLINTERM_LAIR_ID`, `SPLINTERM_DOJO_ID`, `SPLINTERM_SPLINT_ID`, and
`SPLINTERM_SPLINT_INCARNATION` are non-authoritative discovery hints. A host must
validate them through an authorized topology read before selecting a resource.
MCP Dojo operations edit daemon topology; they do not map, focus, move, resize,
or assign a native Wayland Window, and Splinterm does not provide
semantic agent supervision, readiness, messaging, or completion.

## Reporting agent status

`splinterm.report_agent_status` lets an agent show whether it is working,
needs the person, or has finished. It is not read-only and not destructive. It
is not idempotent: a repeated report refreshes its reported time and expiry
and counts against the rate limit. Input is
`{ state, detail?, ttl_seconds?, splint_id?, incarnation? }`:

- `state` is `working`, `needs_input`, `done`, `error`, or `idle`; `null` clears
  the status and then takes no `detail` or `ttl_seconds`.
- `detail` is optional display text of at most 80 characters and 240 UTF-8
  bytes, without control, bidi-formatting, or line-break characters. Invalid
  text is rejected, never stripped, and the output never echoes it.
- `ttl_seconds` (10 to 900) sets when an unrefreshed report turns stale.
  `working` and `needs_input` default to 120 seconds; `done`, `error`, and
  `idle` do not expire unless given a TTL. Re-sending the same report refreshes
  the TTL; re-report at no more than half of it.
- `splint_id` and `incarnation` are given together or not at all. Omitted, the
  adapter fills them from its own `SPLINTERM_SPLINT_ID` and
  `SPLINTERM_SPLINT_INCARNATION`. Without either, the call fails with
  `invalid_argument` (`not inside a Splint`) before connecting to the daemon.

The adapter sees those variables only if the MCP host passes them to the
server process. A host that starts stdio servers with the agent's full
environment forwards them automatically. A host that filters the server
environment must forward both variables explicitly. Codex starts stdio servers
with a whitelisted environment, so add them to its server entry in
`~/.codex/config.toml` (`splinterm mcp setup codex` writes this entry):

```toml
[mcp_servers.splinterm]
command = "/usr/bin/splinterm-mcp"
env_vars = ["SPLINTERM_SPLINT_ID", "SPLINTERM_SPLINT_INCARNATION"]
```

Without them, call the tool with explicit `splint_id` and `incarnation`.

The output resource is `{"kind": "agent_status", "splint_id", "incarnation"}`
and its data is `committed: true`, the reported `state`, the daemon status
`sequence`, and nullable `expires_at_ms`. A stale or replaced incarnation fails
as `stale_incarnation`; more than 10 reports per second for one Splint fail as
retryable `resource_limit`.

An adapter launched by an agent running inside a Splint is in that Splint's PTY
session or process ancestry, so it reports for that Splint with no policy rule
and no consent prompt. It cannot report for any other Splint that way. An agent
that fully detaches (calls `setsid` and is reparented to init or
`systemd --user`, for example inside a tmux server started from the Splint), or
an adapter reporting for another Splint, needs an `agent_status_report` rule
for the exact Splint:

```json
{
  "schema": "splinterm.policy.v2",
  "rules": [{
    "id": "mcp-agent-status",
    "executable": {"path": "/usr/bin/splinterm-mcp", "sha256": "REPLACE_WITH_SHA256"},
    "scopes": ["agent_status_report"],
    "resources": [{"kind": "splint", "splint_id": "REPLACE_WITH_UUID", "incarnation": "current"}],
    "limits": {"deadline_ms": 5000}
  }]
}
```

`inspect_splint` data and each `inspect_topology` Splint gain an optional
`agent_status` object (`state`, nullable `detail`, `stale`, `reported_age_ms`,
`sequence`) only while the live incarnation has a status. The detail is
reporter-supplied text and stays inside the existing `untrusted_terminal_data`
outputs. The topology resource and its subscription do not carry agent status
in this version.

Agents that support command hooks need no MCP at all: wire
`splinterm agent-status set ...` into the agent's hooks as shown in
[automation.md](automation.md#hook-recipes), for example Claude Code's
`UserPromptSubmit` to `working`, `Notification` to `needs_input`, and `Stop` to
`done`, or Codex's `notify` to `done`. Reported status is display data only:
Splinterm still provides no agent supervision, readiness, messaging, or result
transport.

## Upgrade, revocation, and troubleshooting

A package upgrade changes the binary digest. Stop MCP hosts, review the new
binary, update the policy digest explicitly, reload policy, then reconnect. The
package never edits or broadens policy. Removing `splinterm-mcp` removes its
binary/docs; user policy and host configuration remain user-owned. Remove a
host entry with `splinterm mcp remove HOST`.

To revoke immediately, remove/narrow the rule and reload policy, then terminate
the MCP host if desired. Reload disconnects affected daemon connections and
invalidates connection-owned state.

A live daemon upgrade ends every daemon connection the server holds, and the
server reconnects without a restart:

- Ordinary tool calls connect afresh and need nothing. A call whose connection
  ends while it runs fails with `internal`, says it was not retried, and may or
  may not have taken effect: check the state before calling again.
- Controller and transfer handles expire with their connections, since the
  daemon released their control. Using one afterwards fails with
  `controller_unavailable` ("the handle expired because its splinterd
  connection ended…"). Acquire control again with `splinterm.acquire_control`,
  under the same policy and consent. Splint, Dojo, and Lair IDs stay valid.
- Resource subscriptions resubscribe on their own for up to 30 seconds from a
  fresh snapshot. The resource reads stale meanwhile, and its public `sequence`
  keeps increasing. If resubscribing fails, it stays stale until subscribed
  again.

Troubleshooting checklist:

1. `test -x /usr/bin/splinterm-mcp` and verify `sha256sum` matches policy.
2. Confirm `SPLINTERM_SOCKET`, or `$XDG_RUNTIME_DIR/splinterd.sock`, belongs to
   the same user and the daemon is running.
3. Run `splinterm policy validate` and inspect the published generation.
4. Treat `unauthorized`, `stale_topology`, `stale_incarnation`, controller
   denial, timeout, and `resync_required` as state to reconcile—not permission
   to retry blindly or broaden policy.
5. Keep stdout reserved for MCP frames; bounded diagnostics appear on stderr.

To check a whole binary set without touching your daemon, policy, or desktop,
run the headless preflight from a source checkout:

```bash
cargo build --frozen --bin splinterd --bin splinterm --bin splinterm-mcp
python3 tools/mcp/run-demo.py preflight --staged-bin-dir target/debug
```

It starts a private, deny-all daemon with its own socket and directories,
checks binary identity, the MCP handshake and required tools, and denial, then
removes everything it created. It installs no policy and grants nothing. An
`--installed` mode exists but has not yet been validated against a real
installed package. See [`tools/mcp/README.md`](../tools/mcp/README.md).

The optional adapter is available from the AUR as `splinterm-mcp` and retains an
exact-version dependency on `splinterm`. Nix/Home Manager, non-SSH gateways,
broader editor plugins, durable terminal bodies, HTTP/OAuth transport, and
write-capable MCP defaults remain deferred.
