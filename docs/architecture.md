# Architecture

[ADR 0001](adr/0001-foot-rust-port.md) establishes Foot as the authoritative
implementation and behavioral foundation for Splinterm's Rust terminal port.
The boundaries below reorganize ownership for persistence; they do not replace
Foot with another terminal engine.

Splinterm has two lifetimes: the graphical client may come and go, while the
daemon owns terminal processes and session state. A third-party automation
client, including a coding agent or MCP adapter, is also disposable and receives
no authority from running inside a Splint.

```text
local native splinterm
  ├─ trusted local endpoint ───────────────→ owner-only splinterd Unix socket
  └─ remote endpoint (native Window, one endpoint per lifetime)
       └─ one OpenSSH child
            └─ splinterm relay --graphical-stdio
                 └─ bounded logical channels
                      └─ independently validated remote splinterd Unix connections

splinterd → Topology → Lairs → Dojos → Splints
```

## Crate boundaries

### `splinterm-core`

Transport- and UI-independent state. A `Topology` catalogs named persistent
`Lair` sessions. Each `Lair` owns zero or more `Dojo` layouts, and each `Dojo`
owns a binary layout tree whose leaves are `Splint` terminal surfaces.

This crate must not depend on Wayland, async runtimes, PTYs, or a wire format.

### `splinterm-protocol`

Request/response types shared by both processes. The current development
protocol uses bounded length-prefixed JSON frames, version-range negotiation,
request IDs, peer-UID verification, stable errors, and explicit subscription
resynchronization. Protocol v27 carries closed access scopes, grant status,
revocation events, bounded direct command argv/working-directory/shell launch
fields, semantic terminal updates, topology and history generations, stable row
IDs, revision-bound scrollback/search pages, visible-row identity, and bounded
per-Splint control-status/transfer events. Command arguments
are never rebuilt as a shell string. Protocol DTOs remain separate from
terminal and daemon runtime structs.

### `splinterd`

Owns the authoritative Topology and all PTY file descriptors, child
processes, scrollback, and durable snapshots. Clients should be disposable
without ending sessions. The daemon must not depend on Wayland and should run
on headless Linux hosts such as `neuromancer`.

For ungranted terminal access, the daemon launches a disposable sibling
`splinterm consent` process over a private inherited Unix socket. Random
one-use capability material and the bounded grant-once/deny exchange stay on
that channel. In-memory grants bind peer UID/PID/executable identity,
Splint/incarnation, and explicit scopes. Revocation releases tied controllers
and subscriptions; bounded audit metadata excludes terminal, clipboard, and
input bodies. Per-Splint controller leases bind daemon-owned connection IDs.
Control observers receive subscriber-specific status without peer identity;
transfer requests are bounded, owner-decided, timeout-denied, and cancelled on
either disconnect. Forced transfer uses a separate trusted consent prompt,
"Just this once" only: the daemon's own dialog for the installed client, and
for a remote Window, whose relay must run outside every Splint, an offer to
that Window's own consent responder alone (ADR 0015), failing at once without
one and never falling back to this host's display.
A Window that is not a pane's controller takes it when a person resizes the
pane (`ReclaimControl`): no prompt, from any holder, an agent included. The
daemon accepts it only from the installed client and from remote Windows'
relays running outside every Splint, and no policy grants it; the connection
that lost the lease is told a Window took it, not which. ADR 0007 and ADR 0010
list the residual risks.
Literal search runs inside the owning terminal actor with fixed query/result/
preview/deadline bounds and opaque cursors tied to incarnation, terminal
revision, and history generation. See
[ADR 0005](adr/0005-trusted-consent-broker.md) and
[ADR 0006](adr/0006-multiplexing-lifecycle.md).

### `splinterm`

This is the disposable Wayland terminal UI, responsible for presentation,
local input, clipboard/IME integration, and rendering state received from the
daemon. Its private consent mode renders fixed trusted application chrome and
never accepts requester-supplied terminal content. Normal windows keep active
authority, development bypass, controller state, pending transfer decisions,
and the local search surface visibly indicated. Search focus, query, match
selection, and viewport position remain client-local. A managed native Window
also owns an ordered, non-persistent set of at most 32 Dojo tabs and one active
tab. Tabs never enter `splinterm-core`, daemon topology, policy, audit, public
CLI/MCP data, or child context. The same Window may reference Dojos from
multiple Lairs, while duplicate opens activate the existing local tab.

Open tabs retain bounded terminal subscriptions. Hidden tabs drain semantic
updates without painting, blinking, resizing, focus reporting, or controller
ownership; activation installs cached frontend state and performs one active
geometry reconciliation. Closing a tab releases its client resources without
closing the referenced Dojo or Splints. The tab strip is trusted application
chrome, and one content rectangle below it is authoritative for pane layout,
terminal geometry, input mapping, overlays, IME, and damage.

The local daemon admits at most 128 simultaneous private protocol connections.
This remains a hard resource bound while accommodating the current graphical
model's independent observation and control connections for one 32-tab Window,
its topology/theme channels, transient human inspection, and cleanup headroom.
Image-body transport retains its separate eight-connection limit. Connection
pooling or protocol-channel multiplexing may reduce this footprint later; the
higher bounded admission ceiling does not expand authorization or per-connection
subscription, frame, queue, controller, or image budgets.

Terminal images remain sparse protocol-independent content and placement
records in the daemon-owned terminal state. Trusted UI snapshots carry bounded
metadata only; immutable pixel bodies are fetched on demand through sealed
memfd or a separate bounded binary socket and retained under one renderer-wide
source budget. The client composes premultiplied BGRA into its existing CPU
backing/Wayland SHM path, clipped independently per pane. Public automation,
audit, and relay records never expose image bodies. See [images.md](images.md)
for the compatibility and resource matrix.

The graphical identity is fixed to `com.oldjobobo.splinterm` across Wayland and
desktop metadata. `splinterm launch` is the `xdg-terminal-exec` boundary.
Client-owned configuration controls renderer/window policy; a generated,
project-owned theme maps Omarchy roles and reloads without mutating daemon
terminal state or process lifetime.

A daemon `Dojo` is persistent topology, not a compositor-native surface.
Automation may create or mutate Dojos and their Splint trees without mapping a
native `Window`. Mapping, focusing, moving, resizing, or assigning a Window to a
compositor workspace requires a separate trusted graphical broker and is not
implied by topology mutation or the persisted default-focus hint.

### `splinterm-graphical-relay` and `splinterm-relay`

The shared graphical-relay crate owns only the bounded outer framing and the
client-side logical byte-channel router. It does not depend on or parse the
private daemon protocol. Exact magic/version, channel identities, data and
diagnostic sizes, channel count, per-channel queues, and aggregate queues fail
closed.

`splinterm-relay --stdio` remains one byte-transparent automation connection.
The distinct `--graphical-stdio` mode accepts outer channel requests and repeats
socket path, owner/mode, peer UID, pidfd lifetime, and exact adjacent daemon
executable validation for every channel. Daemon EOF is channel-local; validated
daemon process exit, SSH/stdin EOF, corrupt outer framing, or aggregate-bound
failure closes the complete session.

### Remote endpoint foundation

`remote.rs` owns strict credential-free profile parsing and structured OpenSSH
argv. `remote_session.rs` owns one SSH child, separate bounded stderr drain,
outer negotiation, cloneable logical-channel creation, shutdown/kill/reap, and
categorized transport failures. It never reads key material or stores password
or passphrase values. `remote_askpass.rs` owns the native fallback prompt: a
per-attempt private socket and token that the client binary, run by OpenSSH as
`SSH_ASKPASS`, uses to ask the launching status Window, plus the wiped,
non-reallocating buffers that carry the answer to OpenSSH.

`endpoint.rs` defines the behavior carried with a clonable connection factory:
local uses `TrustedUi`, trusted local image content, focus publication, local
launch semantics, trusted force-transfer authority, and the `local` recency
namespace; remote uses `RemoteInteractive`, unavailable image transport,
disabled focus publication, forced transfer confirmed in the Window itself
through its consent responder (Plan 0072 M3), remote-daemon launch defaults,
and `remote-PROFILE` recency. OpenSSH authenticates the human remote
account; persistent automation policy is not part of this path. The daemon
accepts the role only from the adjacent relay running `--graphical-stdio`, while
raw `--stdio` remains `Automation`. Phase 2 routes CLI selection, session
discovery, Window startup, pane observation/control, topology reconciliation,
hidden tabs, history, search, mutations, and cleanup through clones of that one
factory. A Window therefore cannot retarget or mix local and remote identities
after startup.

## Client module boundaries

The `splinterm` binary entry point performs only Tokio runtime setup and calls
one private application entry point. Binary-owned orchestration lives under
`app/`:

| Area | Ownership |
| --- | --- |
| `app/commands.rs` | Leaf-only Clap grammar and value conversions |
| `app/cli.rs` | Output-mode selection and human command routing |
| `app/machine/` | Stable JSON/NDJSON requests, envelopes, deadlines, and subscriptions |
| `app/local_service.rs`, `consent.rs`, `sessions.rs` | Local policy/reset/relay services, trusted consent, session selection, and picker presentation |
| `app/remote_cli.rs` | Strict profile listing/inspection and non-mutating remote reachability checks |
| `app/session_catalog.rs` | Endpoint-aware local/remote-safe launch requests, namespaced recent-session state, and picker projections |
| `app/window.rs` | Graphical task startup, renderer configuration, image-cache setup, and single-/multi-pane lifecycle coordination |
| `app/pane_bridge.rs` | Bounded daemon-to-frontend pane subscriptions, control, resize, image leases, and resynchronization |
| `app/topology_manager.rs` | Per-Dojo async task ownership and topology reconciliation |
| `app/theme_watch.rs` | Bounded theme observation and publication |

Application services do not own Smithay objects, Wayland proxies, SHM buffers,
or renderer frames. As the terminal-window composition boundary, `app/window.rs`
configures the public renderer facade, creates the bounded shared image-content
cache, and maps `WindowOptions` through `run_window`. The Dojo-picker path in
`sessions.rs` also configures the renderer and calls the same public window
facade for its transient trusted UI. The async pane and topology services
exchange frontend contracts, image leases, and protocol data without depending
on Wayland or renderer frames. Command grammar and session-catalog
helpers are neutral leaves, so machine/local-service dispatch and topology/window
orchestration have no reverse dependency on their callers.

Library-owned graphical code has one-way internal dependencies:

```text
frontend contracts ───────────────┐
                                  ▼
renderer ← geometry/config    wayland facade
   ▲                              │
   └──────── explicit frames ─────┘

app/cli → commands + machine/local/session/window services
app/window + Dojo picker → renderer configuration + public wayland facade
app/pane_bridge + topology_manager → frontend contracts + daemon protocol
sessions + topology_manager → neutral session_catalog
```

`frontend/` owns platform-neutral window messages, options, picker state, and
topology contracts. `renderer/` owns immutable process resources, explicit
per-window render contexts, prepared frames, text/raster/image composition,
overlays, and deterministic capture. `wayland/` owns compositor state, SHM,
input/IME/clipboard, damage scheduling, graphical tabs and chrome, and thin
protocol dispatch. Its `App` composes cohesive platform, surface, presentation,
input, clipboard, panes, tabs, modal, and scheduling state rather than exposing
one flat mutable object. Generic tab policy remains in `tab.rs`; graphical tab
presentation remains in `wayland/tabs.rs`; daemon task ownership remains in
`app/topology_manager.rs`.

Only the established library facades are public. Reducers and dispatch seams use
private, `pub(super)`, or `pub(in crate::wayland)` visibility. Binary application
internals use `pub(in crate::app)`; only the single application entry point is
visible to the crate-root binary.

## Automation and coding-agent boundary

The supported path for a coding agent is a least-privileged automation client:
the stable JSON/NDJSON CLI or the separately packaged and authorized
`splinterm-mcp` stdio adapter. Both use the same daemon operations and policy checks;
neither inherits trusted-UI authority. If an agent shells out to the general
`splinterm` CLI, the daemon authorizes that CLI executable, so every process able
to invoke that exact binary can exercise the rule's scopes. The dedicated MCP
binary exists to provide a narrower independently reviewable identity.

Splinterm provides topology, PTYs, terminal observation, process lifecycle, and
exclusive input/resize control. It does not itself provide semantic agent task
status, inter-agent messaging, readiness, completion, or result transport. A
higher-level orchestrator may build those semantics over structured process
launch and bounded observation, but terminal content remains untrusted data and
must never become authority or an automatic instruction.

PTY children receive daemon-overridden logical context hints for their Lair,
Dojo, Splint, and current incarnation. These values improve discovery for an
in-Splint agent but are not credentials and are never accepted as proof of
resource authority. Relaunch replaces the incarnation, and supported clients
reconcile every hint against current public topology before selection.

## Packaged deployment

The main Arch package installs four adjacent runtime executables—`splinterm`,
`splinterd`, `splinterm-relay`, and `splinterm-pty-child`—plus the public-CLI-only
`splinterm-dojo-picker` reference integration, an on-demand systemd user
service, xdg launcher/desktop metadata, icon/AppStream metadata, theme generator,
examples, and license notices. The launcher starts the daemon
and performs one bounded restart when protocol negotiation reveals a stale
pre-upgrade process. The headless-capable unit does not require graphical
display variables, preserves them for PTY children when the graphical session
provides them, strips the unsupported development authorization bypass, loads
only an optional owner-controlled environment file, uses SIGHUP for atomic
fail-closed policy reload, and uses SIGINT for graceful child/socket cleanup.
The optional `splinterm-mcp` split package adds only the independently
policy-identified stdio adapter, its setup guide, and license notices; installation
or MCP host configuration grants no daemon authority. Policy validation and inspection reuse the daemon's secure loader locally;
reload remains systemd control rather than an ordinary socket RPC. Package
scripts do not edit user homes, enable lingering, change SSH policy, or mutate
Omarchy-owned directories; see [headless.md](headless.md),
[integrations.md](integrations.md), [mcp.md](mcp.md), and
[packaging.md](packaging.md).

## Accepted 0.2 upgrade target (not implemented)

[ADR 0011](adr/0011-guarded-in-place-daemon-reexec.md) replaces the current
stale-protocol restart behavior only after their implementation gates pass. A
fully negotiated compatible upgrade uses guarded descriptor-based in-place
re-exec so the daemon PID, child parenthood, PTYs, terminal incarnations, and
one-reader ownership can survive. Compatibility is negotiated by explicit
protocol and schema ranges; `0.2.x` membership alone is insufficient.

The next human launcher invocation automatically performs a compatible handoff
after bounded preflight. After the bootstrap boundary, an idle daemon may restart
automatically, while an incompatible daemon with live Splints blocks until the
user confirms an exact-count destructive fallback. Package scriptlets never
initiate a user-service handoff or restart. The first `0.1.x` to handoff-capable
`0.2.0` transition is a one-time confirmed destructive bootstrap boundary even
when the old daemon is idle.

Handoff preserves the validated listening socket descriptor but disconnects
accepted clients and expires connection-owned authority. Each eligible local
Window carries only a bounded anonymous resume record for ordered Dojo tabs,
active tab, focused panes, and exact old connections across the exact sealed
adjacent-client snapshot exec. After recreating its connections and fully
resnapshotting, it may reclaim only its prior controller disposition through the single-use
resume claim defined by ADR 0011. The claim binds the same surviving process by
monitored pidfd and kernel-supplied message credentials, the immutable sealed
client-snapshot identity, and the new daemon generation. The normal active pane
resumes input without a click after the trusted **input paused** handoff state clears; transfer, replay,
mismatch, or conflict remains visibly view-only. Remote graphical clients
reconnect and reauthenticate, and automation authority never crosses the
generation boundary.

[ADR 0012](adr/0012-defer-durable-terminal-archives.md) keeps every body-bearing
handoff checkpoint in an anonymous sealed memory-backed descriptor so even
abrupt failure leaves no named artifact. `0.2.0` daemon-loss and reboot recovery
remains recipe-only and persists no terminal grids, scrollback bodies, image
bodies, parser state, replies, or input. Crash continuity and durable terminal archives remain
separately gated post-`0.2.0` work.

## Foot reference map

The local Foot source at `~/Playground/foot` suggests useful subsystem seams:

| Foot area | Splinterm destination |
| --- | --- |
| `client.c`, `server.c`, `client-protocol.h` | protocol + daemon transport |
| `terminal.c`, `commands.c`, `csi.c`, `osc.c`, `dcs.c` | future terminal engine crate |
| `grid.c` | future screen/scrollback model |
| `slave.c`, `spawn.c`, `reaper.c` | daemon PTY/process ownership |
| `render.c`, `shm.c` | client renderer |
| Wayland/input/IME modules | client platform layer |

Splinterm should preserve this separation while changing the ownership model:
terminal processes and canonical screen state belong to `splinterd`, not the
Wayland client's lifetime.

## Near-term invariants

1. A client crash must not terminate a splint.
2. Persistent state has one writer: `splinterd`.
3. Domain types do not know about rendering or transport.
4. The protocol is versioned and rejects incompatible peers.
5. Ported Foot code retains MIT attribution and provenance.
6. Headless automation uses the dedicated, exact-policy raw relay over
   authenticated SSH. Native human remote transport uses one authenticated SSH
   child with bounded logical channels negotiating `RemoteInteractive`; it does
   not consult automation policy or receive trusted image or graphical-focus
   authority; its forced transfer is confirmed in the remote Window itself. `splinterd` exposes no network listener.
7. Shutdown owns and drains connection tasks before runtime shutdown and final
   metadata persistence; one pinned signal future prevents lost SIGINT events.
8. Dojo mutation never claims native Window mapping or focus; Window-local tab
   operations never claim daemon topology mutation.
9. In-Splint context and terminal-derived data are discovery inputs, never
   authorization, consent, or executable instructions.
10. Future-descendant authority must be explicit and bounded; containment alone
    cannot silently broaden an automation rule.
