# ADR 0005: daemon-launched trusted consent client

- **Status:** Accepted; persistent policy added by [ADR 0007](0007-supported-automation-policy.md); outcomes, prompt input, and remembered grants amended by [ADR 0014](0014-easy-agent-access.md); where workspace prompts are shown amended by [ADR 0015](0015-remote-consent.md)
- **Date:** 2026-07-18

## Context

Phase 7 must authorize terminal observation and control without allowing
terminal content or the requesting protocol client to spoof the consent
surface. Letting a requester declare itself graphical or resolve its own prompt
would not establish a trustworthy boundary. `splinterd` must remain headless,
and persistent policy requires a separate future product decision.

## Decision

For requests that lack an existing grant, `splinterd` launches a separate
`splinterm consent` Wayland client. The daemon remains headless: all Wayland,
input, rendering, and UI objects belong to that disposable client.

The daemon and consent client communicate over a private inherited file
descriptor. One-use capability material is generated from OS randomness and is
never placed in argv, environment variables, logs, or terminal content. All
other inherited descriptors are closed or close-on-exec. The exchange has
bounded framing, a short deadline, and grant-once/deny outcomes only.

A pending request and resulting grant are bound to:

- peer UID, PID, and executable identity;
- Splint ID and process incarnation;
- explicitly requested scopes;
- one daemon lifetime; and
- one bounded expiry/revocation lifecycle.

Scopes distinguish observe, scrollback, input, resize, clipboard read,
clipboard write, and terminate. Clipboard scopes authorize metadata/capability
use only; clipboard bodies remain client-local and must never enter daemon audit
records.

The trusted consent window identifies the requesting executable and scopes and
provides grant-once and deny actions. It is rendered as application UI, never as
terminal cells. Active grants and controller ownership remain visibly indicated
in the normal Splinterm window. Revocation closes affected subscriptions,
releases controllers, and rejects later operations.

Audit records are bounded metadata: timestamp/order, peer identity, Splint
identity, scopes, decision, revocation, and reason. They never contain terminal
bodies, clipboard bodies, or input bytes.

The matching sibling `splinterm` executable is the trusted first-party UI and
may implicitly receive only observe, input, and resize authority for its normal
window lifecycle. This identity check uses the same executable device/inode
binding as trusted status/revocation UI. It does not cover scrollback,
clipboard, or terminate scopes and does not authorize another executable,
automation client, or copied binary. Those requests continue through explicit
consent. This avoids prompting every time the disposable first-party window is
reopened without creating a persistent third-party allow rule.

Development bypass remains explicit, visually labeled, and unsuitable for
supported automation. No third-party grant is persisted across daemon restart.

## Consequences

- Initial authorization can work even when no normal graphical terminal is
  already attached.
- Requesters cannot approve themselves through the public protocol.
- Consent availability depends on locating and launching the matching
  `splinterm` executable; launch failure safely denies the request.
- Packaging must preserve executable identity and the daemon-to-client launch
  relationship.
- Persistent allow rules remain deliberately out of scope until separately
  approved.

## Amendment: easy agent access (ADR 0014)

[ADR 0014](0014-easy-agent-access.md), accepted for `0.2.0` and implemented on
`main`, changes three parts of this decision. The daemon-launched trusted
client, the private inherited descriptor, the one-use capability, and the
bounded framing stay as decided here.

- **Outcomes.** The exchange returns a typed decision instead of grant-once or
  deny: deny, or a grant for one of three durations (just this once, while this
  workspace is open, always for this workspace) at one of three access levels
  or an exact Custom scope set.
- **Prompt input.** The access dialog becomes default-deny. Enter, Escape, and
  `D` deny; only `G` or the grant button grants. An arming delay restarts on
  every focus gain and duration change, key repeats are ignored, and the
  duration is chosen only by pointer or arrow keys. Until then it grants on
  Enter.
- **Persistence and deadline.** "No third-party grant is persisted across daemon
  restart" no longer holds for an explicit Always answer, which the daemon
  stores in its remembered-grant store under the identity rules of ADR 0014. The
  20-second deadline is replaced by a bounded, cancellable pending request.

## Amendment: remote consent (ADR 0015)

[ADR 0015](0015-remote-consent.md), accepted for `0.2.0` and implemented on
`main`, changes where a workspace access prompt is shown. The daemon still
launches its own local dialog when it has a graphical session, and that dialog
is unchanged. It now also offers the prompt to every eligible remote Window
connected over SSH and every `splinterm approvals` terminal approver; the first
valid answer wins and the others are withdrawn. A remote Window launches its
own `splinterm consent` dialog for an offer, on the viewer's computer, with the
same private descriptor, one-use capability, and input rules, and marks it
remote. With no local dialog and no responder, the request still fails at
once. Splint access requests and forced takeover stay local and
daemon-launched only.
