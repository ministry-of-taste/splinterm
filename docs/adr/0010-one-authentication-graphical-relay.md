# ADR 0010: Multiplex native remote channels over one authenticated SSH process

- **Status:** Accepted and validated — transport, native human-interactive routing, and reviewed real-host graphical closure are complete
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
authority without automation policy. Raw `--stdio` connections continue to
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
