# ADR 0010: Multiplex native remote channels over one authenticated SSH process

- **Status:** Accepted and validated — transport, native human-interactive routing, and reviewed real-host graphical closure are complete; which workspace access prompts a relay connection may answer amended by [ADR 0015](0015-remote-consent.md); a remote Window's resize reclaim and confirmed forced takeover amended for protocol 47
- **Date:** 2026-08-07

## Context

A native Splinterm Window uses independent daemon connections for topology,
terminal observation, control, and pane tasks. Opening one SSH process for every
connection would repeat password or passphrase authentication. Depending on
OpenSSH ControlMaster would still consume one server SSH session per daemon
connection and would make supported Window topology depend on `MaxSessions`.

The product goal is the Splinterm equivalent of:

```text
local Foot -> SSH -> remote tmux -> persistent remote sessions
```

but rendered as one local native Splinterm Window attached directly to the
remote daemon-owned Lairs, Dojos, and Splints. `splinterd` must not gain a
network listener, and an SSH caller must not gain trusted-UI identity.

## Decision

Use one directly spawned OpenSSH child per remote-client lifetime. Its fixed
remote command is:

```text
/usr/bin/splinterm relay --graphical-stdio
```

That mode negotiates a separate bounded outer protocol and multiplexes a hard
maximum derived from 256 supported Splints, two retained channels per Splint,
and nine fixed/transient Window service channels (eight before a remote
Window's consent responder took one, ADR 0015). Channel IDs are nonzero,
monotonically allocated, and never reused. Every accepted channel opens
and repeats the existing owner, mode, UID, pidfd, and exact adjacent
`splinterd` executable validation before carrying opaque private-protocol bytes.
The daemon passes as the adjacent file itself or, after a verified live upgrade
(ADR 0011), as a memfd sealed against write, grow, shrink, and further sealing
whose bytes equal that file. The relay holds the one copy it verified and the
file it matched open for its lifetime, so later channels skip the comparison
only while both exact inodes are pinned and the file's length and change time
are unchanged; a replaced or rewritten `splinterd` is compared again.
The daemon sees the installed `splinterm-relay` executable and requires its
exact `--graphical-stdio` mode before accepting `ClientRole::RemoteInteractive`.
OpenSSH-authenticated native Windows therefore receive normal human terminal
authority without automation policy, including taking control by resizing a
pane (amendment below). Raw `--stdio` connections continue to
negotiate `ClientRole::Automation`.

The outer protocol has exact magic and version plus `Hello`, `HelloAck`,
`OpenChannel`, `ChannelOpened`, `ChannelRejected`, `Data`, `HalfClose`,
`CloseChannel`, and `SessionError` frames. Data producers reserve shared and
per-channel byte permits before reading; permits survive through physical write.
A central round-robin scheduler services one frame per ready channel while a
separate bounded control queue prevents data from starving session control.
Per-channel drain barriers preserve data-before-half-close/close ordering. A
client half-close or close may cross a channel-local daemon EOF after the relay
has retired the same monotonically issued ID; those shutdown frames are
idempotent. The symmetric client route also accepts crossed server half-close or
close only for an already-issued retired ID. Data for any retired or unknown
channel, and shutdown frames for a future never-issued ID, remain session-fatal.
The client waits for its outgoing task to queue `HalfClose` before it queues
`CloseChannel`. The relay owns outer input in one dedicated capacity-one reader
task so coordinator events cannot cancel a partially consumed frame; session
cancellation may stop that reader only after the session is already terminal.
Data frames, queues, diagnostics, and channel counts are bounded. Corrupt outer
framing and aggregate-bound violations fail the session; ordinary daemon EOF is
channel-local unless the validated daemon process exits.

The existing command remains byte-transparent and unchanged:

```text
/usr/bin/splinterm relay --stdio
```

`Connection` accepts split async reader/writer transports while preserving its
local Unix constructors, frame/request/event bounds, cancellation behavior, and
trusted local image-content path. A remote connection has no image socket path
and cannot negotiate trusted image retrieval.

OpenSSH owns keys, agents, certificates, hardware providers, passwords,
passphrases, host-key verification, aliases, and supported proxy routing.
Splinterm supplies structured argv, fixed safety overrides, bounded stderr
capture, standard `SSH_ASKPASS` validation, child cleanup, and categorized
errors. It stores no credentials and performs no host-key enrollment.

## Consequences

Password and passphrase users authenticate once for the complete remote session,
not once per pane. Multiple daemon connections remain independent without
consuming additional SSH sessions. Dropping the final session/channel owner
closes stdin, permits a short graceful exit, then terminates and reaps a stuck
child. Remote daemon-owned Splints are not terminated.

Persistent Splinterm policy remains a separate authorization boundary for
machine clients using raw `--stdio`, JSON/NDJSON, or MCP. It is not part of the
native remote Window path, and policy reload does not disconnect human graphical
connections.

The implemented native path provides profiles, inspection, non-mutating
`remote check`, human-interactive channels, remote-safe graphical mutations,
recency namespaces, focus suppression, and complete remote UI routing. New
Lairs, Dojos, and Splints are immediately attachable without policy publication
or Window restart.

## Rejected alternatives

- One SSH process per daemon connection: repeats authentication and scales poorly.
- OpenSSH ControlMaster as the application multiplexer: remains constrained by
  server SSH-channel policy and `MaxSessions`.
- A `splinterd` TCP listener: enlarges the daemon authentication and network
  attack surface.
- A custom SSH implementation or credential cache: duplicates established SSH
  security authority and creates secret-storage obligations.
- Trusted-UI negotiation through SSH: breaks the local executable/compositor
  trust boundary.
- Changing raw `relay --stdio`: risks existing automation compatibility.

## Validation

Validation covers exact raw-relay compatibility, outer framing under
fragmentation and corruption, channel limits, queue backpressure, half-close,
channel/session EOF, daemon pidfd exit, anti-starvation behavior, split transport
handshake/cancellation, no remote image transport, exact fake-SSH argv, one fake
SSH process serving simultaneous daemon connections, child cleanup, strict
profiles, and read-only `remote check`.

## Amendment: remote consent (ADR 0015)

[ADR 0015](0015-remote-consent.md), accepted for `0.2.0`, lets a native remote
Window's relay connection subscribe as a consent responder on one more logical
channel and answer the workspace access prompts the remote daemon offers it.
This is not the rejected "Trusted-UI negotiation through SSH": the relay gains
no trusted-UI identity, and nothing over SSH proves a compositor or a
graphical session. The remote daemon decides which connections are eligible
from its own process checks, and the dialog that shows the prompt is a local
trusted client on the viewer's computer, launched by its Window. The relay's
human-interactive role and the automation policy boundary are unchanged.

## Amendment: a resize takes control (protocol 47)

Operator decision, 2026-10-30. A remote Window's relay connection may take a
pane's controller when a person resizes the pane (`ReclaimControl`), without
a dialog and from any holder, as the installed local client may (ADR 0007).
This belongs to the relay's human-interactive role. It is not trusted-UI
identity, which SSH still never grants: the relay connection still cannot
send the requests kept for the local client, such as daemon handoff. (The
forced takeover, local-only when this amendment was written, is amended
below.)

Running the relay in `--graphical-stdio` mode needs no SSH, so the daemon
checks a reclaiming relay connection as it checks a remote consent responder
(ADR 0015): the installed relay it ran at `Hello`, outside every Splint's
process tree, scope, and Dojo slice. A connection that fails is refused as
unauthorized, and the refusal is audited as denied.

Residual risk. Each of these is worth slightly more than before, because a
reclaim has no dialog:

- Any holder of the user's SSH credentials for the daemon host, including an
  agent on another host using the user's key, gets an sshd-spawned relay that
  passes the check and can take any pane by resizing it. The operator accepted
  remote reclaim. Before protocol 47 that connection could only ask the pane's
  owner for control. The key already gives a shell on the daemon host.
- `ssh localhost` gives a same-user process on the daemon host such a relay
  too, as do the other ways out of the Splint cgroups that ADR 0007's
  amendment lists.

## Amendment: a remote Window forces control (protocol 47)

Operator decision, 2026-10-30 ("Force key in remote Windows: Yes, in
0.2.0"). Forced takeover (`ForceControlTransfer`, Ctrl+Shift+U, Plan 0072
M3) is no longer local-only. A remote Window's relay connection may force
control of a pane another connection holds, with a confirmation the person
answers in that same remote Window.

- The daemon first checks the forcing relay connection as it checks a
  reclaim and a consent responder (ADR 0015): the installed relay it ran at
  `Hello`, outside every Splint's process tree, scope, and Dojo slice.
  Failing that, it is refused as unauthorized, audited as denied, and nobody
  is asked.
- The confirmation goes through the remote-consent route (ADR 0015's
  amendment): only to the consent responder on the forcing connection's own
  relay process, which is that Window's. It is never shown on the daemon
  host's display and never offered to another Window or to the terminal
  approver. A Window without a responder that may show it (remote agent
  prompts turned off, the responder refused or not yet subscribed, or
  already showing another prompt) fails at once with `consent_unavailable`;
  nothing waits and nothing falls back to the local dialog.
- The prompt offers "Just this once" only. A Deny, or a confirmation left
  unanswered for its 20 seconds, ends the force with `consent_denied`; the
  holder keeps the pane.
- A takeover confirmed so is audited with the reason
  `forced_transfer_confirmed_in_remote_window`, and the answer itself as
  `answered_in_remote_window`, as a remote grant is.
- This is still not trusted-UI identity. Automation connections, policy
  rules, and agent grants are unchanged: no policy scope reaches the remote
  path, and an agent's own grant-based force is as before.

Residual risk. A forced takeover now has a remote path, with a dialog:

- Any holder of the user's SSH credentials for the daemon host, including an
  agent on another host using the user's key, gets an sshd-spawned relay
  that passes the check. It could force control and answer its own
  confirmation, since it also controls the responder that shows it. The net
  change from a resize reclaim (above) is none: that connection can already
  take a pane without a dialog, and the key already gives a shell on the
  daemon host.
- `ssh localhost`, and the other ways out of the Splint cgroups that ADR
  0007's amendment lists, give a same-user process on the daemon host such a
  relay and responder too, with the same reach as a reclaim.
- The confirmation can be no stronger than the remote Window's own account:
  a compromised viewer account or relay can forge the answer, as it can for
  a workspace prompt (ADR 0015).
