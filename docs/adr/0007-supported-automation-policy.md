# ADR 0007: supported automation policy and audit boundary

- **Status:** Accepted; hierarchy, selectors, and public schema amended by [ADR 0009](0009-topology-lair-dojo-migration.md); agent status reporting added by the protocol 38 amendment below; remembered consent grants added beside policy by [ADR 0014](0014-easy-agent-access.md) (amendment below)
- **Date:** 2026-07-23
- **Supersedes:** the persistent-policy deferral in [ADR 0005](0005-trusted-consent-broker.md)

## Context

Protocol v18 authenticates local Unix-socket peers and has scoped, incarnation-
bound grant-once consent for terminal access. It does not authorize topology
reads, process creation, restore, layout mutation, or audit inspection. The
current executable device/inode binding is suitable for one daemon lifetime but
not for persistent policy: replacement changes the inode, a path alone can be
replaced, and a basename, argv, client label, or same UID is not an identity.

Headless hosts cannot display the trusted consent client. Remote clients also
cannot be identified through a stdio relay: `splinterd` sees the local relay
process. Supported automation therefore needs an explicit persistent policy,
an exhaustive operation matrix, and inspectable audit metadata without turning
transport authentication into authorization.

## Decision

### Canonical executable identity

A persistent rule identifies one executable with both:

- an absolute, normalized path with no `.` or `..` components; and
- the SHA-256 digest of the exact regular executable file opened for the peer.

At connection acceptance the daemon obtains `SO_PEERCRED` and the Linux 6.5+
`SO_PEERPIDFD`, opens the peer's `/proc/<pid>/exe` target read-only and
close-on-exec, and derives path, device, inode, owner, mode, size, and digest from
that open file. It rejects a missing, non-regular, oversized, unreadable, or
changed-while-hashing executable. The open descriptor, not a later path lookup,
is authoritative.

Adversarial executable-identity validation established that the pidfd must
identify the same PID, remain alive across the bounded hash, and be monitored
for the connection lifetime. Peer exit closes the connection even
if its socket descriptor was passed elsewhere. Persistent-policy authorization
fails closed when `SO_PEERPIDFD` is unavailable; it never falls back to a reused
numeric PID. Hashing runs outside daemon locks on a bounded blocking worker.

Path and digest must both match. Device/inode remain useful connection audit
metadata but are not persisted as upgrade-stable identity. Basename, argv,
process title, environment, client-provided labels, parent process, and same UID
never contribute authority.

Package or executable replacement does not inherit authority. A newly opened
binary with a new digest is denied until the user explicitly updates and
validates the rule. Existing connections retain their accepted identity
snapshot, but every request is evaluated against the current policy generation;
removing the old digest revokes its subscriptions and controllers. A policy may
list a small bounded set of explicit digests during a staged upgrade. Wildcards,
directory trust, signer inheritance, and “current file at this path” matching
are excluded from v1.

This boundary reduces ambient authority; it cannot contain an already
compromised account. A malicious process with the user's full filesystem and
service-control authority can replace an owner-writable policy or arrange a
daemon restart. Operators needing protection from the service account itself
must provision a policy the account cannot modify using an external control
such as root ownership or MAC. Documentation must state this residual risk.

### Policy file and matching

The policy is a bounded, versioned document loaded from an explicitly configured
path. No policy file means no persistent third-party grants. The daemon opens
each path component without following symlinks and accepts only a regular file
owned by the daemon UID, mode `0600`, no hard links, and at most 256 KiB. A
separately administrator-provisioned read-only file may be root-owned, must not
be group/world writable, and follows the same no-symlink and size rules.

Rules contain a unique bounded ID, canonical executable identity, expiry,
closed operation scopes, exact resource selectors, and operation-specific
limits. The default scope is `topology_metadata_read`; no wildcard scope exists.
Exact Splint selectors may bind an incarnation or explicitly select the current
incarnation. When a policy generation is published, Dojo selectors resolve to
the selected Dojo plus its then-present windows and Splints, and window selectors
resolve to the selected window plus its then-present Splints. Resolution is
deduplicated and capped at 512 resources per rule. Missing selectors or an
oversized expansion reject the candidate generation and install deny-all.
Resources created afterward are excluded until an explicit policy update or
reload publishes a new snapshot. A snapshotted parent follows the same stable
Splint ID under the conspicuous `current` incarnation semantics; a directly
configured exact incarnation remains exact. Future-descendant authority is
deferred beyond policy v1.

Limits include, where applicable, maximum returned rows/results/bytes, maximum
live subscriptions, maximum spawn count, and deadline. Request protocol bounds
remain hard ceilings. A rule can only narrow them.

Reload parses and validates a complete candidate before publication. Invalid,
unsafe, expired, unreadable, or oversized policy produces a bounded diagnostic
and installs a deny-all persistent-policy generation; it never retains a
possibly broader old interpretation. Publication is atomic. Requests already
executing keep the authorization decision and limits captured at start; reload
cannot broaden them. Removal or narrowing cancels now-unauthorized
subscriptions, revokes affected controllers, and denies later requests.

Policy validation and reload are local owner administration actions, not an
ordinary daemon RPC. The CLI may validate a file offline and request service
reload through the service manager, but it does not rewrite policy or bypass
normal filesystem controls.

### Consent, first-party UI, and relay

Graphical grant-once consent remains available and daemon-lifetime-only. If the
trusted consent surface is unavailable, a request without a matching policy
fails closed. Headless operation never enables the development bypass.

The matching packaged `splinterm` UI retains only the narrow implicit behavior
accepted in ADR 0005. Protocol v18 also requires that connection to declare the
`trusted_ui` handshake role. JSON/NDJSON connections declare `automation`, so
sharing the same executable inode cannot activate the graphical bypass. Public
automation modes, editor clients, relays, and MCP adapters are third parties even
when shipped together and require policy or interactive consent as the matrix
allows.

`splinterm relay --stdio` is authorized as its own exact executable identity.
SSH authenticates the host and login, not Splinterm operations. Because the
daemon sees the relay rather than the remote origin, a relay rule delegates its
listed resources, scopes, and limits to every caller able to invoke that exact
relay under the account. The relay cannot mint, forward, or claim a remote
identity. This delegation must be conspicuous in validation output and remote
access documentation.

### Closed operation scopes

Policy v1 uses these closed scopes:

- `topology_metadata_read` and `topology_subscribe`;
- `terminal_visible_read`, `terminal_subscribe`, `scrollback_read`, and
  `scrollback_search`;
- `controller_acquire`, `controller_transfer`, `input`, and `resize`;
- `process_spawn`, `process_restore`, and `process_terminate`;
- `topology_layout_mutate` and `topology_name_mutate`;
- `authorization_inspect`, `authorization_revoke`, and `audit_inspect`;
- `agent_status_report` (protocol 38 amendment).

Clipboard scopes remain reserved internal consent scopes until a daemon
clipboard operation exists. Forced controller takeover is trusted-UI-only in
policy v1 and cannot be granted persistently. Policy administration is not a
socket scope. New requests and new sensitive behavior default to unauthorized
until this ADR's matrix is revised.

### Protocol v18 operation matrix

The public CLI is a composition of these protocol operations. Commands that need
parent IDs or a topology CAS first issue `InspectTopology`, so their CLI policy
rules also require `topology_metadata_read` with a Lair resource selector.
Terminal/control subscriptions use `InspectSplint` and require
`topology_metadata_read` for that Splint. These compound prerequisites are
intentional and visible rather than inherited from the trusted-UI bypass.

`own connection` means an unforgeable daemon-owned subscription, transfer, or
controller identifier created for that connection. Resource selectors are
checked in addition to every listed scope.

| v18 request | Required policy scope or authority |
| --- | --- |
| `Ping` | authenticated local peer; no resource data |
| `ReadGraphicalFocus` | authenticated local peer; narrow nullable Splint ID/CWD projection only |
| `PublishGraphicalFocus` | matching installed trusted graphical client only; no persistent policy scope |
| `ListDojos` | `topology_metadata_read` |
| `InspectTopology` | `topology_metadata_read` |
| `SubscribeTopology` | `topology_subscribe` plus `topology_metadata_read` |
| `InspectSplint` | `topology_metadata_read` for the Splint |
| `RequestAccess` | graphical consent or an exact policy rule for every requested operation scope |
| `AuthorizationStatus` | `authorization_inspect` for the Splint |
| `RevokeAccess` | `authorization_revoke` for the grant's resource |
| `CreateDojo` | `process_spawn` and `topology_layout_mutate`; creation limit |
| `SplitSplint` | `process_spawn` and `topology_layout_mutate` for target; creation limit |
| `RelaunchSplint` | `process_spawn` for the exact Splint |
| `RestoreSplint` | `process_restore` for the exact Splint |
| `RestoreWindow` | `process_restore` for every expanded Splint |
| `RestoreDojo` | `process_restore` for every expanded Splint |
| `CloseSplint` | `topology_layout_mutate`; live process additionally requires `process_terminate` |
| `SetSplitRatio` | `topology_layout_mutate` for target |
| `NewWindow` | `process_spawn` and `topology_layout_mutate` for Dojo; creation limit |
| `CloseWindow` | `topology_layout_mutate` for every expanded Splint; each live process additionally requires `process_terminate` |
| `RenameDojo` | `topology_name_mutate` for Dojo |
| `RenameWindow` | `topology_name_mutate` for window |
| `SetWindowDefaultFocus` | `topology_layout_mutate` for window and Splint |
| `RenameSplint` | `topology_name_mutate` for Splint |
| `Attach` | `terminal_visible_read` and `terminal_subscribe`; also `scrollback_read` when rows are requested |
| `ScrollbackPage` | `terminal_visible_read` and `scrollback_read` |
| `SearchScrollback` | `terminal_visible_read`, `scrollback_read`, and `scrollback_search` |
| `AcquireControl` | `controller_acquire` plus at least one of `input` or `resize` |
| `SubscribeControl` | `terminal_visible_read` for Splint |
| `RequestControlTransfer` | `controller_transfer` plus at least one of `input` or `resize` |
| `DecideControlTransfer` | own pending transfer as current controller; no additional scope |
| `ForceControlTransfer` | trusted graphical UI confirmation only: the daemon's own dialog for the installed client, or (v47) the forcing remote Window's own consent responder, its relay outside every Splint; no persistent policy scope |
| `ReclaimControl` (v47) | installed trusted graphical client or remote Window only, without confirmation; no persistent policy scope or grant; audited as `reclaim_control` |
| `ReleaseControl` | own controller; no additional scope |
| `Input` | own controller and `input` |
| `Resize` | own controller and `resize` |
| `Detach` | own subscription; no additional scope |
| `KillSplint` | `process_terminate` for exact Splint/incarnation |
| `ResolveLaunchDirectory` (v38) | `process_spawn` and `topology_layout_mutate` for the source Splint; it prepares a creation and its answer reveals daemon-host path availability; audited as `resolve_launch_directory` |
| `AuditInspect` | `audit_inspect` for daemon-lifetime bounded metadata |
| `ReportAgentStatus` (protocol 38) | a process in the target Splint's live session or descended from its root process, with no grant; otherwise `agent_status_report` for the Splint from policy or a grant; the trusted UI may only clear |

Audit inspection is paginated and requires `audit_inspect`.
Cancellation is connection-local and needs no separate scope, but can cancel
only the caller's request or subscription.

### Audit contract and retention

The daemon assigns monotonic, nonzero audit IDs for its lifetime and retains the
newest 1,024 records in memory. Inspection is cursor-based and page-bounded; a
cursor older than retention returns an explicit gap and the oldest available
ID. IDs and records restart with the daemon and the API is labeled
`daemon_lifetime`. Durable audit is deferred until retention, rotation, crash
recovery, and privacy can be evaluated independently.

Records contain timestamp, audit ID, policy generation/rule ID when applicable,
peer UID, bounded executable path and digest, operation, resource IDs and
incarnation, requested scopes, decision, bounded symbolic reason, and bounded
outcome metadata. They never contain terminal rows or bytes, scrollback or
clipboard bodies, input bytes, capability tokens, environment contents, search
queries, or complete command arguments. Spawn/restore records may contain an
argument count and redacted executable basename only.

Record authorization grants, denials, revocations, expiry, policy match/reject,
controller transfer, spawn/restore, topology mutation, termination, and policy
reload outcome. Audit insertion is ordered by the daemon and occurs without
holding policy, topology, PTY, protocol-writer, or consent locks. Audit loss or
inspection backpressure cannot block PTY consumption; a retention gap is
reported rather than hidden.

### Amendment: explicitly reported agent status (protocol 38)

`ReportAgentStatus { splint_id, incarnation, status }` sets or clears one
display status for one live Splint incarnation. The state is one of `working`,
`needs_input`, `done`, `error`, or `idle`; an optional detail is at most 80
Unicode scalars and 240 bytes, and control, bidi-formatting, and line-break
characters are rejected rather than stripped. Nothing is inferred from terminal
content, titles, OSC sequences, process names, or output. A status is display
data: it grants no access, confirms no consent, sends no input, and changes no
topology.

Authority, evaluated in this order:

1. **Self-report.** The peer is accepted without a grant when its connection has
   a pidfd-monitored persistent identity, its `SO_PEERCRED` PID still has the
   start time recorded when the connection was verified, and that process is
   in the incarnation's live PTY session (the session leader is the Splint's
   live root process), else a parent walk of at most 64 `/proc/<pid>/stat`
   steps reaches that root process. Each ancestor must have started no later
   than its child, so a PID reused during the walk ends it. A self-reporter can
   only report for the Splint it runs in. Cgroup membership is never evidence:
   any process of the same user can join the Splint's scope or create one with
   the same name. A fully detached agent, one that called `setsid` and was
   reparented to init or `systemd --user` (for example a tmux server started
   inside the Splint), is outside by this rule and needs a grant.
2. **Granted report.** Any other peer needs `agent_status_report` for the Splint
   from an exact persistent policy rule, a Lair grant, or a Splint grant. A
   Splint selector resolves to the current incarnation; a report naming another
   incarnation then fails as stale.
3. **Trusted UI.** The installed trusted UI may only clear a status.

A report for an incarnation other than the live one fails with
`stale_incarnation`. Admission is limited to 10 reports per second per Splint;
excess reports fail with the retryable private `rate_limited` error, which the
CLI and MCP surfaces report as their retryable `resource_limit`.

Granted reports, and reports refused by authorization, are audited as operation
`agent_status_report` with decision and outcome only. Self-reports are not
audited per update, so heartbeats cannot flood bounded audit. Nor are a
self-reporter's refusals for a stale incarnation or invalid detail; its rate
refusals are audited once per incarnation, and later ones are not. The detail
text is never audited, logged, or traced.

The status lives only in daemon memory, keyed by Splint and bound to one
incarnation. It is never persisted, never carried through handoff, and never
bumps the topology revision. The transaction that publishes an exit,
relaunch, restore, close, or termination drops the retired incarnation's
status. Topology snapshots carry it in `SplintRuntimeSummary.agent_status`, and
changes are delivered on topology subscriptions as `AgentStatusChanged`,
coalesced to at most one event per Splint every 250 ms. `working` and
`needs_input` expire after their TTL (default 120 s, bounds 10 to 900 s) and are
then marked stale; a stale status is dropped after 15 more minutes. Reading the
status needs no new scope: it follows `topology_metadata_read`.

### Amendment: remembered consent grants (ADR 0014)

[ADR 0014](0014-easy-agent-access.md), accepted for `0.2.0` and implemented on
`main`, adds a second persistent authority source beside the policy file:
grants the person answers with "Always for this workspace" in the trusted
consent window, kept in a daemon-owned store separate from `policy.json`. It
also adds a daemon-lifetime "While this workspace is open" grant keyed by the
canonical path and digest identity defined above.

The policy file is unchanged. Rules still require path and digest. Package
replacement still inherits no policy authority. Policy selectors keep their
snapshot semantics, and policy reload neither reads nor changes remembered
grants. The exclusion of "current file at this path" matching above applies to
policy rules and stays in force for them.

Remembered grants are the one, narrowly scoped exception. They may name a
durable identity instead of a digest, so that they survive agent upgrades, and
they accept exactly two kinds:

- a **protected install**: every path component root-owned, not group- or
  world-writable, without symbolic links or write ACLs, with the file at the
  path having the executing file's device and inode; and
- a **Nix profile entry** from a closed list, resolved one link at a time on
  every connection to a root-owned, unwritable regular file in `/nix/store`
  with the executing file's device and inode.

Both are checked again on every new connection. Some executables cannot be
remembered and are offered only the shorter durations:

- development builds and other user-writable executables;
- known interpreters, decided by a fixed basename list as a user-experience
  rule, not a security boundary; and
- the installed relay.

These checks protect the executable file, not how it runs: environment such as
`LD_PRELOAD` and user-writable configuration the program loads still affect it.

**Residual risk.** A Nix profile link is user-controlled. Any software running
as the user can ask the multi-user Nix daemon to build any program into
`/nix/store`, then repoint the profile at it, and the next process started from
that path matches the remembered grant. Root ownership of a store path means
the Nix daemon wrote it, not that anyone vetted it.

The remembered-grant store is also a file the account can edit, like the policy
file. Both risks belong to the compromised-account limit stated above.
Remembered grants bound cooperating agents and make their access visible; they
do not contain a hostile process running as the account. Documentation must
state both.

**Takeover.** "Forced controller takeover is trusted-UI-only in policy v1 and
cannot be granted persistently" in the closed-scope section above now applies
only to policy rules.

- Interactive Lair consent can already grant `control_takeover` for five
  minutes.
- Under ADR 0014, the Look and type and Manage levels include it, so a
  remembered or workspace grant at those levels can take control of a Splint the
  person's Window controls.
- A Custom request that contains `control_takeover` is offered just this once
  only.
- The trusted UI's own forced takeover with confirmation is unchanged.
- A Window also takes control when a person resizes the pane, without
  confirmation; see the next amendment.

**Consent.** "Graphical grant-once consent remains available and
daemon-lifetime-only" in the consent section above is amended accordingly.
Just-this-once grants keep that behavior. The longer durations follow ADR 0014.

### Amendment: a resize takes control (protocol 47)

Operator decision, 2026-10-30. When a person resizes a pane in a Window that
is not its controller, that Window takes control with `ReclaimControl`. There
is no dialog, and it takes the pane from any holder: another Window, a remote
Window, or an agent, with or without a grant. For resizes this replaces the
rule that a Window does not take a pane from an agent without the person's
answer.

- Only the installed client and remote Windows may send it. No policy scope
  names it, so no policy rule, grant, or development access gives an agent or
  any other automation connection the right to send it. A remote Window's
  relay connection must also run outside every Splint (ADR 0010's amendment).
- The lease it takes is never bound to a grant. A pending transfer of the
  Splint is cancelled. A connection still holds one lease.
- The connection that lost the lease gets a control status with
  `lost_reason: reclaimed_by_window`, and its next input, resize, or release
  is refused with the same reason. Nobody is told which Window took it.
- Each reclaim is audited as `reclaim_control`; the reason names who took
  control (a Window or a remote Window) and from whom (nobody, itself, a
  Window, a remote Window, an agent, or a connection that was closing).
- The confirmed forced takeover is unchanged.

Residual risk. These bound a cooperating agent, not a hostile same-user
process (ADR 0014), and each is worth slightly more than before, because a
reclaim has no dialog:

- A same-user process that leaves every Splint's process tree and cgroup
  (`setsid` with `systemd-run --user`, `ssh localhost`, or a daemon without
  workload units) can start a relay that passes the remote Window check.
  ADR 0015 lists the same escape for consent responders.
- A process that inherits a trusted connection's descriptor uses it without
  being identified again.
- A client running the sealed snapshot that an adopted daemon pinned for the
  upgrade handoff is a trusted client.
- Code loaded into the installed client, for example with `LD_PRELOAD`, runs
  as that trusted client.
- Remote holders of the user's SSH key are covered in ADR 0010's amendment.

## Threat-model conclusions

- Copied executables fail the canonical-path check; replaced executables fail the
  digest check. Writable policy/account compromise remains an explicit residual
  risk rather than a false promise.
- Symlink, path-component, hard-link, ownership, mode, size, parse, and race
  failures deny the entire persistent-policy generation.
- Stale rules fail on expiry, missing resources, incarnation mismatch, or policy
  generation change. Selector expansion is bounded and snapshot-based.
- Relay impersonation gains nothing unless it matches the exact relay path and
  digest; callers who can invoke the authorized relay receive its explicitly
  delegated authority by design.
- SSH disconnect, relay EOF, or client death cancels subscriptions and releases
  controllers without killing daemon-owned processes.
- Terminal prompt injection remains untrusted returned data. It cannot approve
  consent, alter policy, create a scope, or become an audit reason.
- Frames, policy, selectors, streams, pages, queues, relay buffers, and audit are
  bounded before allocation. Slow consumers receive resynchronization or are
  disconnected.
- Request IDs, stable resource IDs, incarnation, revisions, ownership, policy
  generation, and cursors prevent replay from silently targeting new state.

## Consequences

- Headless access is possible only after an explicit least-privileged policy is
  installed; missing graphical consent otherwise denies access.
- Package upgrades intentionally require policy digest review instead of
  silently transferring authority to replacement code.
- Relay and adapter identities remain disposable clients with no bypass.
- The first implementation must add scopes and authorization checks before
  exposing machine CLI contracts.
- Audit inspection is useful but intentionally not durable in policy v1.
- The executable snapshot and policy-open algorithms require adversarial tests
  before implementation; failure to make either race-safe is a stop gate.
