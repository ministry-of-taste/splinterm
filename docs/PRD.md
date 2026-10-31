# Splinterm Product Requirements Document

- **Status:** Draft
- **Product maturity:** Stable 0.1.0 on the documented Omarchy/Arch target
- **As of:** 2026-08-11
- **Strategic direction authority:** [Product roadmap](product-roadmap.md)
- **Normative requirements authority:** This PRD after review and acceptance
- **Current maturity authority:** [Status](status.md)
- **Related documents:** [Product roadmap](product-roadmap.md), [Architecture](architecture.md), [Usage](usage.md), [CLI](cli.md), [Configuration](configuration.md), [Automation](automation.md), and [Packaging](packaging.md)

## 1. Purpose

This document consolidates Splinterm's normative product requirements and release criteria from the current implementation and repository documentation. The [product roadmap](product-roadmap.md) defines what the product is becoming, whom it serves next, and which outcome horizons have priority; this PRD defines the behavior and gates those active product promises require.

This PRD is not an implementation plan or architecture decision log:

- this document owns normative product requirements and release criteria;
- [architecture.md](architecture.md) owns system boundaries and technical invariants;
- [ADRs](adr/) own accepted architectural decisions;
- specialist documents own detailed user and compatibility contracts; and
- retained tests, benchmark evidence, and release records establish whether a feature has been validated.

Maintainer implementation sequencing and handoffs are tracked outside the public product repository. Decisions needed to understand shipped behavior are promoted into public ADRs and specialist documentation.

When this draft conflicts with the current implementation or an accepted ADR, treat the conflict as a product-documentation defect to resolve rather than silently changing behavior.

## 2. Product definition

> **Splinterm is a persistent, security-conscious terminal substrate for humans and bounded automation.**

Splinterm combines a native Wayland terminal with a headless daemon that keeps shells, layouts, terminal state, and scrollback alive when graphical clients disconnect. Humans use the persistent topology through native windows, tabs, panes, search, clipboard, and explicit control workflows. Authorized tools use the same topology through bounded JSON/NDJSON, SSH relay, and MCP interfaces.

Splinterm is built in Rust from Foot's terminal behavior and is designed first for Omarchy and Arch Linux.

### 2.1 Core differentiator

A normal terminal window owns the shell it presents. Splinterm separates those lifetimes:

- `splinterd` owns persistent terminal processes and canonical state;
- `splinterm` is a disposable native Wayland view;
- automation clients are disposable, independently authorized views and controllers; and
- closing or crashing a graphical client must not terminate its daemon-owned shells.

Persistence is the product foundation, not an optional multiplexer feature layered onto a conventional terminal.

### 2.2 Product vocabulary

| Term | Product meaning |
|---|---|
| **Topology** | The daemon's complete persistent session catalog. |
| **Lair** | A named persistent session or project. |
| **Dojo** | A persistent terminal layout inside a Lair. |
| **Splint** | One terminal pane and process slot inside a Dojo. |
| **Window** | A disposable native Wayland client surface. |
| **Tab** | A Window-local attachment to a Dojo; it does not own or persist the Dojo. |

## 3. Problem statement

Terminal-centric work has several recurring failures:

1. closing or losing a graphical terminal can end valuable shell and process state;
2. external multiplexers add a separate interaction and configuration model;
3. graphical use and automation often operate through unrelated state models;
4. automation access is frequently either too weak to be useful or too broad to be trusted;
5. remote access can accidentally turn a local terminal daemon into a network service; and
6. compatibility claims can drift without versioned, product-owned behavioral contracts.

Splinterm should solve these problems with one daemon-owned topology, a native Omarchy-first interface, explicit and bounded authority, and tested terminal behavior initially informed by the pinned Foot reference.

## 4. Target users

### 4.1 Primary users

**Omarchy and Arch developers** who want a fast native Wayland terminal whose shells and layouts survive graphical client closure.

They need:

- familiar terminal behavior;
- native windows, panes, tabs, clipboard, search, IME, scaling, and theme integration;
- quick creation, detachment, reopening, and recovery workflows; and
- honest process-lifetime semantics.

### 4.2 Advanced users and operators

**Users operating persistent local or headless terminal sessions** who need explicit lifecycle, restore, remote access, policy, and troubleshooting controls.

They need:

- a daemon that can run without Wayland;
- owner-controlled policy and service administration;
- authenticated SSH stdio relay without a daemon network listener;
- bounded state, clean shutdown, and documented backup/recovery semantics; and
- clear distinctions between detach, process exit, relaunch, restore, and reset.

### 4.3 Automation and integration authors

**Tool authors and coding-agent operators** who need structured terminal access without receiving trusted graphical authority.

They need:

- versioned JSON/NDJSON schemas;
- stable IDs, revisions, errors, and exit categories;
- least-privileged executable-scoped policy;
- bounded observation and exclusive control;
- explicit resynchronization after gaps; and
- CLI, SSH relay, and MCP paths over the same topology.

### 4.4 Evaluators and contributors

**Evaluators** need to understand the product, differentiator, validated environment, current maturity, and first workflow quickly. **Contributors** need clear architecture, ADRs, test authorities, and scope boundaries.

## 5. Jobs to be done

1. **Keep work alive:** When I close, restart, or lose a graphical terminal client, keep daemon-owned shells and layouts running so I can reconnect later.
2. **Organize terminal work:** Let me arrange persistent sessions, layouts, and panes while using disposable native windows and tabs as views.
3. **Resume deliberately:** Let me create a fresh terminal by default, choose a recent running Dojo, or reopen the last remembered running Dojo without confusing reopening with process restoration.
4. **Control safely:** Let multiple clients observe the same state while ensuring only one controller owns input and resize authority for a Splint at a time.
5. **Automate explicitly:** Let authorized tools inspect and mutate terminal topology through stable machine contracts without inheriting human UI trust.
6. **Work remotely without exposing a daemon port:** Let a human use native remote Splinterm through authenticated SSH, while separately policy-scoped automation uses the raw stdio relay.
7. **Retain terminal compatibility:** Give me terminal behavior governed by versioned Splinterm contracts and informed by a pinned Foot historical differential rather than an undocumented approximation.
8. **Fit Omarchy naturally:** Follow the active Omarchy theme, launch through `xdg-terminal-exec`, and behave as a native Wayland application without modifying user configuration automatically.

## 6. Product principles

1. **Daemon-owned persistence:** terminal processes and canonical state outlive graphical views.
2. **One substrate:** humans and authorized automation operate on the same topology.
3. **Bounded automation:** authority is explicit, scoped, revocable, resource-bounded, and independently identified.
4. **Disposable clients:** neither graphical nor automation clients become the source of persistent truth.
5. **Exact targeting:** stale IDs, incarnations, revisions, or captured targets fail explicitly; operations never guess a replacement.
6. **Terminal content is data:** output can never grant authority, approve consent, change policy, or become an automatic instruction.
7. **Compatibility needs owned contracts:** Splinterm-owned tests and adopted fixtures are release authority; Foot 1.27.0 commit `3c5b584b0eafa772eb4376fb6eaf6643399e190e` remains an optional historical differential.
8. **Honest maturity:** implemented, validated, supported, proposed, deferred, and publicly released have distinct meanings.
9. **Omarchy first, portability later:** optimize and validate the primary environment before claiming broader support.
10. **No hidden mutation:** installation and integration do not silently change the default terminal, user keybindings, SSH policy, or Omarchy-owned files.

## 7. Current product baseline

The following table summarizes the current repository state. “Validated” means retained tests or evidence record the required checks; it does not imply a public compatibility guarantee beyond the documented scope.

| Area | Current status |
|---|---|
| Native Wayland terminal | Implemented and validated on the documented Omarchy/Hyprland target. |
| Daemon-owned sessions and client detach/reattach | Implemented and validated. |
| Lair/Dojo/Splint topology and persistent multiplexing | Implemented and validated. |
| Multi-pane graphical layouts | Implemented and validated. |
| Window-local Dojo tabs | Implemented; a Window supports up to 32 distinct Dojo attachments and may mix Lairs. |
| Recent Dojos picker and reopen-last workflow | Implemented. |
| Searchable closed command palette and trusted tab context menu | Alpha2 scope is packaged and accepted; the bounded alpha3 expansion and keymap-closure matrix are implemented and non-graphically validated, with packaged graphical acceptance still pending. |
| Multi-client observation and exclusive control transfer | Implemented and validated. |
| Scrollback, literal search, selection, clipboard, IME, and scaling | Implemented within the documented contracts. |
| Omarchy theme discovery and live reload | Implemented. |
| Sixel | Supported Foot-compatible bounded implementation. |
| Kitty graphics | Supported practical static-image subset; not full Kitty compatibility. |
| iTerm2 inline images | Supported bounded inline-PNG subset. |
| JSON/NDJSON automation | Implemented as the publicly documented machine compatibility contract; 0.x compatibility remains versioned and may change with documented migration. |
| SSH stdio relay | Implemented and validated; no daemon network listener. |
| Native remote graphical transport | Implemented and validated: strict profiles, one-authentication multiplexer, endpoint-bound human-interactive workflow, remote-safe launches, namespaced recency, remote no-image/focus enforcement, lifecycle, authentication, failure handling, persistence, and reviewed real-host graphical evidence. |
| MCP adapter | Implemented and validated as an optional separately identified package. |
| Easy agent access: access levels, remembered grants, and the access list | Implemented on `main` for `0.2.0` per [ADR 0014](adr/0014-easy-agent-access.md) and non-graphically validated: access levels, durations, remembered grants, the default-deny prompt, pending requests, the human and JSON `splinterm access` list and revoke, the Agent access list in the command palette, the Explorer marks, the waiting indicator, and one-step `splinterm mcp setup`. Pending: graphical acceptance of the prompt and the Agent access list in the VM. |
| Arch/Omarchy package and release installer | Immutable versioned GitHub/AUR packages and installation paths implemented and validated. |
| Public source and documentation | Available. |
| AUR packages | Prebuilt `splinterm-bin` and source-built `splinterm` available; stable 0.1.0 does not promise broader compatibility or a support lifetime. |
| Stable support policy | Not released. |
| Nix and broader distribution | Planned. |
| Public product/documentation website | Implemented and build/link validated; repository `docs/status.md` remains the maturity authority. |

## 8. Goals

### 8.1 Current product goals

- Preserve shell, layout, terminal, and scrollback continuity across graphical client disconnection.
- Provide a complete native Wayland terminal workflow for the validated Omarchy environment.
- Make persistent multiplexing understandable through Lairs, Dojos, Splints, windows, panes, and tabs.
- Offer first-class human workflows without requiring an external multiplexer.
- Provide stable, bounded machine access through CLI, relay, and MCP contracts.
- Keep human consent, trusted UI, automation policy, and control ownership visibly distinct.
- Preserve Foot-derived behavior with reproducible differential evidence.
- Keep ordinary text-only terminal use efficient and ensure optional image support has bounded resource cost.
- Package and upgrade Splinterm without silently changing user-owned desktop configuration.
- Keep current-status, usage, CLI, security, and release documentation explicit throughout the release lifecycle.

### 8.2 Stable-release goals

A supported stable release should let a user:

1. understand the product and maturity before installation;
2. install from a versioned, immutable, verifiable artifact;
3. launch a fresh terminal through the documented desktop/XDG path;
4. detach and reconnect without losing daemon-owned work;
5. use panes, tabs, history, clipboard, search, and control workflows confidently;
6. understand persistence and upgrade limitations before risking active work;
7. configure the supported surface and migrate documented Foot values;
8. authorize or decline automation using least-privileged examples; and
9. find a declared compatibility, upgrade, troubleshooting, and support policy.

## 9. Non-goals

The current product does not promise:

- broader platform compatibility or a guaranteed support lifetime;
- live daemon upgrade handoff in Splinterm 0.1;
  [ADR 0011](adr/0011-guarded-in-place-daemon-reexec.md) defines an unimplemented
  `0.2.0` target only for explicitly negotiated compatible planned upgrades;
- transparent process continuity across daemon crash, logout, reboot, or host
  failure;
- restoration of process memory, kernel PTYs, durable terminal bodies, or
  arbitrary unpersisted state after daemon or host loss;
- full Foot configuration compatibility;
- tmux configuration or plugin compatibility;
- broad Linux, compositor, or distribution support;
- a daemon TCP listener or unauthenticated remote API;
- collaborative simultaneous typing or automatic controller stealing;
- automation access to trusted graphical authority or compositor window control;
- inferred coding-agent task status, inter-agent messaging, readiness, completion, or result transport;
  an agent may explicitly report one of a closed set of display states for its
  own Splint (see [ADR 0007](adr/0007-supported-automation-policy.md)), which is
  display data only and never authority, a message channel, or a completion signal;
- terminal output as trusted instructions;
- full Kitty graphics compatibility, external Kitty file/SHM transports, placeholders, relative placement, or animation;
- arbitrary shell/plugin/terminal-content commands in trusted application menus;
- a general graphical widget toolkit; or
- automatic edits to Hyprland, Omarchy, SSH, terminal preference, or user policy files.

## 10. Functional requirements

Priority meanings:

- **P0:** defining behavior or release blocker;
- **P1:** required for a complete supported primary workflow;
- **P2:** valuable expansion that must not weaken P0/P1 contracts.

### 10.1 Persistence and topology

| ID | Priority | Requirement | Current state |
|---|---:|---|---|
| `FR-PERSIST-01` | P0 | `splinterd` must remain the sole owner and writer of live terminal processes, canonical terminal state, topology, and durable metadata. | Implemented |
| `FR-PERSIST-02` | P0 | Closing or crashing a graphical client must not terminate daemon-owned Splints. | Implemented and validated |
| `FR-PERSIST-03` | P0 | The product must model stable Lair, Dojo, and Splint identities independently of names, display position, and client focus. | Implemented |
| `FR-PERSIST-04` | P0 | Relaunch must retain the Splint identity while allocating a new nonzero process incarnation; stale authority must not carry across incarnations. | Implemented |
| `FR-PERSIST-05` | P0 | After daemon or host loss, restore must recover only validated topology and launch metadata with leaves exited/restorable and relaunch remaining explicit. It must not claim restoration of live PTYs, process memory, terminal grids, scrollback bodies, or image bodies. | Implemented; documentation must remain explicit |
| `FR-PERSIST-06` | P1 | Structural mutations must be revision-bound and fail rather than overwrite concurrent topology changes. | Implemented |
| `FR-PERSIST-07` | P0 | For `0.2.0`, an explicitly negotiated compatible planned daemon upgrade must preserve the daemon PID, child/PTY identity, one-reader ownership, terminal incarnation, ordered I/O, reaping, and signaling through guarded in-place re-exec. Unsupported state or incompatible schema must block rather than degrade silently. | Accepted target in ADR 0011; not implemented |
| `FR-PERSIST-08` | P0 | `0.2.0` reboot and daemon-loss restoration must remain recipe-only. Body-bearing handoff checkpoints must use only anonymous sealed memory-backed descriptors and must not become durable terminal-grid, scrollback, image, parser, reply, or input archives, including after abrupt candidate failure. | Accepted in ADR 0012; existing durable privacy boundary retained |

### 10.2 Native human interface

| ID | Priority | Requirement | Current state |
|---|---:|---|---|
| `FR-UI-01` | P0 | Provide a native Wayland terminal client with keyboard, pointer, clipboard, IME, scaling, and damage-driven rendering in the validated environment. | Implemented and validated |
| `FR-UI-02` | P0 | A managed Window must render and operate a persistent Dojo containing a binary Splint layout. | Implemented |
| `FR-UI-03` | P1 | A Window must support an ordered, non-persistent set of no more than 32 distinct Dojo tabs, including Dojos from multiple Lairs. | Implemented |
| `FR-UI-04` | P0 | Closing a tab must detach only the Window-local reference; it must not close the Dojo or terminate its Splints. | Implemented |
| `FR-UI-05` | P1 | Hidden tabs must remain synchronized without painting, blinking, reporting focus, resizing, or owning a controller. | Implemented |
| `FR-UI-06` | P1 | Provide trusted Recent Dojos, command palette, tab menu, rename, confirmation, search, consent, and control surfaces that isolate terminal input while active. | Implemented |
| `FR-UI-07` | P0 | Destructive actions must be explicit, exactly targeted, and confirmed where documented; cancellation must be the safe default. | Implemented |
| `FR-UI-08` | P1 | Modal actions must capture exact resource identities and availability when opened and must never retarget because asynchronous state changes. | Implemented |
| `FR-UI-09` | P1 | Native application controls must remain usable without exposing shell-, plugin-, terminal-content-, or automation-provided trusted commands. | Implemented |
| `FR-UI-10` | P1 | Before alpha3 publication, the curated command palette, bindable action registry, resolved shortcut labels, built-in `splinterm` and `omarchy-tmux` profiles, safe context-sensitive `Super+C/V/X/Z` behavior, custom-overlay reload, and runtime dispatch must work as one closed validated surface without permitting externally registered trusted commands or capturing terminal-pane `Super+X/Z`. | Implemented and non-graphically validated; packaged graphical acceptance pending |
| `FR-UI-11` | P2 | In a post-alpha3 update before supported 1.0, provide strictly validated user-defined tab identity, behavior, and appearance: presentation labels/icons/pinning, deterministic ordering and default actions, per-tab shortcuts limited to the closed typed action registry, and bounded theme-aware visual overrides. Customization must preserve accessibility, exact targeting, non-destructive tab semantics, and trusted-UI isolation. | Planned post-alpha3, pre-1.0 |
| `FR-UI-12` | P1 | Before alpha3 publication, a bounded Wayland `text/uri-list` drop of one or more local regular files must insert one deterministic POSIX-shell-escaped path payload into the exact accepted, live, controlled Splint. It must reject malformed, remote, stale, modal, unsupported, or unauthorized drops without PTY input; never read or persist file bodies or paths; preserve bracketed paste; and append no submission bytes. | Implemented and non-graphically validated; packaged graphical acceptance pending |

### 10.3 Session lifecycle and control

| ID | Priority | Requirement | Current state |
|---|---:|---|---|
| `FR-LIFE-01` | P0 | Default desktop/XDG launch must create a fresh Lair and Dojo rather than silently attaching to previous work. | Implemented |
| `FR-LIFE-02` | P1 | Reopening existing work must be a separate explicit picker or reopen-last action. | Implemented |
| `FR-LIFE-03` | P0 | Kill, close, detach, relaunch, restore, and reset must remain separate operations with honest consequences. | Implemented |
| `FR-LIFE-04` | P1 | Before alpha3 publication, users must be able to explicitly save, preview, pin, and restore a Lair's Dojo trees, proportional split ratios, default focus, Splint names, known structured launch recipes, launch working directories, and bounded geometry hints. Saving must never checkpoint process state, infer arbitrary foreground applications, persist sensitive terminal bodies, or execute commands automatically. | Implemented and non-graphically validated; packaged graphical acceptance pending |
| `FR-CTRL-01` | P0 | Observation must not imply input or resize control. | Implemented |
| `FR-CTRL-02` | P0 | At most one connection may own controller/size authority for a live Splint at a time; different Splints may have different controllers. | Implemented and validated |
| `FR-CTRL-03` | P0 | Control transfer, force transfer, release, revocation, denial, timeout, and disconnect behavior must be explicit and visible. | Implemented |
| `FR-HIST-01` | P1 | Scrollback and literal search must be bounded, revision/generation-aware, and resynchronize explicitly after gaps or replacement. | Implemented |
| `FR-HIST-02` | P1 | Before alpha3 publication, Return or keypad Enter pressed while the focused Splint viewport is historical must return that viewport to live output, consume the physical key through release, and send no PTY bytes; Enter pressed while already live retains normal terminal behavior. | Implemented and non-graphically validated; packaged graphical acceptance pending |

### 10.4 Configuration and Omarchy integration

| ID | Priority | Requirement | Current state |
|---|---:|---|---|
| `FR-CONFIG-01` | P1 | Support a small explicit configuration surface with strict parsing, bounded values, and actionable diagnostics for unsupported settings. | Implemented |
| `FR-CONFIG-02` | P1 | Support the documented Foot migration subset without claiming arbitrary `foot.ini` compatibility. | Implemented |
| `FR-OMARCHY-01` | P0 | Integrate through stable application identity and `xdg-terminal-exec` without modifying user Hyprland or Omarchy configuration automatically. | Implemented |
| `FR-OMARCHY-02` | P1 | Discover the active Omarchy theme and apply valid palette changes live while retaining the last valid theme during transient or malformed updates. | Implemented |
| `FR-OMARCHY-03` | P1 | Preserve a portable explicit theme override and a safe bundled fallback when Omarchy state is unavailable. | Implemented |

### 10.5 Terminal behavior and images

| ID | Priority | Requirement | Current state |
|---|---:|---|---|
| `FR-TERM-01` | P0 | Terminal semantics and renderer behavior must retain Foot provenance and use the pinned Foot oracle for claimed compatibility. | Implemented and validated within recorded scopes |
| `FR-TERM-02` | P0 | Full-frame, incremental, scroll-copy, cache, resize, and reattach paths must preserve equivalent visible results for equivalent terminal state. | Implemented and covered by retained tests and evidence |
| `FR-IMG-01` | P1 | Image semantics must remain daemon-owned and protocol-independent while pixel bodies use a separate bounded trusted-client transfer path. | Implemented |
| `FR-IMG-02` | P1 | Support the documented bounded Sixel, practical Kitty static-image, and inline iTerm2 PNG subsets without claiming broader compatibility. | Implemented and documented |
| `FR-IMG-03` | P0 | Image support must not enlarge every terminal cell, place pixel bodies in public automation/audit records, or create unbounded decode, cache, queue, or scrollback storage. | Implemented |
| `FR-IMG-04` | P1 | Unsupported external media transports must fail closed rather than granting ambient filesystem or shared-memory authority. | Implemented |

### 10.6 Automation and remote operation

| ID | Priority | Requirement | Current state |
|---|---:|---|---|
| `FR-AUTO-01` | P0 | Provide human-readable CLI behavior and versioned stable JSON/NDJSON machine contracts with checked-in schemas. Human rendering is not a compatibility boundary. | Implemented |
| `FR-AUTO-02` | P0 | Machine records must carry stable operation, resource identity, revision, truncation, error, and resynchronization semantics appropriate to each operation. | Implemented |
| `FR-AUTO-03` | P0 | Machine mode must never prompt; destructive actions must require explicit confirmation flags or remain outside the machine contract. | Implemented |
| `FR-AUTO-04` | P0 | Automation topology mutation must not imply native Window mapping, compositor focus, movement, resize, or workspace control. | Implemented and documented |
| `FR-AUTO-05` | P1 | Provide a separately executable and independently policy-identifiable MCP adapter with the documented operation coverage. | Implemented and validated |
| `FR-AUTO-06` | P1 | For `0.2.0`, interactive consent must offer three workspace access levels with fixed consent-scope bundles: Look, Look and type (including control takeover), and Manage, each including agent-status reporting and none including clipboard, authorization administration, audit, or handoff authority. Any other requested scope set must be shown as Custom with every scope listed and granted exactly, never rounded up; clipboard scopes remain invalid, and a Custom request containing control takeover may be granted just this once only. | Implemented on `main` for 0.2 and non-graphically validated; graphical acceptance of the prompt in the VM is pending |
| `FR-AUTO-07` | P1 | For `0.2.0`, the person must be able to choose just this once, while this workspace is open, or always for this workspace. Each duration must bind the identity ADR 0014 assigns to it. "Always" must be offered only to protected installs and qualifying Nix profile entries, revalidated on every connection, and must never use a host name, basename, or copied digest as an identity. Known interpreters, recognized by a fixed basename list as a user-experience rule rather than a security boundary, must be offered only the two shorter durations; peers without a pidfd-pinned identity and the installed relay must be offered only just this once. | Implemented on `main` for 0.2 and non-graphically validated; graphical acceptance of the prompt in the VM is pending |
| `FR-AUTO-08` | P1 | For `0.2.0`, the consent prompt must be default-deny (Enter and Escape deny), grant only on an explicit key or button after an arming delay, ignore key repeats, and name the exact choice on its grant button. A request awaiting an answer must be a bounded, cancellable pending request that works within ordinary MCP call deadlines and is never carried across a daemon upgrade. | Implemented on `main` for 0.2 and non-graphically validated; graphical acceptance of the prompt in the VM is pending |
| `FR-AUTO-09` | P1 | For `0.2.0`, an Agent access list in the command palette and a human CLI command must show every grant of every duration, its last use and active use, and matching policy rules, and must revoke per entry. Revoking a remembered grant must leave matching policy rules in place and say so. | Implemented on `main` for 0.2 and non-graphically validated: the daemon inventory, per-entry revoke, and Revoke all access, with the human and JSON `splinterm access` commands and the **Agent access** list in the command palette; graphical acceptance of the list in the VM is pending |
| `FR-REMOTE-01` | P0 | Remote operation must use SSH relays; `splinterd` must expose no network listener by default. Raw automation relay access remains exact-policy scoped. | Implemented and validated |
| `FR-REMOTE-02` | P0 | A machine client must receive only what policy or explicit consent grants it; SSH login, socket access, or running inside a Splint grant nothing by themselves. This bounds cooperating clients. It is not a boundary against a hostile process running as the account, which can start the graphical relay or the graphical client itself. | Implemented; limit documented |
| `FR-REMOTE-03` | P0 | A native remote client must use one authenticated OpenSSH process carrying bounded independent daemon channels rather than one login per pane or a daemon network listener. | Implemented and validated |
| `FR-REMOTE-04` | P0 | Every SSH graphical channel must negotiate the human `RemoteInteractive` role without automation policy, while suppressing trusted-local focus and image authority; a forced transfer is confirmed in the remote Window itself (Plan 0072 M3). | Implemented and validated |
| `FR-REMOTE-05` | P0 | OpenSSH must retain authority for keys, agents, passwords, passphrases, askpass, and host keys; Splinterm must store no credentials or accept unknown keys automatically. | Implemented and validated |
| `FR-REMOTE-06` | P0 | Disconnecting the local remote client must release connection-owned authority without terminating daemon-owned remote Splints. | Implemented and validated |
| `FR-REMOTE-07` | P0 | A native remote Window must create, attach, and control new Lairs, Dojos, and Splints immediately without policy publication or reopening. | Implemented and validated |

### 10.7 Packaging and installation

| ID | Priority | Requirement | Current state |
|---|---:|---|---|
| `FR-PKG-01` | P0 | Packaged releases must contain coherent adjacent runtime executables so trusted graphical identity checks cannot be bypassed by a shadowing client. | Implemented and validated |
| `FR-PKG-02` | P0 | Installation and upgrade must verify exact artifacts, warn before ending daemon-owned shells, and report the lack of cross-version process continuity. | Implemented |
| `FR-PKG-03` | P0 | Packaging must not edit user homes, default terminal preference, Omarchy-owned files, SSH policy, or service lingering without an explicit separate action. | Implemented |
| `FR-PKG-04` | P1 | The MCP adapter must remain an optional exact-version split package and installation alone must grant no authority. | Implemented |
| `FR-PKG-05` | P0 | Stable distribution must use immutable versioned source/artifact URLs, checksums, and a documented upgrade/support policy. | Versioned GitHub and AUR artifacts are available; stable 0.1.0 retains documented platform and upgrade boundaries without promising a support lifetime |
| `FR-PKG-06` | P0 | A handoff-capable launcher must automatically perform only a fully negotiated compatible handoff on human launch, restart automatically only when idle after the `0.1.x` bootstrap boundary, and require exact-count confirmation for the first `0.1.x` to `0.2.0` restart and any active destructive fallback. Package scriptlets must never initiate user-service handoff or restart. | Partial: launcher idle-only restart and confirmed destructive restart implemented (Plan 0037 M4); the `0.1.x` count is best effort (operator decision); automatic handoff on launch deferred |

## 11. Security and privacy requirements

| ID | Priority | Requirement |
|---|---:|---|
| `SEC-01` | P0 | Default third-party authority must fail closed. No policy file and no remembered grant means no persistent third-party grant. From `0.2.0`, a remembered grant exists only through an explicit "always" answer in the trusted consent window ([ADR 0014](adr/0014-easy-agent-access.md)). |
| `SEC-02` | P0 | Grants must bind explicit scopes, the peer or executable identity their duration requires, an exact Lair, Splint, or incarnation, and explicit lifetime or revocation semantics. |
| `SEC-03` | P0 | Revocation must release related controllers and subscriptions. |
| `SEC-04` | P0 | Consent UI must be trusted application chrome and must not render requester-controlled terminal content. |
| `SEC-05` | P0 | Terminal, scrollback, clipboard, search, and input bodies must not appear in audit metadata. |
| `SEC-06` | P0 | Terminal-derived data and in-Splint context variables are discovery hints only, never credentials or instructions. |
| `SEC-07` | P0 | Commands must carry structured argv and working-directory fields; implementations must not rebuild arguments into a shell string. |
| `SEC-08` | P0 | Messages, queues, subscriptions, history, searches, images, transfers, and caches must have explicit bounds and deadlines. |
| `SEC-09` | P0 | Stale identities, revisions, cursors, tokens, and incarnations must fail explicitly without fallback selection. |
| `SEC-10` | P0 | Public claims must say “security-conscious” and describe concrete controls; they must not claim absolute security. |
| `SEC-11` | P0 | Daemon handoff must fence stale clients and expire old connection authority. Any smooth local controller resumption must bind the same surviving client process, exact old connection set, immutable sealed executable snapshot, bounded Window-local resume record, full resnapshot, and single-use generation claim through monitored pidfd identity and kernel-supplied message credentials; transfer, replay, mismatch, or conflict fails closed. |
| `SEC-12` | P0 | From `0.2.0` ([ADR 0014](adr/0014-easy-agent-access.md); implemented on `main` for 0.2 and non-graphically validated): remembered grants must live in a daemon-owned store separate from user policy, opened with strict ownership, mode, link, and size checks and written atomically. A missing or invalid store must deny only remembered authority, and no remembered grant may be reported before its durable write succeeds. Documentation must state that the store, like policy, is editable by any process running as the account. |
| `SEC-13` | P0 | From `0.2.0` ([ADR 0014](adr/0014-easy-agent-access.md); implemented on `main` for 0.2 and non-graphically validated): a daemon upgrade must reload remembered grant definitions fresh from the store and must end old connections, agent controllers, shorter grants, and pending consent requests. |

Detailed scope and policy behavior remain authoritative in [automation.md](automation.md), [ADR 0005](adr/0005-trusted-consent-broker.md), [ADR 0007](adr/0007-supported-automation-policy.md), and, for access levels and remembered grants, [ADR 0014](adr/0014-easy-agent-access.md).

## 12. Non-functional requirements

### 12.1 Reliability

- A graphical-client failure must not end daemon-owned Splints.
- A slow observer must not block PTY consumption or topology mutation.
- Subscription gaps must be detected and surfaced as explicit resynchronization requirements.
- Shutdown must drain owned connection tasks, reap children, persist final metadata where promised, and remove runtime sockets cleanly.
- Failed spawn, persistence, or mutation operations must not leave addressable phantom state.

### 12.2 Performance and resource bounds

- Ordinary terminal use must remain responsive under the accepted output, resize, history, and multi-pane matrices.
- Idle behavior must remain event-driven; optional features must not add material idle work when unused.
- Memory ownership and high-water behavior must be measurable for terminal history, publication queues, image content, transfers, and renderer caches.
- Performance work must preserve correctness, bounded memory, small-write latency, and resynchronization contracts.
- Quantitative gates and retained evidence belong in public benchmark and validation records rather than being duplicated here.

### 12.3 Compatibility

- Splinterm-owned versioned tests and fixtures are terminal behavior and release authority; Foot 1.27.0 at the pinned commit is an optional historical differential.
- The validated primary platform is x86_64 Arch/Omarchy with native Wayland under the documented Hyprland environment.
- Broader compositor, distribution, Nix, sandboxed-package, or hardware claims require separate implementation and evidence.
- Public JSON/NDJSON schemas and documented operation semantics are compatibility boundaries; private daemon frames and Rust types are not.

### 12.4 Accessibility and input

- Trusted overlays must provide keyboard operation, visible focus/selection, safe cancellation, and pointer isolation.
- Selection must not rely on color alone.
- IME/preedit, compose fallback, fractional scale, focus indication, and reduced-motion behavior must remain functional within the accepted native-client scope.
- Destructive confirmation must default to cancellation.

### 12.5 Maintainability

- Domain types must remain independent of rendering, Wayland, PTYs, async runtimes, and wire formats as documented by crate boundaries.
- Graphical clients, automation clients, and transport adapters must remain replaceable without becoming persistent-state authorities.
- Every significant architecture change must be recorded in an ADR; implementation slices must retain focused validation evidence.

## 13. User experience requirements

### 13.1 First-run workflow

A supported user should be able to:

1. install a verified package;
2. launch a fresh terminal through the desktop or XDG path;
3. create panes and Dojos using discoverable native controls;
4. close or detach the graphical view without ending the shell;
5. reopen the running Dojo from Recent Dojos or reopen-last; and
6. understand when an operation will terminate a process or lose state.

### 13.2 Interaction rules

- Fresh launch and reopen must remain distinct.
- Window/tab focus is client-local and must not move another client's focus.
- Tab close is non-destructive detach; Splint/Dojo termination is explicitly destructive.
- Unavailable actions remain visibly unavailable rather than silently selecting another target.
- Trusted surfaces consume relevant input and prevent click-, paste-, IME-, focus-, selection-, URL-, and terminal-mouse-through.
- The command palette remains a closed application-owned catalog; terminal output and plugins cannot add trusted actions.
- Error messages must identify stale state, denied authority, unsupported behavior, and recovery steps without exposing sensitive bodies.

## 14. Success criteria

### 14.1 Product success

Splinterm meets its defining product promise when:

- daemon-owned shells and layouts remain live across graphical client disconnect and reconnect;
- a user can complete the primary Omarchy terminal workflow without an external multiplexer;
- native windows, panes, tabs, history, search, clipboard, control, and theme behavior pass their accepted validation gates;
- automation can inspect and operate the same topology through explicit least-privileged policy;
- no automation path receives trusted graphical authority by implication;
- Foot-derived behavior and intentional divergences remain reproducible and documented; and
- optional capabilities remain bounded and do not regress ordinary terminal use.

### 14.2 Original stable-release readiness criteria

The following criteria are retained from the pre-0.1.0 product plan. They do not
mean the published stable 0.1.0 release is still beta, or attest that every
original support-policy aspiration shipped. [Current status](status.md) and the
published release notes define the actual release scope; broader compatibility
and a support lifetime remain unpromised.

1. a current status document defines supported environments, validated capabilities, known limitations, deferred work, and open gates;
2. product, usage, CLI, installation, configuration, security, troubleshooting, and automation documentation have clear authoritative homes;
3. README, website, package, desktop, AppStream, and Cargo descriptions use consistent product and maturity language;
4. versioned immutable public artifacts and checksums are available;
5. install, upgrade, daemon restart, active-shell loss, rollback, and compatibility expectations are documented and validated;
6. a public compatibility and support policy exists;
7. machine schemas and examples are published with their compatibility rules;
8. licensing and Foot provenance are complete in source and packages;
9. all release-blocking non-graphical and approved graphical matrices pass on a clean committed build; and
10. independent product/readability and technical-accuracy reviews have no unresolved blockers.

### 14.3 Documentation comprehension

A new evaluator should be able to answer within one minute:

- What is Splinterm?
- Why does daemon-owned persistence matter?
- How do humans and bounded automation share one topology?
- Which environment and capabilities are validated?
- What does stable 0.1.0 promise, and what remains outside its scope?
- What is the first safe workflow?

## 15. Risks and mitigations

| Risk | Product impact | Mitigation |
|---|---|---|
| Persistence is mistaken for crash/reboot resurrection | Users risk losing active work | Use explicit lifetime language at install, upgrade, restore, and shutdown boundaries. |
| Automation is marketed as unrestricted “AI control” | Authority and trust boundaries become unclear | Use “bounded automation”; document identity, scopes, revocation, control, and untrusted output. |
| Product documentation drifts behind accepted implementation | Users see contradictory maturity and feature claims | Establish one status authority and link specialist documents instead of duplicating claims. |
| Foot compatibility claims exceed evidence | Applications behave unexpectedly | Keep the pinned oracle, exact scope language, and differential gates. |
| Multiplexer concepts become harder than tmux | Primary workflow becomes inaccessible | Lead with user outcomes, native controls, clear vocabulary, and discoverable trusted menus. |
| Optional images or history regress ordinary use | Core terminal responsiveness and memory suffer | Preserve explicit budgets, no-image gates, event-driven expiry, and benchmark matrices. |
| Trusted UI and terminal content blur together | Spoofing or accidental authority | Keep trusted chrome visually distinct and input-isolated; never derive authority from terminal content. |
| Stable 0.1.0 is mistaken for broader support | Users depend on compatibility or a support lifetime the release does not promise | Keep validated-platform limits and daemon-upgrade warnings prominent; distinguish prereleases from stable releases. |
| Platform expansion dilutes the validated Omarchy path | More environments than the project can test | Require separate evidence and support decisions for each platform. |

## 16. Open product decisions

These decisions remain outside the current accepted product baseline:

1. stable release versioning, channels, and support duration;
2. exact public compatibility guarantees beyond current machine schemas and documented subsets;
3. Nix, Home Manager, and tertiary-distribution scope;
4. compositor support beyond the validated Hyprland/Omarchy environment;
5. sandboxed packaging and daemon/socket integration;
6. whether and how to support advanced Kitty graphics, placeholders, external media, or animation;
7. whether client-native compositor/window automation receives a separate trusted graphical broker;
8. public issue/support/security-reporting processes;
9. telemetry policy—currently no telemetry requirement is defined; and
10. whether any safe extension model for trusted commands should exist beyond the closed application-owned catalog; and
11. the exact persistence scope, icon source policy, ordering semantics, action allowlist, and theme precedence for the planned post-alpha3 user-customizable tab system.

Each material decision should receive an ADR or a scoped PRD/roadmap change before changing compatibility or authority boundaries.

## 17. Documentation authority map

| Subject | Authority |
|---|---|
| Normative product requirements and release criteria | This PRD after review and acceptance |
| Current maturity and availability | [status.md](status.md) |
| Product entry point | [README.md](../README.md) |
| Human operation and controls | [usage.md](usage.md) |
| Human and machine command inventory | [cli.md](cli.md) |
| Architecture and ownership | [architecture.md](architecture.md) |
| Architecture decisions | [docs/adr/](adr/) |
| Product direction, audiences, and outcome horizons | [product-roadmap.md](product-roadmap.md) |
| Human configuration and bindings | [configuration.md](configuration.md) |
| Machine contracts and policy | [automation.md](automation.md) |
| MCP integration | [mcp.md](mcp.md) |
| SSH remote access | [remote.md](remote.md) |
| Headless operation | [headless.md](headless.md) |
| Terminal images | [images.md](images.md) |
| Packaging and installation | [packaging.md](packaging.md) |
| Integration-author workflows | [integrations.md](integrations.md) |
| Validation evidence | Retained tests, benchmark artifacts, and release records |
| Foot lineage and licenses | [ADR 0001](adr/0001-foot-rust-port.md), [THIRD_PARTY.md](../THIRD_PARTY.md), and [LICENSE](../LICENSE) |

## 18. Source documents used for this draft

This draft synthesizes the current implementation and, principally:

- [README](../README.md)
- [Architecture](architecture.md)
- [Product roadmap](product-roadmap.md)
- [ADRs](adr/)
- [ADR 0011: guarded in-place daemon re-exec](adr/0011-guarded-in-place-daemon-reexec.md)
- [ADR 0012: defer durable terminal-body archives](adr/0012-defer-durable-terminal-archives.md)
- [ADR 0014: easy agent access](adr/0014-easy-agent-access.md)
- [Supported automation contracts](automation.md)
- [Configuration and Foot migration](configuration.md)
- [Arch packaging](packaging.md)
- [Terminal image compatibility](images.md)

## 19. Draft review questions

Before accepting this PRD, reviewers should decide:

1. Is “persistent, security-conscious terminal substrate for humans and bounded automation” the accepted product definition?
2. Are Omarchy/Arch developers the correct primary user, with operators and integration authors as secondary users?
3. Does the current baseline accurately distinguish implemented, validated, supported, proposed, deferred, and unreleased behavior?
4. Are the P0 requirements truly release-blocking?
5. Does any requirement accidentally promise daemon-crash, reboot, or upgrade continuity?
6. Does any automation language imply trusted graphical or compositor authority?
7. Are any current capabilities missing, or are implementation details incorrectly elevated to product requirements?
8. Which stable-release gates require a product decision rather than more implementation?
9. Which remaining PRD draft questions require a later product decision beyond the accepted current status authority?
10. Which success criteria should gain quantitative thresholds before stable release?
