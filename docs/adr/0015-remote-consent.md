# ADR 0015: agent access prompts in remote Windows and the terminal approver

- **Status:** Proposed (Plan 0066). The protocol contract is on `main`; the
  daemon still answers every consent responder request with
  `UnsupportedOperation`, and prompts are routed as before.
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

The first valid answer from any route settles the request. The daemon then
withdraws every other offer (`ConsentOfferWithdrawn` with reason
`answered_elsewhere`) and kills and reaps the local dialog if it is still
showing. A route that fails or disconnects drops out, and the others keep
their offers; a failed local dialog never diverts anything, because every
route had the prompt from the start. With no route at all, the request fails
at once with `consent_unavailable`, as today.

A remote Window shows the prompt in its own dialog child, which keeps every
rule of the trusted dialog (a separate process, never focused on map, inert
keys until engaged, an arming delay, default deny). The Window sets the
prompt's origin itself, from its own endpoint: the SSH profile name and the
daemon's hostname. The daemon never supplies an origin, so a remote daemon
cannot produce a dialog that looks local.

### Binding

An offer (`ConsentOffered`) carries the daemon generation, an offer ID that is
never reused, the agent-visible request ID, a fresh 32-byte random nonce, the
seconds left to answer, and the prompt content without the dialog capability
or origin. An answer (`AnswerConsentOffer`) is accepted only on the connection
the offer was sent on, only while that offer is outstanding, and only when the
generation and nonce match; a grant must name a duration the offered prompt
offers. Everything else fails with `NotFound` and grants nothing. A valid answer
commits through the same re-checks as a local answer: the request is still
pending, the requester is alive with the same identity, the workspace exists,
and no handoff fence is up. The audit records which route answered.

Offers and responders live in memory only. A daemon handoff fences the
request, withdraws its offers (`fenced`), and ends the responders, which
subscribe again on the new daemon.

### Eligibility

- **Window responders:** a `RemoteInteractive` connection from the installed
  `splinterm-relay --graphical-stdio`, whose relay process is outside every
  live Splint's session and process ancestry and outside the requester's.
- **Terminal approvers:** a trusted-UI connection from the installed
  `splinterm` binary (the device and inode rule of the trusted-UI identity),
  with a real controlling terminal on standard input and output, outside every
  live Splint's session and ancestry and outside the requester's.
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

- requests `SubscribeConsentOffers { kind, presence }`,
  `SetConsentPresence { presence }`, and
  `AnswerConsentOffer { daemon_generation, offer_id, nonce, decision }`;
- responses `ConsentOffersSubscribed { subscription_id }` and
  `ConsentOfferAnswered`;
- subscription events `ConsentOffered { offer }` and
  `ConsentOfferWithdrawn { offer_id, reason }`, the reason one of
  `cancelled`, `expired`, `request_ended`, `fenced`, or
  `answered_elsewhere`;
- the dialog prompt gains `respond_within_seconds` (at most the 120-second
  pending-request lifetime; zero shows no countdown) and `origin` (`local`, or
  `remote` with a profile and hostname), both defaulted so older prompts
  decode unchanged;
- audit operations `subscribe_consent_offers`, `answer_consent_offer`, and
  `withdraw_consent_offer`.

A Window's presence (whether it is focused, its active Lair, and at most 32
open Lairs) is display state for the daemon only. It is never an identity or
an authority, and it is never shown to agents. There is no Hello capability
bit: negotiation is exact-version, and support is opt-in by subscribing. MCP
gains no tools or error codes.

## Consequences

- A headless daemon host can grant workspace access from the Window the
  person is using, or from their own terminal on that host.
- A desktop daemon host still shows its local dialog; connected remote Windows
  and approvers get the same prompt in parallel.
- No relay connection gains trusted-UI identity, and no public surface (MCP or
  the machine CLI) answers a prompt.
- **Residual risk.** A same-user process that deliberately escapes the Splint
  and requester process trees (for example with `systemd-run --user`, or
  through `ssh localhost`) can register as a responder, see every workspace
  prompt, and answer it, Always included. Such a process can already edit
  `policy.json` or the remembered-grant store, so this is the same class of
  risk, not a new one. Consent bounds cooperating agents; it is not a sandbox
  (ADR 0014). A compromised relay or remote account can likewise forge
  answers and show remote-labelled dialogs, but it already controls that
  account and everything its Windows show.
- Every host connected by a remote Window must run protocol 45 together.
