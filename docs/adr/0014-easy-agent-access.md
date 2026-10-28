# ADR 0014: easy agent access

- **Status:** Accepted for `0.2.0`; implemented (the grant engine, the
  consent prompt, pending requests, the access list with per-entry revoke and
  Revoke all access through the daemon API, the human and JSON CLI, and the
  command palette, the Explorer marks, the waiting indicator, and one-step MCP
  host setup (`splinterm mcp setup`)); where a prompt is shown amended by
  [ADR 0015](0015-remote-consent.md)
- **Date:** 2026-10-20
- **Amends:** [ADR 0005](0005-trusted-consent-broker.md) (consent outcomes,
  prompt input, and persistence), [ADR 0007](0007-supported-automation-policy.md)
  (a second persistent authority source beside policy), and
  [ADR 0011](0011-guarded-in-place-daemon-reexec.md) (what an upgrade reloads)

## Context

Today an agent gets access to a live workspace in one of two ways. It can ask
through `RequestLairAccess`, which opens the trusted consent window. That
window offers only Deny and Grant once, expires after 20 seconds, and lists
individual scopes. The resulting grant lasts five minutes, lives in daemon
memory, and is bound to the requesting process, so a long agent session is
asked again and again and a restarted agent starts over. Or the person writes a
`policy.json` rule by hand, naming the adapter's path and SHA-256 digest, the
scopes, and exact resource IDs. The rule stops matching after every package
upgrade, covers only the resources present when it was loaded, and every reload
disconnects all automation clients. Nothing lists which agent can reach what.

Granting access must be easy for the person and still deliberate. The design
also has to be honest about what it protects. A program running as the user
can already read the user's files and keys and run any command without
Splinterm. Consent and policy cannot stop that program. What they can do is
keep cooperating agents inside what the person agreed to, show the person which
agent can reach what, and let the person take access away.

## Decision

### What this protects, and what it does not

Access levels, durations, remembered grants, and the access list bound
cooperating agents and make their access visible and revocable. They are not a
sandbox. A hostile program running as the account can edit any file the
account owns, including the remembered-grant store, can start the graphical
relay or client itself, and can reach terminals without asking Splinterm.
Documentation must say this wherever remembered access is described, in the
same terms as `FR-REMOTE-02`.

### Access levels

The consent window offers three levels. Each level is a fixed bundle of
**consent scopes**, the vocabulary `RequestLairAccess` already uses. Each larger
level contains the smaller ones. Every level covers the named workspace (Lair)
and all of its current and future Dojos and Splints, as the existing Lair grant
does. No level reaches another Lair or a daemon-level resource, so no level can
create a new Lair. A grant carries no numeric limits of its own; the protocol
ceilings apply.

| Consent scope | Policy scopes it authorizes (ADR 0007) | Look | Look and type | Manage |
| --- | --- | :-: | :-: | :-: |
| `topology_observe` | `topology_metadata_read`, `topology_subscribe` | yes | yes | yes |
| `observe` | `terminal_visible_read`, `terminal_subscribe` | yes | yes | yes |
| `scrollback` | `scrollback_read`, `scrollback_search` | yes | yes | yes |
| `agent_status_report` | `agent_status_report` | yes | yes | yes |
| `input` | `input`, with `controller_acquire` | | yes | yes |
| `resize` | `resize`, with `controller_acquire` | | yes | yes |
| `control_takeover` | `controller_transfer` | | yes | yes |
| `process_spawn` | `process_spawn` | | | yes |
| `topology_layout` | `topology_layout_mutate` | | | yes |
| `topology_name` | `topology_name_mutate` | | | yes |
| `terminate` | `process_terminate` | | | yes |
| `process_restore` | `process_restore` | | | yes |
| `clipboard_read`, `clipboard_write` | none | invalid | invalid | invalid |
| not a consent scope | `authorization_inspect`, `authorization_revoke`, `audit_inspect` | | | |

The bundles cover 16 of the 19 policy scopes. `topology_observe` covers both
topology read and subscribe, `observe` covers both visible-terminal read and
subscribe, and `scrollback` covers both read and search. `controller_acquire`
has no consent scope of its own; `input` or `resize` implies it.

In words:

- **Look** reads the workspace's layout, the visible terminal, and scrollback,
  including search.
- **Look and type** adds sending input and resizing. It also includes
  `control_takeover`, so the agent can take typing control of a Splint that
  another connection controls, including the person's Window. The person's
  Window shows the change of controller, and the person can take control back.
- **Manage** adds starting processes, changing layout and names, closing and
  terminating, and restoring.
- **Every level** includes reporting agent status.
- **No level** includes clipboard access, authorization inspection or
  revocation, audit inspection, or any handoff or trusted-UI authority.

The clipboard scopes stay invalid in a workspace request. A request that
contains either one is rejected as invalid, as today, and is never shown as
Custom. Authorization and audit inspection are not consent scopes at all.
Nothing can request them through the consent window. They are available only
through the policy file.

An agent may ask for a level by name; the daemon expands the name to exactly the
bundle above. An agent may instead ask for an explicit set of consent scopes.
Matching and granting both use consent scopes. If the set equals one bundle
exactly, the prompt names that level. Otherwise the prompt shows **Custom**
access, lists every requested scope, and grants exactly that set. A request is
never rounded up to the next level, and the prompt never grants more than was
asked for. The person can accept or deny the request as shown. To get less,
the person denies and the agent asks again.

**Takeover outside a level.** `control_takeover` is durable only as part of
Look and type or Manage. A Custom request that contains it is offered **Just
this once** only. The two longer durations are shown disabled, with the reason.
Today's Lair consent already grants takeover for five minutes, so Just this once
keeps current behavior.

### Durations

The person chooses one of three durations. Duration and identity are decided
together: the longer a grant lasts, the more stable, and the more carefully
checked, the identity it is bound to.

| Choice | Bound to | Lasts | Survives | Ends |
| --- | --- | --- | --- | --- |
| **Just this once** | The requesting process: UID, PID, start time, executable device and inode, and executable path and digest (today's grant) | Five minutes | Closing every Window | Expiry, requester exit, revocation, workspace end, daemon restart or upgrade |
| **While this workspace is open** | The verified executable identity of ADR 0007 (UID, canonical path, and SHA-256 digest of the executing file) plus the Lair | While the Lair exists in this daemon generation | Agent restart and reconnect, closing every Window | Workspace end, daemon stop, restart, crash, or upgrade, revocation |
| **Always for this workspace** | A durable identity (below) plus the Lair | Until revoked or the workspace is removed; no automatic expiry | Daemon restart and upgrade, agent restart and upgrade | Revocation, workspace removal |

A **workspace ends** when its Lair leaves daemon topology: it is terminated,
retired by the capacity policy, or its saved metadata is removed. Closing its
last Window, detaching, or the exit of all its Splints does not end it. These
are the same events that revoke a Lair grant today. Lair IDs are never reused,
so a grant can never attach to a different workspace.

"While this workspace is open" means the Lair's lifetime, not Window
visibility. Help text says "until this workspace is closed or Splinterm
restarts". A graphical disconnect never revokes it.

"Always for this workspace" has no inactivity expiry. The access list shows when
each remembered grant was last used. An optional explicit expiry may be added
later; a hidden inactivity rule will not.

**Which durations are offered.** The daemon decides from the requester and the
request, and the prompt shows every choice it withholds as disabled, with a
one-line reason:

| Requester or request | Offered |
| --- | --- |
| Peer without a pidfd-pinned identity (`SO_PEERPIDFD` unavailable) | Just this once |
| Custom request containing `control_takeover` | Just this once |
| `splinterm-relay` (below) | Just this once |
| Known interpreter (below) | Just this once, While this workspace is open |
| Development build or any other executable without a durable identity | Just this once, While this workspace is open |
| Protected install or qualifying Nix profile entry, including the packaged `splinterm-mcp` adapter and other compiled agents | All three |

A development build qualifies for While this workspace is open because that
binds its exact digest. Rebuilding changes the digest and therefore the
identity.

### Identity for remembered grants

A remembered grant stores a durable identity instead of a digest, so that it
survives upgrades of the agent. The daemon accepts exactly two kinds, checks the
connecting process against the stored identity on **every new connection**, and
caches nothing between connections.

Both kinds start from the same evidence ADR 0007 already requires. The daemon
reads `SO_PEERCRED` and `SO_PEERPIDFD`, opens the peer's `/proc/<pid>/exe`
read-only, and treats that open file as the executing file. Without a pidfd,
remembered and workspace authority fail closed.

**Protected install.** The stored identity is an absolute, canonical path `P`.
A connection matches when, at that connection:

- every component of `P`, from `/` to the file, opens without following a
  symbolic link;
- every directory on the path, and the file itself, is owned by root and is
  neither group- nor world-writable, and none carries an access ACL that grants
  write permission to anyone else;
- the file is a regular executable file; and
- the file opened through `P` in the daemon's mount namespace has the same
  device and inode as the executing file.

The last check means an executable running from a different mount namespace,
a copy, or an old, replaced file does not match. After a package upgrade
replaces the file at `P`, newly started agents match without a prompt. An agent
process that is still running the old, deleted file does not match. It must
be restarted, or it is asked again.

These checks protect the executable file, not how it runs. The account cannot
modify the file itself. It can still change what the program does through
`LD_PRELOAD` or other environment, and through configuration, plugins, or
scripts the program loads from user-writable places.

**Nix profile.** The stored identity is a profile entry path `P` from this
closed list, where `HOME` and `USER` come from the password database entry for
the daemon's UID, never from any environment. The list is in priority order,
with system profiles first:

1. `/run/current-system/sw/bin/NAME`
2. `/etc/profiles/per-user/USER/bin/NAME`
3. `/nix/var/nix/profiles/default/bin/NAME`
4. `/nix/var/nix/profiles/per-user/USER/profile/bin/NAME`
5. `HOME/.nix-profile/bin/NAME`
6. `HOME/.local/state/nix/profile/bin/NAME`

`NAME` is a single path component. The daemon resolves `P` with this walk, in
its own mount namespace:

- It walks the path one component at a time. Any component, including a
  directory inside a store path such as a profile's `bin`, may be a symbolic
  link. Every link followed counts toward a limit of 40.
- A relative link target resolves against the directory that contains the link.
- `..` resolves physically, against the actual parent of the directory reached
  so far, never by removing text from the path.
- Every path formed during the walk, and the link text read at each step, is at
  most 4096 bytes. Exceeding either bound, or the link limit, fails the match.

A connection matches when, at that connection:

- the walk ends at a regular executable file whose physical path has the form
  `/nix/store/HASH-NAME/...`;
- `HASH-NAME` is well formed: 32 characters of the Nix base-32 alphabet
  (`0123456789abcdfghijklmnpqrsvwxyz`), then `-`, then a valid store name of
  letters, digits, and `+-._?=` that does not start with `.`;
- `/` and `/nix` are owned by root and are neither group- nor world-writable;
  `/nix/store` is owned by root and may be the usual sticky, group-writable
  store directory; the store object and every directory and file beneath it on
  the physical path are owned by root and are neither group- nor world-writable;
  and
- the resolved file has the same device and inode as the executing file.

The walk runs again in full on every new connection. Nothing from an earlier
resolution is reused.

Intermediate links may be owned by the user. That is what lets a profile follow
upgrades, and it carries an accepted residual risk:

- The user controls the profile link. Any software running as the user can ask
  the multi-user Nix daemon to build any program into `/nix/store`, then
  repoint the profile at it. The next process started from that store path
  matches the remembered grant.
- Root ownership of a store path means the Nix daemon wrote it. It does not mean
  anyone vetted it.

A single-user Nix installation, whose store is not owned by root, does not
qualify. An executable started through a Nix wrapper script runs a different
file from the one the profile entry names, so it does not match.

**Choosing the identity at prompt time.** To offer Always, the daemon must find
a qualifying identity when it shows the prompt:

1. It tests the executing path as a protected install.
2. Otherwise it tests, in list order, the profile entries whose `NAME` equals
   the executing file's name. The first entry that matches is stored, so a
   system profile wins over a user profile.

The name only selects which entries to test; the device and inode check
decides the match.

**Never an identity.** Host name, basename, MCP client name, argv, environment,
parent process, the same UID, or a copied digest never establish or select a
remembered grant. The host-reported agent name is stored and shown only as
display data.

**Shared identity.** A While open or Always grant belongs to an executable, not
to a process. Every program that runs that executable shares it. For example,
every MCP host that starts `splinterm-mcp` shares that adapter's grant. Each
time the prompt offers one of the longer durations, it shows one line in the
same form for every executable: "Any program that runs `<path>` shares this
access."

**Known interpreters.** When the executing file is a known interpreter, the
prompt offers Just this once and While this workspace is open. Always is shown
disabled with the reason "This program runs scripts; any script it runs would
share this access."

- The daemon decides by the basename of the resolved executing file, the target
  of `/proc/<pid>/exe`. It compares that name against a fixed list built into
  the daemon, ignoring a trailing version suffix made of digits and dots. The
  list covers at least `python`, `node`, `nodejs`, `deno`, `bun`, `bash`, `sh`,
  `dash`, `zsh`, `fish`, `ksh`, `mksh`, `busybox`, `perl`, `ruby`, `php`, `lua`,
  `luajit`, `tclsh`, `java`, `pwsh`, and `Rscript`.
  For example, `python3.12` and `perl5.38.2` match.
- This is a **user-experience rule, not a security boundary**. It keeps the
  person from remembering a grant that would follow every script by accident.
  A renamed or copied interpreter evades it. The basename can only withhold
  Always. It never grants or selects anything.
- Compiled agents and the Splinterm MCP adapter are offered all three
  durations.

**Relay.** The installed relay is never eligible for While this workspace is
open or Always, because it relays for every caller that can reach it over SSH.
It is the `splinterm-relay` executable in the daemon's installed prefix.
The daemon recognizes it by matching the executing file's device and inode
against that installed file, never by name. Remote and headless automation
keeps using the policy file.

### The prompt

The prompt stays a daemon-launched trusted Window, as decided in ADR 0005. It
changes as follows:

- It shows the agent's host-reported name as data, the exact executable path,
  the workspace name and short ID, the level name with one line saying what it
  allows (for example "Read terminals, type commands, and create or close
  sessions" for Manage), and a details control listing the exact scopes. For a
  Custom request, the scope list is always visible.
- It is **default-deny**. Enter, Escape, and `D` deny. Only `G` or a pointer
  click on the grant button grants. This is a deliberate change: the access
  dialog grants on Enter today.
- It **never takes keyboard focus** when it appears (operator rule for every
  trusted Splinterm dialog, 2026-10-20). The prompt maps under its own app ID,
  `com.oldjobobo.splinterm.trusted-dialog`, which the Omarchy integration's
  Hyprland module gives `no_initial_focus` and `focus_on_activate = false`, and
  Splinterm never asks the compositor to activate it. Whatever the
  compositor does, the prompt's keyboard is inert until the person **clicks
  inside it** or presses the **dialog shortcut** while it has focus: until
  then every key, including the deny keys, is ignored. Keyboard focus alone
  never engages it. Closing it, or no answer before the request expires,
  denies. A hint line says to click it or press the shortcut first.
- The dialog shortcut (Plan 0053 item 9, operator decision 2026-10-20) is
  **Ctrl+Alt+G**, configurable as `[integration] dialog-shortcut` to another
  Ctrl+Alt chord with one letter or digit, never with Shift or Super (`none`
  turns it off). It engages the prompt exactly as a click inside does and
  **never answers**; no Super chord is used, configured or not. Any other
  key pressed with Ctrl, Alt, or Super held is ignored, so the chord's
  modifiers never turn into an answer.
  Where the prompt maps without focus (the integration's rule above), the
  prompt also enables a binding of the chord in Hyprland for exactly as long
  as it waits: `hyprctl eval`, in the background, enables a non-consuming
  binding that focuses the waiting dialog, matched by its process ID and the
  trusted dialog app ID, and lets the chord through to it, which then
  engages. The prompt removes its entry once it has answered, the process
  that reaped it removes the entry again, so a killed prompt leaves no
  enabled binding, and each new prompt drops entries whose process is gone;
  the binding is disabled with the last entry, never removed, because
  Hyprland removes every binding of a chord at once. A binding object that
  Hyprland has dropped is recognized only by its string form, since touching
  one crashes Hyprland 0.56. When the person's own Hyprland configuration
  binds the chord in any submap, Splinterm enables nothing. Elsewhere, the
  chord engages only a prompt that already has focus. The same applies to every trusted dialog.
- After that click or shortcut, key presses and the grant click are ignored
  until an arming delay of 600 ms has passed. The delay restarts every time
  the Window gains keyboard focus again and every time the selected duration
  changes. Key repeats are always ignored. A click on the deny area always
  counts.
- The person selects a duration with the pointer or the arrow keys, never with
  a letter key. **Just this once** is preselected. Selecting a duration never
  grants.
- The grant button names the exact choice: "Allow Look once", "Allow Look and
  type while 'Workspace' is open", "Always allow Manage for 'Workspace'". This
  one careful prompt is the whole confirmation. No level, including Manage with
  Always, asks a second time.
- Each withheld duration is shown disabled with its one-line reason.
- When the person selects a longer duration, the prompt shows the
  shared-identity line.
- When the answer would replace an existing remembered grant for the same
  identity and workspace, the prompt says what it replaces.

The workspace name is user data and can be renamed by any agent holding Manage.
It is always shown together with the short ID.

### Pending requests

A prompt lasts longer than an MCP call. MCP calls use a 5-second default daemon
deadline, configurable up to 30 seconds, and the daemon cannot know a client's
deadline. So a request that needs a prompt becomes a bounded, cancellable
**pending request**.

**Limits.**

- The daemon opens at most one pending request per requesting process and
  workspace, and at most eight in total. Excess requests fail with the retryable
  `resource_limit`.
- A pending request expires 120 seconds after it opens. This replaces the
  20-second prompt deadline.

**Prompts and waiting.**

- Only one prompt is visible at a time. The other requests wait in a queue in
  arrival order. While requests wait, Windows show a visible pending indicator,
  and the access list shows them.
- The request carries a bounded **wait budget**. The adapter sets it below its
  own dispatch deadline, leaving a margin, and the daemon caps it at 25
  seconds.
- If the person has not answered when the budget runs out, the daemon replies
  `pending`, with the request's identifier and expiry. This is not a failure.

**Repeats and cancellation.**

- The requesting process collects the answer by repeating the same request from
  any of its connections. A repeat with the same workspace and the same scopes
  joins the pending request and never opens a second prompt.
- A repeat asking for different access while one is pending fails with the
  existing request's identifier.
- The requester can cancel its pending request.

**Ending and committing.**

- A pending request ends without a grant when it expires, is cancelled, the
  requesting process exits, the workspace ends, **Revoke all access** runs, or
  the daemon fences for an upgrade. Its prompt closes or leaves the queue, and
  a later answer grants nothing.
- After a deny, or a prompt that expires unanswered (operator decision
  2026-10-28), the same executable identity gets a 10-second cooldown for that
  workspace, and a requester not waiting at that moment learns the deny or the
  expiry once on its next call. During it, new requests fail at once with the private
  `rate_limited` error, which the CLI and MCP report as the retryable
  `resource_limit`. This keeps an agent that retries in a loop from reopening
  the prompt.
- Before committing an answer, the daemon validates again: the request is
  still pending; the requester process is alive with the same identity; the
  workspace still exists; the scopes are still valid; the chosen duration is
  still offered; for Always, the durable identity still qualifies; no upgrade
  fence is active; and, for Always, the durable write has succeeded.
- Pending requests are never carried across a daemon upgrade or restart.

If no trusted graphical session can show the prompt, the request fails closed
immediately, as today. (Amended by ADR 0015: a connected remote Window or
terminal approver can also show it; see the amendment below.)

### Authorization and composition

Policy rules, Just-this-once grants, While-this-workspace-is-open grants, and
Always grants are independent sources. A request is authorized when any one
source covers every scope it needs. Scopes are not combined across sources.
Controller ownership, operation preconditions, resource checks, and protocol
limits apply unchanged.

Each duration keeps its own definitions. A short answer never widens a longer
grant. Granting Always for the same identity and workspace replaces the earlier
Always definition with exactly what the new prompt showed. Repeating a While
open grant replaces that definition in the same way. The Just-this-once grant
keeps its current behavior. A request already covered by an existing grant is
answered without a prompt.

### The remembered-grant store

Remembered grants live in a daemon-owned store, separate from `policy.json`.
The policy file stays the expert and headless path, and is unchanged.

**Location and contents.**

- The store is `$XDG_STATE_HOME/splinterm/agent-access.json` (by default
  `~/.local/state/splinterm/agent-access.json`). It sits beside the session
  state its Lair IDs refer to. `splinterm reset` moves it to the same backup as
  the session state. The private format is versioned and is not a public
  compatibility surface.
- It holds at most 256 entries and 256 KiB. Each entry holds a stable entry ID,
  the identity kind and path, the Lair ID, the level or Custom scope set, the
  creation time, the last-used time, and the host-reported name as display data.
  It holds no secret, token, digest, or terminal data.

**File checks.**

- The store follows the session-state rules. The state base,
  `$XDG_STATE_HOME` or `~/.local/state`, is used as given and may itself pass
  through symbolic links, so a home under `/home -> /var/home` works.
- Below the base, the `splinterm` directory and the store file must not be
  symbolic links. The directory must be owned by the daemon UID with mode
  `0700`.
- The store must be a regular file with one hard link, owned by the daemon UID
  with mode `0600`, and within the size bound.

**Writes.**

- One lock serializes every read-modify-write of the store together with the
  matching in-memory change.
- Before each write, the daemon checks that the file still has the device,
  inode, size, and modification time it last wrote or loaded. If anything else
  changed the file, the daemon refuses the write and reports the conflict.
- Writes are atomic. The daemon writes a new `0600` file in the same directory,
  syncs it, renames it over the store, and syncs the directory. Only then does
  it change memory or report success.
- A failed write grants nothing for that answer, not even a downgraded
  Just-this-once grant. The agent receives an error, and the person sees the
  failure.

**Failures and versions.**

- A missing store means no remembered grants.
- A store that is unsafe, corrupt, oversized, or of an unsupported version makes
  the remembered source deny everything. That failure never affects policy or
  the in-memory grants. The daemon reports the failure and offers no Always
  choice until the store is repaired.
- The daemon never silently overwrites a rejected store. Recovery is an
  explicit trusted action that moves the file aside to a reported backup.
- A daemon generation writes only the format version it loaded. A newer
  generation that loads an older version keeps writing that older version, so a
  rollback can still read it. An older generation that finds a newer version
  treats remembered access as unavailable, never rewrites the file, and offers
  no Always choice until a generation that supports the version runs again.

**Last use and missing workspaces.**

- Last-used time is updated in memory on every authorized request and written
  at most once an hour per entry and at clean shutdown. It is display data and
  never extends or ends a grant.
- An entry whose Lair is absent when the store is loaded authorizes nothing. It
  is listed as "workspace missing" until it is revoked.
- Entries for a Lair are removed when the daemon observes that workspace
  ending.

The store is only as safe as the account. Any process running as the user can
edit it, as it can edit `policy.json`; no key stored under the same account
would change that. This is the same limit as the rest of this ADR.

### Access list and revocation

A daemon-wide, bounded inventory, shown as **Agent access** in the command
palette and through a human CLI command, lists every grant definition and its
active use:

- every Always, While open, and Just-this-once grant, with the agent's display
  name, the executable path, the identity kind (protected install, Nix profile,
  exact build, or process), the workspace, the level or Custom scopes, the
  duration, the creation time, and, for Always, the last-used time;
- the active use of each grant: connected processes, controllers it
  authorized, and pending requests; and
- policy rules that match a connected agent, marked as coming from the policy
  file and read-only here.

Remembered entry IDs and runtime grant IDs are distinct, and the list never
confuses the two. The inventory is available to the trusted UI and the human
CLI. A machine client needs `authorization_inspect` or `authorization_revoke`
from the policy file; no level includes them.

**Revoking one entry.**

- Revoke stops new requests at once, cancels the grant's subscriptions, and
  releases the controllers it authorized.
- For an Always entry, the daemon also removes it durably. If that write fails,
  the grant stays revoked in memory, and the daemon retries the removal on the
  next store write and at shutdown. Until a removal succeeds, the list says the
  grant will return after a restart.

**Revoke all access.**

- Before acting, it shows how many remembered grants it will remove. It acts
  only when the person confirms.
- It revokes every consent-derived grant of every duration and cancels every
  pending request. It never touches policy rules or the trusted UI.

**Policy rules.** Revoking a remembered grant does not remove a `policy.json`
rule that matches the same agent. When such a rule exists, the list says that
access continues under the policy file, names the rule ID, and explains that the
rule must be edited in the policy file.

**Explorer.** The Explorer marks workspaces an agent can reach, and distinguishes
them from workspaces an agent is connected to now.

### Agent status

`agent_status_report` is in every level, so a granted agent can show its status
for any Splint in the workspace. Reporting uses the ordinary automation
transport and role. There is no separate reporting role, and the handoff
continuation channel is never used. Self-reports from inside a Splint still
need no grant. A grant is identity-wide: a While open or Always grant authorizes
every connection whose process matches its identity, so a status report on a
fresh connection is covered without asking again. Whether an adapter reuses one
socket is an implementation choice. A status report is display data: it never
creates, extends, or confirms a grant.

### Upgrade and handoff

Remembered definitions are durable user state, not handoff state. They are not
written into the handoff checkpoint. An adopted or rolled-back generation loads
the store fresh from disk, with the same checks, before admission reopens. A
generation that does not support the store's version treats it as unavailable
and never rewrites it. Old connections, controllers, Just-this-once grants,
While open grants, and pending requests all end at the fence, as ADR 0011
already requires. An agent reconnects, its new connection is checked against
the remembered identities again, and it continues without a prompt only if it
still matches. Loading a definition is not restoring an old session, and the
resume ticket never restores an agent's controller.

## Consequences

- An agent the person trusts with a workspace can keep working across its own
  restarts, daemon restarts, and package upgrades after one deliberate answer.
- The person can see every grant, its last use, and its active connections, and
  can revoke each one.
- The policy file keeps its exact-digest semantics and remains the only path for
  headless, remote, daemon-level, and administrative access.
- Remembered access depends on the installation model. Distribution packages and
  multi-user Nix qualify; development builds, user-writable installs, and known
  interpreters get the shorter durations only.
- The Nix profile kind accepts a documented residual risk: same-user software
  can build any program into the store and repoint the profile link at it.
- Look and type and Manage can take control from the person's Window, and a
  remembered grant at those levels keeps that ability across restarts.
- The access dialog's keyboard behavior changes from Enter-grants to
  default-deny.
- A prompt can outlast one MCP call, so the consent exchange gains a wait
  budget, a pending state, and typed decisions instead of a yes/no reply.
- `splinterm reset` will also move the remembered-grant store. The reset text in
  [usage](../usage.md) and [headless operation](../headless.md), which says
  reset leaves policy and configuration untouched, must be updated when this is
  implemented.

## Validation requirements

Implementation must proceed through bounded milestones. At minimum, retained
non-graphical evidence must prove:

- each level expands to exactly the table above in consent scopes; an exact
  bundle is named and any other set is Custom; nothing is rounded up; clipboard
  scopes are rejected as invalid;
- a Custom request containing `control_takeover` is offered Just this once only;
- every duration has the identity and lifetime above, including workspace end,
  daemon generation change, agent restart, and executable replacement;
- the offered durations follow the table above for peers without a pidfd, the
  relay, known interpreters, development builds, and qualifying installs;
- the protected-install check rejects symbolic links, writable or
  non-root-owned components, write ACLs, device or inode mismatch, and a
  deleted executing file;
- the Nix resolver rejects paths outside the closed list, link chains longer
  than 40, paths or link text longer than 4096 bytes, targets outside
  `/nix/store`, malformed store object names, non-root-owned or writable `/`,
  `/nix`, or store paths, wrapper mismatches, and device or inode mismatch;
  resolves relative targets and `..` physically; prefers earlier list entries;
  and follows a repointed profile as documented;
- the store rejects unsafe files, corrupt and oversized content, and unknown
  versions by denying only the remembered source; refuses a write after an
  outside change; writes only its loaded version; a failed write grants
  nothing; success is never reported before the durable write;
- the prompt takes no keyboard focus, ignores every key until a click inside
  it or the exact dialog shortcut, which engages it without answering,
  ignores other Ctrl, Alt, and Super chords, denies on Enter, Escape, and `D`,
  ignores input before arming, after each focus gain and duration change, and
  all key repeats, changes duration only by pointer or arrow keys, and grants
  only on `G` or the grant button;
- the dialog shortcut's Hyprland binding is exact and non-consuming, is
  enabled only while a dialog waits, focuses only a trusted dialog, is
  disabled on every close path including a killed dialog and never removed,
  never touches an expired binding object, drops stale entries, and is never
  enabled over the person's own binding of the chord in any submap;
- pending requests honor their bounds, wait budget, queue, and deny cooldown,
  join on repeat, and grant nothing after expiry, cancellation, requester exit,
  workspace end, Revoke all access, or an upgrade fence;
- revocation releases subscriptions and controllers, retries a failed durable
  removal as documented, and leaves matching policy rules in place; and
- after an upgrade, remembered definitions reload fresh while connections,
  controllers, shorter grants, and pending requests end.

The trusted prompt, the access list, and the Explorer marks also require one
separately approved graphical acceptance session.

## Amendment: remote consent (ADR 0015)

[ADR 0015](0015-remote-consent.md), accepted for `0.2.0`, offers a workspace
access prompt to every eligible remote Window and `splinterm approvals`
terminal approver as well as to the local dialog, so a request fails with
`consent_unavailable` only when none of them can show it. Every duration,
Always included, may be answered remotely, and the answer passes the same
re-checks before it commits. The prompt also counts down and names the
computer the agent runs on. The rule that a request through `splinterm-relay`
is never offered While this workspace is open or Always, the remembered-grant
store, and the policy file are unchanged.
