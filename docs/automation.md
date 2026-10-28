# Supported automation contracts

> **Phase 4 status:** the local JSON/NDJSON CLI, authorization, persistent policy
> loading, daemon-lifetime audit inspection, dedicated SSH stdio relay, and
> public-CLI reference integration are implemented. Human rendering and raw protocol DTOs remain private interfaces
> and are not compatibility promises.

Splinterm automation uses the owner-only local daemon socket. Remote automation
uses the same daemon operations through `splinterm relay --stdio` over
authenticated SSH; `splinterd` does not expose a network listener. See
[remote.md](remote.md) for the exact executable policy and transport lifecycle.
Transport access, an SSH
login, or the same Unix UID does not grant terminal authority.

Native `splinterm --remote PROFILE` is a human graphical workflow, not a new
machine compatibility surface. It uses the separate bounded
`relay --graphical-stdio` multiplexer and automation-role daemon channels, but
its picker, tabs, renderer, input, and endpoint-scoped lifecycle orchestration
remain private native-client behavior. JSON/NDJSON automation continues to use
the raw single-channel relay and is not implicitly redirected by graphical
remote profiles.

The security and operation-to-scope contract is fixed by
[ADR 0007](adr/0007-supported-automation-policy.md). Terminal and scrollback
content is always untrusted data. It cannot approve consent, change policy, or
be interpreted as an instruction by a client.

## Public and private boundaries

The public compatibility boundary consists of:

- JSON documents emitted by `--output json`;
- NDJSON records emitted by `--output ndjson`;
- the checked-in Draft 2020-12 schemas under `dist/schemas/v2/`; and
- documented command behavior, exit codes, limits, and cancellation semantics.

Raw daemon frames, protocol versions, Rust enum layouts, and serialized runtime
structs remain private. Public DTOs are explicit conversions from those
internal types. The schemas are handwritten so review can reject accidental
exposure of internal fields; implementation tests must prove emitted DTOs match
the checked-in artifacts.

Every machine record names its schema and major version. Fields may be added
within a major version. Removing a field, changing its type, or reinterpreting
its meaning requires a new major version. A client must reject an unknown major
version rather than guessing.

## Output modes

Human-readable output remains the default. Machine modes reserve stdout:

- `--output json` emits exactly one `splinterm.cli.v2` document and is the only
  machine mode accepted for one-shot commands;
- `--output ndjson` is reserved for subscriptions, where it emits one bounded
  `splinterm.cli.event.v2` record per line; and
- diagnostics and logs go to stderr and never include terminal or input bodies.

One-shot NDJSON and JSON subscriptions are rejected rather than silently changing
record shape. Machine output, schema, and timeout flags are also rejected for the
graphical `window` and `launch` commands and for local policy, relay, or service
administration. This includes the destructive `reset` command, which is guarded
by interactive confirmation or an explicit `--yes` and intentionally has no
machine-output contract. Those commands remain human-only; automation uses the
non-Wayland lifecycle commands instead.

A daemon `Dojo` is a persistent topology resource. `new-dojo`, `close-dojo`,
`rename-dojo`, and `dojo-focus-hint` do not map, focus, move, resize, or close a
native Wayland Window. Graphical `splinterm window` clients are separate
disposable processes, and the default-focus field is only a persisted
presentation hint. Compositor orchestration is outside the v2 automation
contract.

Request and subscription IDs are client-owned nonzero decimal strings, independent
of private daemon protocol IDs. This avoids precision loss in JSON consumers and
allows a one-shot invocation to use request ID `"1"` even when schema selection,
configuration, socket connection, or handshake fails after successful argument
parsing. Syntactically invalid command lines produce no stdout record, write usage
and diagnostics to stderr, and exit with status 2. An unsupported requested major
may be reported using the highest schema major the executable can safely emit.

UUID resource IDs remain strings. Incarnations and history generations are
positive JSON integers; topology and terminal revisions are nonnegative because
their initial state is revision 0. Every terminal read carries exact Splint, incarnation, and revision
provenance plus explicit truncation state. Terminal snapshots contain semantic
cells, not raw bytes: `content_encoding` is `unicode_scalars`, each cell carries a
Unicode `text` string and a display `width`, and ambiguous byte insertion is not
permitted.

Successful one-shot envelopes contain `data` and cannot contain `error`. Failed
envelopes contain a stable symbolic error, bounded message, and retryability,
cannot contain `data`, and always set `truncated: false` because errors have no
continuation mechanism. The optional `operation` discriminator preserves the
operation-less draft v2 fixtures byte-for-byte. Production output always includes
it; whenever it is present, the schema applies a closed operation-specific
`data`, `resource`, and error payload. Both successful and failed `ping` and
`focus` records, and `audit_inspect` records, omit `resource`; failed `ping` and
`focus` records also omit
resource-revision hints because those operations expose no resource data.

### One-shot command and operation inventory

The implemented v2 one-shot names are frozen as follows. Human-only local
administration commands such as `policy`, `relay`, and `reset` are deliberately
outside this inventory. `mcp setup` and `mcp remove` are local file edits that
never contact the daemon; see [Set up an agent host](mcp.md#set-up-an-agent-host).

| Command | `operation` |
| --- | --- |
| `ping` | `ping` |
| `focus` | `focus` |
| `list` | `list_lairs` |
| `topology` | `inspect_topology` |
| `inspect SPLINT_ID` | `inspect_splint` |
| `snapshot SPLINT_ID` | `terminal_snapshot` |
| `scrollback SPLINT_ID` | `scrollback_page` |
| `search SPLINT_ID QUERY` | `search_scrollback` |
| `authorization status SPLINT_ID` | `authorization_status` |
| `authorization revoke GRANT_ID --yes` | `revoke_access` |
| `access list [--after ID] [--max-entries N]` | `access_list` |
| `access revoke ID --yes` | `access_revoke` |
| `access revoke --all --yes` or `--all --dry-run` | `access_revoke_all` |
| `audit` | `audit_inspect` |
| `new NAME [-- ARGV...]` | `create_lair` |
| `split TARGET_SPLINT_ID ... [-- ARGV...]` | `split_splint` |
| `close SPLINT_ID --yes` | `close_splint` |
| `ratio TARGET_SPLINT_ID RATIO` | `set_split_ratio` |
| `new-dojo LAIR_ID ... [-- ARGV...]` | `new_dojo` |
| `close-dojo DOJO_ID --yes` | `close_dojo` |
| `rename-lair LAIR_ID NAME` | `rename_lair` |
| `rename-dojo DOJO_ID NAME` | `rename_dojo` |
| `rename-splint SPLINT_ID TITLE` | `rename_splint` |
| `dojo-focus-hint DOJO_ID SPLINT_ID` | `set_dojo_default_focus` |
| `relaunch SPLINT_ID [-- ARGV...]` | `relaunch_splint` |
| `restore SPLINT_ID` | `restore_splint` |
| `restore-dojo DOJO_ID` | `restore_dojo` |
| `restore-lair LAIR_ID` | `restore_lair` |
| `send SPLINT_ID TEXT` | `input` |
| `resize SPLINT_ID COLUMNS ROWS` | `resize` |
| `kill SPLINT_ID --yes` | `kill_splint` |
| `agent-status set STATE [...]` | `agent_status_set` |
| `agent-status clear [...]` | `agent_status_clear` |
| `agent-status get [SPLINT_ID]` | `agent_status_get` |
| `mcp setup HOST [--dry-run] [--replace]` | `mcp_setup` |
| `mcp remove HOST [--dry-run] [--force]` | `mcp_remove` |

The NDJSON subscription commands are:

- `subscribe terminal SPLINT_ID --output ndjson`;
- `subscribe topology --output ndjson`; and
- `subscribe control SPLINT_ID --output ndjson`.

JSON is rejected for subscriptions, and NDJSON is rejected for one-shot commands.
The `focus` command is machine-only and must be invoked as
`splinterm --output json focus`.

### Focus adapter projection

`focus` is the supported narrow adapter for integrations that need to associate
an external tool with the active Splinterm pane. A successful response contains
exactly nullable `data.splint_id` and nullable `data.cwd`. No keyboard-focused
Splinterm window returns both fields as `null`. A live focus whose working
directory cannot be read safely returns the stable Splint ID and a null `cwd`.
The daemon derives `cwd` from the live shell process, accepts only absolute UTF-8
paths within the protocol bound, and suppresses deleted-path markers.

Any authenticated local client may read this projection without topology or
terminal-content authority. Only the installed trusted graphical client may
publish focus. Publication is ephemeral and connection-owned, so disconnecting
the publishing window clears its claim. The projection never contains topology,
titles, commands, process IDs, incarnations, terminal content, or private-state
paths, and focus-specific public errors do not echo filesystem paths.

Terminal, scrollback, and search responses always carry the exact Lair, Dojo,
Splint, incarnation, terminal revision, and history generation. Bounded pages
use an opaque base64url continuation cursor; `truncated: true` requires a cursor
and `truncated: false` requires `null`. Resync results have an explicit symbolic
reason and no continuation. Topology mutations carry the committed topology
revision and the exact affected stable IDs. Multi-Splint restores return one
closed result per Splint rather than pretending a single generic acknowledgement
covers partial outcomes.

Grant IDs and audit IDs in CLI DTOs are nonzero decimal strings. Audit inspection
returns bounded, body-free metadata and daemon-lifetime retention state. Public
responses never echo direct argv, input text, or search queries and never expose
private frame tags, protocol request IDs, subscriptions, controllers, or transfer
IDs. Creation, process-start, layout-mutation, acknowledgement, resize,
confirmation, and multi-restore results use separate closed families; generic
families are shared only where their semantics are identical.

### Exit status categories

A successful one-shot exits 0. A syntactically invalid command line emits no JSON
and exits 2. After successful parsing, machine failures emit one closed,
operation-tagged error envelope and use these stable categories:

- 3: authorization, consent, or confirmation failure;
- 4: daemon connection, authentication, handshake, schema, or version failure;
- 5: invalid request or argument, missing/stale resource, controller state, or
  resource-limit failure;
- 6: cancellation or timeout; and
- 70: unexpected internal failure.

A subscription assigns public subscription ID `"1"` and emits its current state
as public sequence 1. Each subsequently emitted record increments a client-owned
public counter; private daemon sequence values are checked internally but are not
exposed or offset. Terminal initial state uses `snapshot`; topology and control
initial states use `topology_snapshot` and `control_snapshot`. On a daemon sequence
gap, subscriber stall, or replaced history, the client emits the next sequence as
`resync_required` and terminates cleanly. Production resync records include a
`stream` discriminator (`terminal`, `topology`, or `control`). Their `resource`
object is the single authoritative current-state location: terminal records carry
exact Splint/incarnation/terminal revision and, for replaced history, history
generation; topology records carry only topology revision; control records carry
only exact Splint/incarnation. The `resync` object carries only the symbolic
reason, so duplicated or conflicting revision state is impossible. Operation-less
legacy draft fixtures and stream-less legacy resync fixtures remain accepted for
v2 compatibility but are never emitted by production. The caller must explicitly
resubscribe; the CLI does not hide a gap by silently reattaching.

## Controller and confirmation rules

Controller leases belong to one daemon connection and are not reusable across CLI
processes. Public input and resize commands therefore use an atomic same-connection
workflow: acquire control, perform the action, and release control during cleanup.
The machine CLI does not expose reusable controller IDs, standalone acquire/release
commands, transfer requests or decisions, or forced takeover. Observation and control-status subscriptions never acquire control.

Machine mode never prompts. It requires `--yes` for killing a process, closing a
Splint, closing a Dojo, and revoking authorization. Create, split, restore,
relaunch of an exited Splint, rename, ratio, focus hint, input, and resize remain
explicit authorized commands but do not require a second confirmation flag.

In-Splint clients pass `--expected-incarnation N` to `split`, `snapshot`, `send`,
and terminal/control subscriptions. The CLI compares that precondition against
its fresh authoritative lookup, then carries the selected incarnation or topology
revision into the daemon request. A relaunch before or during the operation fails
as `stale_incarnation` or `stale_topology`; the command never silently retargets
the replacement process.

## Persistent policy v2

The policy file is the only persistent authority it defines; absence of the
trusted consent UI fails closed.
[Easy agent access](#easy-agent-access) adds remembered consent grants beside
the policy file without changing it.

CLI commands that need parent IDs or topology CAS compose their operation with
`InspectTopology`; their policy rules also include `topology_metadata_read` with
a matching resource selector. Terminal/control subscriptions preflight with
`InspectSplint` and require `topology_metadata_read` for that Splint. The daemon
request matrix in ADR 0007 remains authoritative for each individual request.

A v2 rule has:

- a unique bounded rule ID;
- an absolute canonical executable path and the SHA-256 digest of the exact
  executable file;
- one or more closed operation scopes from ADR 0007;
- explicit resource selectors;
- operation limits; and
- an optional Unix expiry time.

There are no wildcard scopes, basename identities, path-only identities, or
implicit future resources. A `splint` selector identifies an exact Splint and
either an exact incarnation or the conspicuous value `current`. When a policy
generation is published, a `lair` selector snapshots that Lair plus its existing
Dojos and Splints; a `dojo` selector snapshots that Dojo plus its existing
Splints. Overlaps are deduplicated and each rule may resolve to at most 512
resources. Missing selectors or larger expansions reject the generation and
install deny-all. Later descendants remain unauthorized until the user updates
or reloads policy, which publishes a fresh snapshot. Parent snapshots follow a
stable Splint ID using `current`; directly configured numeric incarnations remain
exact. The singleton `{ "kind": "daemon" }` selector authorizes daemon-level
operations such as creating a Lair and never selects the resulting descendants.

Schema validation is necessary but not sufficient. The daemon must additionally
reject duplicate rule IDs, expired rules, unsafe ownership or mode, symlinks,
hard links, non-canonical paths, unknown resources, invalid scope/selector
combinations, and limits that exceed protocol ceilings. Any load or reload
failure installs a deny-all persistent-policy generation. Use `splinterm policy
validate PATH` and `splinterm policy inspect PATH` for offline administration
through the same secure loader. `splinterm policy reload` asks the canonical
systemd user service to deliver `SIGHUP`; it does not claim the file was
accepted. Publication is atomic, and existing client sessions are disconnected
so subscriptions and connection-owned controller state cannot survive a
narrowed or rejected generation. See [headless.md](headless.md) for installation,
verification, and lifecycle guidance.

## Easy agent access

> **Status:** implemented for 0.2. The contract is fixed by
> [ADR 0014](adr/0014-easy-agent-access.md). The access levels, the three
> durations, remembered grants, the prompt, pending requests, the access list
> with per-entry revoke and **Revoke all access** through `splinterm access`
> and the command palette, the Explorer marks, the waiting indicator, and
> one-step agent setup through `splinterm mcp setup` work as described below.

Easy agent access lets you give an agent access to one workspace (a Lair) in a
single deliberate answer, choose how long that lasts, and see and revoke it
later. It is for agents helping you in your own graphical session. Headless,
remote, and administrative access keeps using the [policy file](#persistent-policy-v2),
which does not change.

What it protects: it keeps cooperating agents inside what you agreed to and
shows you which agent can reach what. It is not a sandbox. A program running as
your account can already read your files and run commands, and can edit the
remembered-grant store, without asking Splinterm.

### Setting up an agent

`splinterm mcp setup claude-code` (or `codex`, or `vscode`) adds the installed
MCP adapter to that agent host's own configuration in one step. `--dry-run`
shows the exact change first, a second run changes nothing, and
`splinterm mcp remove HOST` takes the entry out again. Setup grants no access
and writes no policy: the agent asks for a level the first time it needs a
workspace, and you answer in the prompt described below. Setup also says when
the adapter it names can never be offered "Always", such as a development
build or a raw Nix store path. See
[Set up an agent host](mcp.md#set-up-an-agent-host) for the files, entries, and
safety checks.

### Access levels

The consent window names one of three levels and says in one line what it
allows. A details control lists the exact scopes. Levels are made of the
consent scopes an agent requests; the last column shows the policy scopes each
one corresponds to.

| Level | Allows | Consent scopes | Policy scopes |
| --- | --- | --- | --- |
| **Look** | Read the workspace layout, visible terminals, and scrollback, including search; report agent status | `topology_observe`, `observe`, `scrollback`, `agent_status_report` | `topology_metadata_read`, `topology_subscribe`, `terminal_visible_read`, `terminal_subscribe`, `scrollback_read`, `scrollback_search`, `agent_status_report` |
| **Look and type** | Look, plus type and resize, including taking typing control of a pane your Window controls | Look, plus `input`, `resize`, `control_takeover` | Look, plus `controller_acquire`, `input`, `resize`, `controller_transfer` |
| **Manage** | Look and type, plus start processes, change layout and names, close and terminate, and restore | Look and type, plus `process_spawn`, `topology_layout`, `topology_name`, `terminate`, `process_restore` | Look and type, plus `process_spawn`, `topology_layout_mutate`, `topology_name_mutate`, `process_terminate`, `process_restore` |

Every level covers the whole workspace, including Dojos and Splints created
later, and nothing outside it.

- No level includes clipboard access. A request that asks for a clipboard scope
  is rejected as invalid.
- No level includes authorization inspection or revocation, audit inspection,
  or creating a new Lair. These are not consent scopes at all and stay available
  only through the policy file.
- When an agent with Look and type or Manage takes control of a pane, your
  Window shows the change, and you can take control back.

An agent that asks for anything other than exactly one level is shown as
**Custom** access, with every requested scope listed. Splinterm grants exactly
what was asked for and never rounds a request up to a level. A Custom request
that includes `control_takeover` can be granted just this once only.

### How long access lasts

| Choice | Lasts | Keeps working after | Ends when |
| --- | --- | --- | --- |
| **Just this once** | Five minutes, for that one agent process | Closing every Window | It expires, the agent exits, the workspace is closed, you revoke it, or Splinterm restarts |
| **While this workspace is open** | Until the workspace is closed or Splinterm restarts | The agent restarting or reconnecting; closing every Window | The workspace is closed, the daemon stops or upgrades, or you revoke it |
| **Always for this workspace** | Until you revoke it or the workspace is removed | Splinterm restarting and upgrading; the agent restarting and upgrading | You revoke it, or the workspace is removed |

A workspace is closed or removed when its Lair is terminated, retired, or its
saved metadata is removed. Closing its Windows or the exit of all its Splints
does not close it. Always has no inactivity expiry; the access list shows when
each remembered grant was last used.

### Which agents can be remembered

**While this workspace is open** remembers the agent by its exact executable
path and file digest, so it works for any build, including a development build.
Rebuilding the agent makes it a different program, and it is asked again.

**Always for this workspace** is offered only when the account cannot modify the
agent's executable file itself:

- a **protected install**, such as a distribution package under `/usr/bin`:
  every directory on the path and the file itself are owned by root and are not
  writable by anyone else; or
- a **Nix profile** entry, such as `/run/current-system/sw/bin/NAME` or
  `~/.nix-profile/bin/NAME`, that resolves to the running file in a root-owned
  multi-user `/nix/store`. System profiles are checked before user profiles.

This protects the file, not how it runs. Environment such as `LD_PRELOAD`, and
configuration, plugins, or scripts the program loads from places you can write,
can still change what it does.

Splinterm checks this again every time the agent connects, so a package upgrade
keeps the grant without asking. Restart an agent that was running before the
upgrade: it is still running the old, replaced file, which no longer matches.

A Nix profile link is owned by you. Any program running as you can have Nix
build any program into the store and repoint the profile at it, and that
program would then match. A root-owned store path means Nix built it, not that
anyone checked it.

Some agents are offered fewer choices, and the prompt shows the missing ones
disabled with the reason:

- **Scripts run by an interpreter**, such as `python`, `node`, `bash`, `sh`,
  `perl`, or `ruby`, get just this once and while this workspace is open.
  Remembering the interpreter would cover every script it runs. Splinterm
  recognizes interpreters by the name of the running executable. This is a
  convenience that prevents mistakes, not a security boundary. Compiled agents
  and the Splinterm MCP adapter get all three choices.
- **Development builds** and other programs in places you can write get the
  two shorter durations.
- **The SSH relay** `splinterm-relay` gets just this once. Remote automation
  uses the policy file.
- **A Custom request with `control_takeover`** gets just this once.

A longer grant belongs to the program, not to one process. The prompt says so in
one line, for example "Any program that runs `/usr/bin/splinterm-mcp` shares
this access." A host or agent name is only shown, never trusted. Splinterm never
identifies an agent by host name, basename, or a copied digest.

### The prompt

The prompt is deliberate by default:

- With the Omarchy integration enabled it appears without taking keyboard
  focus, so typing meant for another Window never reaches it. Click it when you
  get to it, or press Ctrl+Alt+G: its keyboard stays inert until you do, even
  if your desktop gives it focus,
  and Ctrl+Alt+G itself never answers. Under Omarchy, the Hyprland module that
  `splinterm integration omarchy enable` installs maps it without focus, and
  Ctrl+Alt+G then reaches it from any Window while it waits; see
  [Trusted dialogs](usage.md#trusted-dialogs) and
  [Integrations](integrations.md).
- Enter, Escape, and `D` deny. Only `G` or a click on the green **Allow**
  button (right) grants.
  Earlier releases granted on Enter; this one does not.
- Input is ignored for a short moment after your click, each time the prompt
  regains focus, and each time you change the duration. Held-down keys never
  answer it. A click on the red **Deny** button (left) always counts.
- You choose the duration with the mouse or the arrow keys, never with a
  letter. **Just this once** is preselected, and a duration that is not offered
  is shown with the reason and cannot be chosen.
- The Allow button names your exact choice, for example **Allow Manage while
  open** or **Always allow Manage**, and the chosen duration's row names the
  workspace. That one answer is the whole confirmation.
- Choosing a longer duration shows who shares it, and, when your answer would
  replace an earlier one for the same agent and workspace, what it replaces.

The prompt shows the request as one sentence with the agent's reported name.
When the agent reported a name, a note follows that the agent names itself and
Splinterm cannot verify that name. Then come the computer the agent runs on,
its exact executable path, the workspace name with its short ID, the level
with one line of what it allows, and the exact scopes. Names and paths are
shown escaped, as data, in the same form as the **Agent access** list: a
control or bidirectional character, and every quote or quote look-alike,
appears as an escape such as `\u{202e}`, and a run of spaces as one space, so
the name you approve reads the same in the list and cannot imitate the
prompt's own wording.

The computer comes first in the table, on the `host` row, for example
`this computer (wintermute)`, or just `this computer` when its hostname is not
usable.

A request that a grant already covers is answered at once, without a prompt.
Prompts appear one at a time; further requests wait their turn. If you do not
answer right away, the request stays pending for up to two minutes, and the
agent collects your answer by asking again, without a new prompt. The prompt
counts the time left down at the right of its top edge, such as
`denies in 1:42`. Once it runs out, the Allow button is disabled and nothing
grants; the prompt then closes and the request ends unanswered, without
access. After you deny a request, the same agent cannot ask again for that
workspace for a few seconds. Closing the prompt denies.

If nothing can show the prompt, the request fails with `consent_unavailable`
at once: "no Splinterm Window or approver connected to this computer can show
the prompt".

On a computer without a Window, such as a headless host, run
`splinterm approvals` in your own terminal there to answer prompts as text
([Usage](usage.md#answering-access-prompts-in-a-terminal)). It shows the same
request, starts on Deny, and refuses to run inside a Splinterm pane or a tmux or
screen session started from one, so an agent cannot answer its own request.

### Seeing and revoking access

`splinterm access list` shows every grant of every duration, with the agent's
reported name, its executable path and how it is identified (one process, an
exact build, a protected install, or a Nix profile entry), the workspace, the
level or Custom scopes, when it was created, when an "Always" grant was last
used, when a shorter one expires, and what it is doing now: the agent
connections it authorizes and the control it holds. Pending requests are
listed too, and so are policy rules that match a connected agent, marked as
coming from the policy file and read-only here.

**Agent access** in the command palette shows the same list in a Window,
grouped by kind under the headers `always`, `while open`, `once`, `waiting`
and `policy` while nothing is typed. Each row names the agent, the access
(with its kind in words, such as "always for" or "while"), and the
workspace, and says whether the agent is connected now; the line under the
list shows the selected entry's executable, how it is identified, its
times, and what Enter does. Type to search; the results are one list. A click only
selects a row and never revokes it; Enter revokes. Enter on a just-this-once or
while-open grant revokes it at once; on an "Always" grant it first asks, with
Cancel selected. A pending request or a policy rule
explains instead: answer the request in its prompt, or remove the rule from
`policy.json`. Agent names and paths are shown escaped, as data. The list
refreshes every few seconds, and a confirmation whose entry changed meanwhile
is shown again on Cancel; one whose entry ended closes without revoking.

Each entry has one ID:

| ID | Entry | Revocable here |
| --- | --- | --- |
| `17` | A just-this-once or while-open grant (its runtime grant ID) | Yes |
| `ra_` and 32 hex digits | An "Always" grant (its remembered entry ID) | Yes |
| `pending:3` | A pending request | No; it ends when answered or expired, or with `--all` |
| `policy:RULE` | A policy rule | No; edit `policy.json` |

An "Always" grant also shows the runtime grant ID its controllers belong to;
revoke it by its `ra_` ID, never by that runtime ID.

```bash
splinterm access list
splinterm access revoke 17
splinterm access revoke ra_0123456789abcdef0123456789abcdef
splinterm access revoke --all --dry-run
splinterm access revoke --all
```

Revoke takes effect immediately: new requests stop, subscriptions close, and
control is released, including a control transfer the grant asked for. An
"Always" grant is then removed from the store. If that write fails, the grant
stays revoked now, the command says so and exits with an error, and the list
shows it as returning after a restart until a later write succeeds; Splinterm
retries on its next write and at shutdown.

`splinterm access revoke --all` first shows the exact number of remembered
grants it will remove, with the shorter grants and pending requests it ends,
and asks before acting (`--yes` skips the question; `--dry-run` only counts).
It never touches policy rules or the trusted UI.

Revoking a remembered grant does not remove a matching policy rule. When one
exists, the list and the revoke result name the rule and tell you to edit
`policy.json` to remove it.

**Revoke all access** in the command palette (and its shortcut) works the same
way: it first shows the exact counts from a dry run, with Cancel selected.
When you confirm, Splinterm counts again; if the numbers changed, nothing is
revoked and you are shown the new numbers to confirm. A grant that arrives
between that second count and the revoke is revoked too, and the result shows
what was removed. A single-pane Window has no palette; there the shortcut
still ends only the grants that Window shows and every pending request, so
use `splinterm access revoke --all`.

The Explorer marks which reach an agent has, with an icon in its own column
and in words in the selected row's footer and each row's accessible text:
**Agent can reach** (a grant or matching policy rule covers it, but no agent
it covers is connected) or **Agent connected** (one is connected or holds
control now), on the workspace, or **…part** when only some of its Dojos or
Splints are covered. **Agent asking for access** marks a workspace with a
request waiting for your answer, and while any request waits the Window title
ends with **AGENT ACCESS REQUEST WAITING** and **Agent access** in the palette
shows how many wait. The prompt's keys stay inert until you click it or press
the dialog shortcut.

The human command speaks as the installed trusted client, so it works only
with the packaged `splinterm` next to the running daemon. The installed
graphical relay a native remote Window uses (the remote interactive role) may
list and revoke as well, as it may already revoke grants; like the rest of
easy agent access this bounds cooperating clients and is not a sandbox
([ADR 0014](adr/0014-easy-agent-access.md)). `splinterm access` itself refuses
`--remote`. Machine output
(`--output json`) uses the automation role: listing needs
`authorization_inspect` on the daemon, revoking one entry needs
`authorization_revoke` on its workspace (on the daemon for a workspace that no
longer exists), and `--all` needs `authorization_revoke` on the daemon, all from
the policy file. No access level includes these scopes. Machine revokes require
`--yes`; a failed durable removal is a successful revoke whose
`durable_removal.status` is `failed`. `access list` pages with `--after ID` and
`--max-entries N` (at most 64); `truncated: true` means `next_after` names the
ID to continue after, and `authorization_revision` advances on every grant or
pending-request change, so a changed revision between pages means the list
moved.

`splinterm authorization status` and `splinterm authorization revoke` still
work for one Splint and its just-this-once grants; `splinterm access` covers
every grant.
Audit records a listing as `authorization_status` and every revoke as
`revoke_access`.

### Where remembered access is stored

Remembered grants live in `~/.local/state/splinterm/agent-access.json`
(`$XDG_STATE_HOME/splinterm/agent-access.json` when set), separate from
`policy.json`. Splinterm writes it, owner-only, and reports a remembered grant
only after the file is safely written; if the write fails, nothing is granted.
If the file is changed by something else, Splinterm refuses to overwrite it and
reports the conflict. A damaged or unsafe file disables remembered access until
it is repaired, and policy and shorter grants keep working. `splinterm reset`
moves it to the same backup as your sessions.

After a live upgrade, remembered grants are read fresh from this file. Every
agent connection, its control, shorter grants, and pending requests end, and
agents reconnect without a new prompt if they still match.

Agent status reporting is part of every level and uses the ordinary automation
connection. A remembered or workspace grant covers every connection from the
same agent program, so status reports from a fresh connection need no new
prompt.

## In-Splint automation context

A process running inside a Splint receives no authority merely because of its
location. `splinterd` overrides and injects `SPLINTERM_LAIR_ID`,
`SPLINTERM_DOJO_ID`, `SPLINTERM_SPLINT_ID`, and
`SPLINTERM_SPLINT_INCARNATION` into each PTY child. These values are discovery
hints so a CLI-using coding agent can identify its initial logical location
without guessing from topology. They are not credentials, policy selectors,
proof of ancestry, or consent, and the daemon never trusts values presented
back by a client. Relaunch injects the new incarnation.

Automation must reconcile every hint against a fresh authorized `topology`
response. Absence, malformed values, stale incarnation, process exit, or denied
topology access must not broaden policy or cause an adapter to select an
arbitrary Splint. See [integrations.md](integrations.md) for the client-author
checklist, reference picker, and safe shell/`jq` workflows.

## Agent status

An agent can tell Splinterm what it is doing so a person can see which Splint
is working, which needs them, and which has finished or failed. Status comes
only from an explicit report. Splinterm never infers it from terminal text,
titles, prompts, process names, or output. A report is display data: it cannot
grant access, confirm consent, send input, or change topology.

```bash
splinterm agent-status set STATE [--detail TEXT] [--ttl SECONDS] [--splint ID --incarnation N]
splinterm agent-status clear [--splint ID --incarnation N]
splinterm agent-status get [SPLINT_ID]
```

`STATE` is one of `working`, `needs_input`, `done`, `error`, or `idle`; any
other value is a usage error. `--detail` is optional display text of at most 80
characters and 240 UTF-8 bytes. Control, bidi-formatting, and line-break
characters are rejected, never stripped. `working` and `needs_input` are active
states: they turn stale after `--ttl` seconds without a fresh report (default
120, bounds 10 to 900). `done`, `error`, and `idle` last until replaced,
cleared, or the incarnation ends, unless `--ttl` is given. Re-sending the same
state and detail refreshes the TTL without publishing a change, so a heartbeat
should re-report at no more than half the TTL. A stale status keeps its last
state and detail and is dropped 15 minutes later. Status lives only in daemon
memory: it is dropped when the incarnation exits or relaunches and does not
survive a daemon restart or live handoff.

The graphical client shows status in the Lair Explorer and as one icon badge per
Dojo tab; see [Human usage](usage.md#agent-status-in-the-explorer-and-on-dojo-tabs).

### Target and authority

Run inside a Splint, the target defaults to `$SPLINTERM_SPLINT_ID` and
`$SPLINTERM_SPLINT_INCARNATION`, so `splinterm agent-status set working` needs
no other arguments. `--splint` and `--incarnation` are given together and
override the environment. When neither the flags nor the environment name a
target, the command fails before connecting with the distinct usage error
`not inside a Splint` (exit 2, no stdout). An incomplete or malformed Splint
environment is a separate usage error that also exits 2. `get` defaults to
`$SPLINTERM_SPLINT_ID`.

The environment is only a discovery hint; the daemon decides authority from the
reporting process. A report is accepted without any grant or consent prompt
when the reporting process is in the target Splint's live PTY session, or
descends from the Splint's root process. A process can therefore report only
for the Splint it runs in. Any other process needs an `agent_status_report`
grant for that Splint from policy or interactive consent. In particular, an
agent that fully detaches from the Splint, by calling `setsid` and being
reparented to init or `systemd --user` (for example an agent inside a tmux
server that was started inside the Splint), is outside the Splint by this rule
and needs a grant. Cgroup or workload-scope membership is never evidence.
Reports target exact incarnations: a report naming an exited or replaced
incarnation fails as `stale_incarnation`, and admission is limited to 10
reports per second per Splint (retryable `resource_limit`).

Reports never preflight topology, so a self-reporting hook needs no read grant.
`get`, `inspect`, and `topology` read status through the ordinary
`topology_metadata_read` authority. Reports always connect with the automation
role, in human mode too; `agent-status` reports go only to the local daemon,
and `--remote` is accepted only for `get`. A remote agent reports to its own
daemon from inside its own Splint.

### Machine output and exit codes

With `--output json`, `set` and `clear` emit `agent_status_set` and
`agent_status_clear` documents whose resource is the exact `splint_id` and
`incarnation`. Their data reports `accepted: true`, the daemon status
`sequence`, and for `set` the state and nullable `expires_at_ms` (Unix
milliseconds at which an active report turns stale). The detail is never
echoed. `get` emits `agent_status_get` with the Splint's topology location,
its nullable `current_incarnation`, and a nullable `agent_status` object.

`inspect_splint` data and each `inspect_topology` Splint summary gain the same
optional `agent_status` object only while the live incarnation has a status:
`state`, nullable `detail`, `stale`, `reported_age_ms`, and `sequence`. Its
absence means no status; documents without status are unchanged. Topology
subscriptions do not carry status in this version.

A report exits 0 when accepted and otherwise uses the categories above: 2 for
usage errors including `not inside a Splint`, 3 when the reporter is neither
inside the Splint nor granted, 4 for daemon connection failures, 5 for a stale
incarnation, an invalid report, a missing Splint, or the rate limit, and 6 for
a timeout. Human mode prints one confirmation line on success, exits 1 on a
daemon failure, and exits 2 on usage errors.

### Hook recipes

Agent hooks run as descendants of the agent, inside its Splint, so they
self-report with no grant. End every hook command with `>/dev/null 2>&1 ||
true`: a hook outside Splinterm then does nothing, and some hosts treat hook
stdout or a nonzero exit as input to the agent.

Claude Code (`~/.claude/settings.json` or a project `.claude/settings.json`):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {"hooks": [{"type": "command", "command": "splinterm agent-status set working --ttl 900 >/dev/null 2>&1 || true"}]}
    ],
    "PostToolUse": [
      {"hooks": [{"type": "command", "command": "splinterm agent-status set working --ttl 900 >/dev/null 2>&1 || true"}]}
    ],
    "Notification": [
      {"matcher": "permission_prompt", "hooks": [{"type": "command", "command": "splinterm agent-status set needs_input --ttl 900 >/dev/null 2>&1 || true"}]}
    ],
    "Stop": [
      {"hooks": [{"type": "command", "command": "splinterm agent-status set done >/dev/null 2>&1 || true"}]}
    ],
    "SessionEnd": [
      {"hooks": [{"type": "command", "command": "splinterm agent-status clear >/dev/null 2>&1 || true"}]}
    ]
  }
}
```

`UserPromptSubmit` marks the agent `working`, `Notification` marks it
`needs_input` when Claude Code asks for permission, and `Stop` marks it `done`
when the turn ends. `PostToolUse` returns the status to `working` after a
permission is approved and refreshes the TTL during long turns; identical
re-reports do not publish a change. The `permission_prompt` matcher matters:
Claude Code also sends a notification after waiting idle for a prompt for about
a minute, and an unmatched `Notification` hook would replace `done` with
`needs_input` after every turn. Both active states use `--ttl 900`, so a long
tool run or an unanswered permission prompt stays visible for 15 minutes before
it is shown as stale.

Codex CLI runs its `notify` program after each completed agent turn. Map that
to `done` in `~/.codex/config.toml`:

```toml
notify = ["/bin/sh", "-c", "splinterm agent-status set done >/dev/null 2>&1 || true"]
```

Codex's documented `notify` hook has no prompt-submitted or approval event, so
`working` and `needs_input` are left unreported there unless a wrapper reports
them. Any other agent can call the same commands, or the MCP tool
`splinterm.report_agent_status` described in [mcp.md](mcp.md).

## Audit v2

Audit records are bounded metadata with the schema `splinterm.audit.v2` and
retention label `daemon_lifetime`. IDs are monotonic and nonzero during one
daemon lifetime. The daemon retains the newest 1,024 records; paginated reads
must report a retention gap rather than hide it.

The audit schema intentionally has no fields for terminal rows, terminal bytes,
scrollback or clipboard bodies, input bytes, search queries, capability tokens,
environment contents, or complete command arguments. Spawn and restore outcomes
may include only an argument count and redacted executable basename. The
one-shot `audit_inspect` projection serializes audit IDs as decimal strings even
though daemon-private counters remain integers.

## Contract fixtures

Golden fixtures live under `tests/automation/fixtures/`. Each wrapper names one
schema and contains the document being tested. The invalid corpus includes the
security-significant cases of contradictory envelopes, incomplete resync,
wildcard scope, path-only executable identity, and an attempted terminal body in
an audit record.

Validate the draft contracts with:

```bash
uv run --with jsonschema python tools/automation/validate-contract-fixtures.py
```

Later implementation slices must retain these fixtures, add command-specific
examples, validate every emitted machine document, and preserve reviewed v2 fixtures when adding compatible fields.
