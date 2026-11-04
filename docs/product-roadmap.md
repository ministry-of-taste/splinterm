# Product roadmap

- **Status:** Active strategic direction
- **Horizon model:** Now / Next / Later / Explore; no date promise
- **Product maturity:** Stable 0.2.0 on the documented Omarchy/Arch target
- **Current-state authority:** [Current product status](status.md)
- **Delivery authority:** Maintainer-controlled delivery coordination; accepted public decisions are promoted to the PRD, ADRs, and specialist documentation

This roadmap explains where Splinterm is going as a product: who it serves,
which user outcomes matter next, how the audience expands, and what must become
true before the product advances to another maturity horizon. It is not a
feature backlog, implementation sequence, release date, or compatibility
promise.

## North star

> **Splinterm should become the place terminal work lives while windows,
> machines, and tools come and go.**

The product begins with a simple promise: closing a graphical terminal should
not end the work beneath it. It grows from that foundation into one persistent,
understandable workspace shared by people and explicitly bounded automation.

Splinterm is not trying to win by having the longest terminal feature list. It
should win by making continuity, topology, and authority feel native:

- shells and layouts belong to a persistent substrate rather than a window;
- Lairs, Dojos, and Splints make long-lived terminal work understandable;
- local, remote, and headless access reach the same work without creating
  separate worlds;
- automation can be useful without inheriting human trust; and
- compatibility and release claims remain narrower than the evidence.

## Audience sequence

Splinterm should expand one audience at a time rather than claim broad Linux
support before it can support that claim.

1. **Beachhead — Omarchy and Arch developers.** People who live in terminals,
   value native Wayland integration, and want persistence without layering a
   separate multiplexer interaction model over every terminal.
2. **Expansion — remote and headless operators.** People who want the same
   persistent work available locally, over SSH, or on a homelab/server without
   exposing a new daemon network service.
3. **Platform users — integration and automation authors.** People building
   tools that need structured, least-privileged access to real terminal work
   rather than a disconnected automation-only session model.
4. **Broader Linux users.** Additional distributions, compositors, and package
   ecosystems only after demand and validation capacity justify a support
   commitment.

The primary human workflow remains the product anchor. Automation expands the
product; it does not redefine Splinterm as an “AI terminal.”

## Roadmap commitments

The labels in this document have deliberate meanings:

- **Now** — product outcomes that strengthen daily-driver confidence on the
  validated Omarchy/Arch target.
- **Next** — outcomes required for a broader 1.0 support contract; these future
  guarantees are not implied by stable 0.2.0.
- **Later** — strategic expansion that follows a trustworthy primary product.
- **Explore** — options worth researching, not commitments.

Moving an item between horizons is a product decision. Implementation plans may
change without changing the product outcome, but they may not silently promote
an Explore option into a promise.

## Horizon 1 — Stable 0.1: strengthen daily-driver confidence (Now)

### Product promise

A user can install Splinterm on the validated Omarchy/Arch environment, use it
as a capable native terminal, close the graphical view, and deliberately return
to the same daemon-owned work. Authorized tools can inspect or control that same
work only through explicit bounded authority.

### Product outcomes

- The persistence model is easy to understand before a user risks important
  work.
- Fresh launch, detach, reopen, process exit, restore, delete, daemon stop, and
  upgrade have distinct language and consequences.
- The native terminal feels coherent enough for daily use: visual fidelity,
  input, tabs, panes, search, clipboard, presets, themes, and Omarchy desktop
  integration reinforce one product rather than a collection of features.
- Installation, package identity, upgrade refusal, recovery, and diagnostics are
  trustworthy outside the maintainer's own machine.
- Bounded automation is demonstrable through a calm consent-to-revocation
  journey, not only through protocol tests.
- Known client memory and responsiveness limits have a passing result or an
  explicit product disposition before expanding support claims.

### Current product bets

- Make Lair and Dojo lifecycle intentional: named, pinned, disposable,
  restorable, and expired work should be recognizable and manageable.
- Finish the fit-and-finish gaps that make the terminal feel native to Omarchy,
  including exact theme fidelity, live native system-font following, and
  supported desktop integration.
- Improve ordinary desktop workflows such as inserting local file and saved
  image paths without weakening terminal input or privacy boundaries.
- Turn the existing CLI, SSH, and MCP capability into understandable user and
  integration-author journeys.
- Keep release, website, package, and support language synchronized with actual
  evidence.

### Graduation signals

Horizon 1 is successful when:

- a new user can install, launch, close, and reopen work from the published
  documentation without maintainer intervention;
- users can distinguish detached, exited, restorable, and destructive states;
- packaged upgrade and recovery behavior is tested and understood;
- daily-driver blockers on the validated target have explicit dispositions;
- continued performance validation passes without moving cost from daemon to client or
  regressing responsiveness; and
- user feedback identifies workflow problems rather than basic uncertainty
  about what the product is.

This horizon does **not** promise broad compositor support, reboot-transparent
process survival, arbitrary `foot.ini` compatibility, collaborative typing, or
unrestricted automation.

## Horizon 2 — Supported 1.0: become dependable (Next)

### Product promise

An Omarchy/Arch user can choose Splinterm as a supported primary terminal and
understand how long compatibility lasts, what an upgrade may end, how to recover,
and where to report a defect or security issue.

### Product outcomes

- The complete primary journey—install, first launch, organize, detach, return,
  configure, update, recover, and remove—is supported rather than merely
  implemented.
- Stable public boundaries are explicit: human workflows, configuration,
  machine schemas, package channels, compatibility windows, and intentional
  exclusions.
- Release artifacts are immutable and verifiable, with tested rollback and
  recovery procedures.
- Support and security-reporting processes exist and match the project's actual
  maintenance capacity.
- Performance, resource bounds, diagnostics, and failure behavior are release
  qualities, not specialist knowledge hidden in retained evidence.
- Documentation and trusted UI make destructive consequences visible before the
  action occurs.

### Graduation signals

A supported 1.0 requires:

- declared platform, compositor, architecture, and compatibility commitments;
- declared release channels, support duration, and breaking-change policy;
- public install/upgrade/recovery evidence beyond the maintainer workflow;
- no unresolved release-blocking correctness, resource, or security gate;
- an actionable issue and security-reporting path; and
- independent product/readability and technical release review.

Version `1.0` is a support contract, not a reward for accumulating features.

## Horizon 3 — Connected persistent workspace (Later)

### Product promise

A person's terminal work remains one coherent workspace whether they enter from
a local Wayland window, a remote native client, a headless host, or an authorized
tool.

### Product outcomes

- Remote and headless workflows feel like intentional product journeys rather
  than expert transport configuration.
- Endpoint identity, connectivity, control ownership, and recovery are visible
  and understandable.
- Integrations can discover, observe, and act through stable bounded contracts
  without depending on private daemon frames or terminal scraping.
- Human and automation activity remain distinguishable even when they share a
  Lair or Splint.
- Operators can reason about policy, grants, audit metadata, revocation, and
  cleanup without becoming protocol experts.

### Candidate bets

- Productize native remote profiles, connection diagnostics, and recovery around
  the already implemented SSH transport.
- Provide integration kits and reference journeys for coding tools, operators,
  and MCP hosts.
- Evaluate first-class integrations only when the external system has a stable
  capability baseline and the integration preserves Splinterm's authority model.
- Improve portable configuration and workspace definitions without executing
  untrusted shell source or turning presets into hidden authority.

This horizon does not imply a `splinterd` TCP listener, cloud account, hosted
control plane, synchronized secrets, or collaborative simultaneous typing.

## Horizon 4 — Broader Linux workspace platform (Explore)

### Product promise

Where support capacity and demand justify it, Splinterm can bring the persistent
workspace model beyond its Omarchy beachhead without diluting reliability or
claiming environments it cannot validate.

### Candidate outcomes

- Reproducible Nix and Home Manager workflows with honest daemon/socket and
  upgrade semantics.
- Additional Wayland compositor support backed by explicit compatibility
  matrices.
- Tertiary distribution artifacts whose service, policy, desktop, and trusted-UI
  identity contracts remain coherent.
- A carefully bounded extension model for integrations and trusted application
  actions, if it can remain closed to terminal-content authority.
- Selective terminal compatibility expansion driven by real applications rather
  than checklist competition.

### Decision gate

Each platform or ecosystem expansion must answer:

1. Who is the user and what current journey is blocked?
2. Can the project continuously validate the environment?
3. What compatibility and support promise would publication create?
4. Does the expansion preserve daemon ownership, explicit authority, and the
   Foot oracle?
5. What primary-product work would be delayed by accepting it?

Until those questions have acceptable answers, broader distribution and feature
expansion remain options rather than roadmap commitments.

## Product health signals

Splinterm does not embed product telemetry in the terminal application. The
public website uses Cloudflare Web Analytics for privacy-preserving aggregate
page and visit trends; it does not establish whether a terminal workflow
succeeded. Product decisions should combine that limited signal with opt-in
reports, issue patterns, release validation, documentation feedback, and
explicit user research.

The product should eventually measure or otherwise establish:

- **Activation:** users reach a running Splint and successfully return after
  closing its Window.
- **Continuity:** users trust detach/reopen and do not lose work through a
  misunderstood lifecycle or upgrade boundary.
- **Adoption:** users choose Splinterm for sustained terminal work, not only a
  successful first launch.
- **Comprehension:** users can explain Lair, Dojo, Splint, Window, detach,
  restore, and controller ownership well enough to predict consequences.
- **Quality:** crashes, resynchronizations, resource spikes, and recovery failures
  are bounded, diagnosable, and declining.
- **Automation trust:** consent, denial, control transfer, revocation, and cleanup
  succeed without granting broader authority than intended.
- **Supportability:** reported problems contain enough bounded diagnostics to act
  on without exposing terminal bodies or secrets.

Quantitative targets belong in a reviewed release or product plan once a
collection method and baseline exist. This roadmap does not invent numbers that
the project cannot currently observe.

## Strategic boundaries

Across every horizon, Splinterm remains:

- persistence-first rather than window-first;
- human-centered even when automation is present;
- security-conscious rather than absolutely secure;
- explicit about destructive lifecycle boundaries;
- Omarchy-first before broadly portable;
- grounded in Splinterm-owned contracts, with pinned Foot retained as an
  optional historical differential; and
- honest about implemented, validated, supported, planned, and deferred work.

The product roadmap should be reconsidered if growth requires weakening those
boundaries. A larger audience is not success if the product becomes less
understandable, less supportable, or less trustworthy.

## Relationship to delivery planning

This document owns strategic direction, audience sequence, product outcomes, and
horizon priority. Maintainer-controlled delivery records translate the active
horizon into technical workstreams and dependency order. The [PRD](PRD.md) owns
normative product requirements and release criteria. [Current status](status.md)
owns what exists and is validated today. Public ADRs and specialist documents
own the decisions needed to understand shipped behavior.

When these documents disagree, resolve strategy and horizon priority here, then
update the PRD only when normative requirements or release criteria change. Do
not rewrite historical evidence to make it resemble the current strategy.
