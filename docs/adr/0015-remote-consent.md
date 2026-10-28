# ADR 0015: agent access prompts in remote Windows and the terminal approver

- **Status:** Proposed (Plan 0066). The protocol contract and the daemon's
  routing are on `main`: the daemon offers workspace prompts to subscribed
  responders and commits their answers. No Window or `splinterm approvals`
  subscribes yet.
- **Date:** 2026-10-27
- **Amends:** [ADR 0005](0005-trusted-consent-broker.md) (where the trusted
  prompt is shown), [ADR 0010](0010-one-authentication-graphical-relay.md)
  (what a relay connection may answer), and
  [ADR 0014](0014-easy-agent-access.md) (a prompt nobody local can show no
  longer fails at once). The amendment notes in those ADRs follow when this
  one is accepted.

## Context

An agent asks for workspace access with `RequestLairAccess`, and the daemon
shows the trusted consent prompt by launching `splinterm consent` in the
graphical session its environment names (ADR 0005, ADR 0014). A daemon host
without a graphical session, such as a server reached only over SSH, has no
place to show it. The request fails at once with `consent_unavailable`, even
while the person is looking at a Splinterm Window attached to that daemon
through the SSH graphical relay (ADR 0010). That Window, not the headless host,
is where the person expects the prompt.

A person who is logged in to the daemon host only through a terminal has the
same problem with no Window at all.

## Decision

### Responders and fan-out

A daemon offers each workspace access prompt to every route that can show it,
at the same time:

- the local dialog, when the daemon has a graphical session, exactly as today;
- every eligible remote Window: a Window registers its relay connection as a
  consent responder with `SubscribeConsentOffers { kind: window }`;
- every eligible terminal approver: the `splinterm approvals` command, run by
  the person in their own terminal on the daemon host, registers with
  `SubscribeConsentOffers { kind: terminal }`.

Every eligible responder gets the prompt. A daemon holds at most 16
responders, one per connection and at most 2 per process.

Each responder connection gets its own offer, with its own offer ID and nonce.
The first valid answer from any route consumes the request. The daemon then
withdraws every other offer (`ConsentOfferWithdrawn` with reason
`answered_elsewhere`) and kills and reaps the local dialog if it is still
showing. A route that fails or disconnects drops out, and the others keep
their offers; a failed local dialog never diverts anything, because every
route had the prompt from the start. With no route at all, the request fails
at once with `consent_unavailable`, as today.

A remote Window shows the prompt in its own dialog child, which keeps every
rule of the trusted dialog (a separate process, never focused on map, inert
keys until engaged, an arming delay, default deny).

### Origin

The process that launches the dialog sets the prompt's origin, never the
offer. The daemon's own prompts are `local`. A remote Window sets the `remote`
kind and its own SSH profile name from its endpoint, and adds the remote
daemon's self-reported `Hello` hostname for display only. A remote daemon can
therefore claim any hostname, but it can never produce a local origin. A
Window that cannot build a valid remote origin skips or denies the offer; it
never falls back to local.

### Untrusted offer strings

Every string an offer carries (the requester path, the agent name, the
workspace name, each shared-identity path) is chosen by an agent, its host, or
a remote daemon, and is untrusted. The protocol bounds its length only. Every
responder, the dialog and the terminal approver alike, neutralises C0 and C1
controls, ESC, and bidi and invisible formatting characters before display,
exactly as the consent dialog and the **Agent access** list escape them
today, and quotes names so they cannot pass for prompt text.

### Binding

An offer (`ConsentOffered`) carries the daemon generation, an offer ID that is
never reused, the agent-visible request ID, a fresh 32-byte random nonce, the
seconds left to answer, and the prompt content without the dialog capability,
the origin, or the countdown. An answer (`AnswerConsentOffer`) is accepted
only on the connection that offer was sent to, only while that offer is
outstanding, and only when the generation and nonce match; a grant must name a
duration the offered prompt offers. Everything else fails with `NotFound` and
grants nothing, with two exceptions that grant nothing either: a grant naming
a duration the prompt does not offer fails with `InvalidArgument` and leaves
the offer open for another answer, and an answer on a connection whose process
no longer has the identity it subscribed with fails with `Unauthorized`. Once
the request has ended (answered, cancelled, expired, or fenced), its offers
answer `NotFound` even before their withdrawal arrives. A valid answer commits through the same re-checks as a local
answer: the request is still pending, the requester is alive with the same
identity, the workspace exists, and no handoff fence is up. The audit records
the subscription and the answer, and which route answered.

Offers and responders live in memory only. Subscribing and answering are
fenced during a daemon handoff (`HandoffInProgress`). The fence ends the
request and withdraws its offers (`fenced`); an answer then commits nothing.
The fence leaves responders subscribed: a handoff that completes ends their
connections, and they subscribe again after reconnecting to the new daemon,
while a cancelled handoff leaves them in place.

### Eligibility

- **Window responders:** a `RemoteInteractive` connection from the installed
  `splinterm-relay --graphical-stdio`, whose relay process is outside every
  live Splint's session and process ancestry and outside the requester's,
  and is still the process that subscribed (same start time) when it answers.
- **Terminal approvers:** a trusted-UI connection from the installed
  `splinterm` binary (the device and inode rule of the trusted-UI identity),
  with a real controlling terminal on standard input and output, outside every
  live Splint's session and ancestry and outside the requester's.
- "Outside the requester's" covers the requester's session too: a responder
  that descends from the requester's session leader is refused even after
  leaving that session with `setsid`.
- When the daemon places workloads in systemd units, a responder whose
  `/proc/<pid>/cgroup` path lies inside any live Splint scope or Dojo slice is
  refused, however it left the process tree. An unreadable or unparsable
  cgroup file refuses the responder. Without workload units the check is
  skipped.
- Automation connections are never responders. Responder status belongs to
  one connection and is never inherited by another channel or process.

### Scope

Only workspace (Lair) access prompts go to responders. Splint access requests
and forced control takeover keep their local dialog only.

### Durations

A prompt answered in a remote Window or by the terminal approver offers the
same durations a local prompt would, including "Always for this workspace".
The existing withholding rules (relay callers, interpreters, executables
without a durable identity, custom takeover) still apply. This is an operator
decision.

### Protocol

The private protocol moves to version 45:

- requests `SubscribeConsentOffers { kind }` and
  `AnswerConsentOffer { daemon_generation, offer_id, nonce, decision }`;
- responses `ConsentOffersSubscribed { subscription_id }` and
  `ConsentOfferAnswered`;
- subscription events `ConsentOffered { offer }` and
  `ConsentOfferWithdrawn { offer_id, reason }`, the reason one of
  `cancelled`, `expired`, `request_ended`, `fenced`, or
  `answered_elsewhere`;
- the dialog prompt gains `respond_within_seconds` (at most the 120-second
  pending-request lifetime; zero shows no countdown) and `origin` (`local`, or
  `remote` with a profile and an optional hostname), both defaulted so older
  prompts decode unchanged;
- audit operations `subscribe_consent_offers` and `answer_consent_offer`. A
  withdrawal is not audited on its own: it always follows an audited event
  (a cancel, an expiry, or the winning answer).

There is no Hello capability bit: negotiation is exact-version, and support is
opt-in by subscribing. MCP gains no tools or error codes.

## Consequences

- A headless daemon host can grant workspace access from the Window the
  person is using, or from their own terminal on that host.
- A desktop daemon host still shows its local dialog; connected remote Windows
  and approvers get the same prompt in parallel.
- No relay connection gains trusted-UI identity, and no public surface (MCP or
  the machine CLI) answers a prompt.
- **Residual risk.**
  - Stock tools leave a process tree without any special privilege: a
    `tmux` or `screen` server, `setsid -f`, or a double fork reparents a
    process to init or a subreaper and gives it a new session. The
    requester-session rule catches these for descendants of the requester's
    session, and the cgroup check catches them for any process still in a
    Splint scope or Dojo slice, which is where such tools leave it when
    workload units are active.
  - What remains is a same-user process that leaves both: an agent that starts
    the adapter itself under `setsid` outside its Splint's cgroup (with
    `systemd-run --user`, through `ssh localhost`, or on a daemon without
    workload units). It can register as a responder, see every workspace
    prompt, and answer it, Always included.
  - Any holder of the user's SSH credentials for the daemon host, including an
    agent on another host using the user's key, gets an sshd-spawned relay
    that passes the process-tree check, and can likewise see and answer every
    prompt, Always included. The net change from today is none: that key
    already gives a shell on the daemon host.
  - Either can already edit `policy.json` or the remembered-grant store, so
    this is the same class of risk, not a new one. Consent bounds cooperating
    agents; it is not a sandbox (ADR 0014).
  - A compromised relay or remote account can forge answers and show
    remote-labelled dialogs, but it already controls that account and
    everything its Windows show.
  - Some residuals cost availability only, never a grant. A forged responder
    can answer Deny, which ends the request and starts the deny cooldown. A
    responder that holds an offer unread keeps a headless prompt open until it
    expires. The caps of 2 responders per process and 16 per daemon can be
    filled, refusing later responders until one disconnects.
  - The requester-session rule refuses a legitimate responder whose own
    ancestry passes through the requester's session leader, for example when
    the agent runs as a session leader high in the person's own login tree.
- Every host connected by a remote Window must run protocol 45 together.
