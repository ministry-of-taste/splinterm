# Headless service and policy administration

`splinterd` owns Unix sockets, PTYs, persisted topology metadata, authorization,
and bounded audit metadata. It does not require or connect to Wayland or X11,
so the service works when `DISPLAY` and `WAYLAND_DISPLAY` are absent. When they
are present, the daemon preserves them for PTY children launched from graphical
sessions. The packaged unit removes the unsupported
`SPLINTERM_ENABLE_DEV_ATTACH` development bypass after loading its environment
file; graphical `splinterm window` clients remain separate processes.

## Service lifetime

The package installs but does not enable `splinterd.service`. Start it on demand:

```bash
systemctl --user start splinterd.service
splinterm ping
```

The desktop launcher also starts it on demand. To start it whenever this account's
systemd user manager reaches `default.target`:

```bash
systemctl --user enable --now splinterd.service
```

A normal non-lingering user manager usually stops at logout, which stops the
daemon and its shells. Persistent post-logout service requires an administrator's
conspicuous decision:

```bash
sudo loginctl enable-linger ACCOUNT
```

The package and its scripts never enable lingering, modify SSH policy, create a
service account, or enable the unit. For a dedicated service account, the
administrator must provision the account, its home/runtime ownership, login or
lingering policy, and the exact automation executable policy independently.

## Resource guards and workload containment

The packaged daemon separates its control plane from terminal workloads before
commands execute:

```text
splinterd.service                  daemon control plane
app-splinterm.slice                all terminal workloads
└── one transient slice per Dojo
    └── one transient scope per live Splint
```

`splinterd.service` retains `TasksMax=2048` and `MemoryHigh=75%` as an
independent control-plane guard. Terminal workload units do not set `TasksMax`:
each Splint scope receives the systemd user manager's normal `DefaultTasksMax`,
subject to any stricter administrator or ancestor policy. This matches ordinary
application-scope behavior and avoids rejecting legitimate thread-heavy browser,
test, editor, build, and agent workloads.

The aggregate workload slice retains `MemoryHigh=75%`; transient Dojo slices use
50% and Splint scopes use 25% memory pressure. `MemoryHigh` asks the kernel to
reclaim and throttle under sustained pressure; Splinterm does not install a
`MemoryMax` hard kill boundary in this release.

A new Splint is rejected if its exact helper PID cannot be verified inside its
scope while still blocked before target execution. Existing Dojos are not
retargeted or silently launched inside the daemon service. Direct development
launches use containment when a user manager is available and otherwise emit a
bounded warning; only the packaged service passes the strict
`--require-workload-cgroups` mode.

Inspect both boundaries without changing them:

```bash
systemctl --user show -p DefaultTasksMax
systemctl --user show splinterd.service \
  -p TasksCurrent -p TasksMax -p EffectiveTasksMax \
  -p MemoryCurrent -p MemoryHigh -p MemoryMax
systemctl --user show <splint.scope> \
  -p TasksCurrent -p TasksMax -p EffectiveTasksMax
systemctl --user status app-splinterm.slice
systemd-cgls --user-unit app-splinterm.slice
systemd-cgtop
```

`EffectiveTasksMax` is authoritative because systemd applies the strictest limit
from the scope and its ancestors. Existing transient scopes created by an older
daemon retain their old explicit task ceiling until that workload hierarchy is
recreated; package installation does not restart the daemon automatically.

`MemoryCurrent` includes page cache charged to each cgroup. Inactive file cache
is normally reclaimable under pressure, but it is neither immediately free nor
excluded from cgroup accounting. Do not use `drop_caches` as routine Splinterm
recovery.

The aggregate workload slice may be inactive when no Splints are running. Empty
transient Splint scopes and Dojo slices are collected after runtime cleanup.
Packaged Dojo slices are also `PartOf=splinterd.service`: stopping or failing the
daemon service stops its workload hierarchy, while a Splint or Dojo stopping
does not propagate failure back to the daemon.

## Install an owner-only policy

The unit optionally reads `%h/.config/splinterm/daemon.env`. `EnvironmentFile`
contents are not shell code and later interactive-shell exports do not alter an
already running user manager. `SPLINTERM_POLICY` must be an absolute canonical
path.

Create policy files without a permissive intermediate file:

```bash
install -d -m 700 "$HOME/.config/splinterm"
umask 077
policy_tmp=$(mktemp "$HOME/.config/splinterm/policy.json.XXXXXX")
cat >"$policy_tmp" <<'JSON'
{
  "schema": "splinterm.policy.v2",
  "rules": []
}
JSON
chmod 600 "$policy_tmp"
mv -f "$policy_tmp" "$HOME/.config/splinterm/policy.json"

printf 'SPLINTERM_POLICY=%s\n' "$HOME/.config/splinterm/policy.json" \
  >"$HOME/.config/splinterm/daemon.env"
chmod 600 "$HOME/.config/splinterm/daemon.env"
```

An empty rule list is the explicit deny-all starting point. Add only reviewed
absolute executable paths, SHA-256 digests, closed scopes, exact resources, and
bounded limits described in [automation.md](automation.md). Never grant a shell,
interpreter, writable executable, or broad wrapper unless all code it can execute
is intended to receive that authority.

Authorizing the general `splinterm` CLI delegates the selected rule to every
same-account process able to invoke that exact binary; running inside a Splint
does not narrow or grant that authority. This can support a supervised CLI-based
coding agent, but the optional `splinterm-mcp` split package uses its own exact
executable identity for a narrower production boundary; see [mcp.md](mcp.md).
Lair and Dojo rules snapshot only resources present when the policy generation
is published. To
authorize a newly created child, review the concrete resource, update policy,
reload the service, and reconnect; broad future-descendant authority is not part
of policy v2.

Validate and inspect the file offline through the daemon's exact secure loader:

```bash
splinterm policy validate "$HOME/.config/splinterm/policy.json"
splinterm policy inspect "$HOME/.config/splinterm/policy.json"
```

Validation enforces the daemon's owner, mode, hard-link, no-symlink, size, JSON,
and semantic rules. Inspection prints normalized validated JSON; it does not
query or mutate the running daemon.

After editing, validate first and request a reload through the canonical unit:

```bash
splinterm policy reload
```

This command reports only that systemd delivered the reload request. Confirm the
result through the user journal or authorized bounded audit inspection:

```bash
journalctl --user-unit splinterd.service -n 30 --no-pager
splinterm --output json audit --max-records 16
```

Reload is atomic and fail-closed. A rejected file installs a new deny-all
policy generation. Every reload disconnects automation-role clients and revokes
their connection-owned subscriptions, controller leases, and pending transfers;
automation clients must reconnect explicitly. Local and SSH-remote human
graphical clients do not use persistent policy and remain connected.

## Agent access prompts

A headless computer cannot show an [easy agent access](automation.md#easy-agent-access)
prompt itself, so a request that needs one fails at once with
`consent_unavailable`: "no Splinterm Window or approver connected to this
computer can show the prompt". Policy rules keep working without one. A
Splinterm Window connected to this computer over SSH shows the prompt and
answers it (see [remote Windows](remote.md#agent-access-prompts)). To answer
such prompts without a Window, run `splinterm approvals` directly in your own
terminal on that computer, for example in an SSH login, not inside a Splinterm
pane or a tmux or screen session
([Usage](usage.md#answering-access-prompts-in-a-terminal)).

The daemon refuses an approver that runs inside a Splint's or the requesting
agent's process tree or session, or, when workloads run in systemd units,
inside a Splint scope or Dojo slice, and it never offers a request to an
approver inside that agent's tree or session. A same-user process that leaves
all of these on purpose, or anyone holding your SSH key for this computer, can
still see and answer prompts, and a program that types into an approver run
inside tmux or screen can answer them. A bogus approver can also deny a
request or leave it waiting until it expires, and one process may hold at most
2 of the daemon's 16 approver places. See
[ADR 0015](adr/0015-remote-consent.md#consequences) for the details.

## Runtime, state, restart, and recovery

The default socket is `$XDG_RUNTIME_DIR/splinterm/splinterd.sock` and is removed
on clean shutdown. Runtime directories are per-login and are not backups. Durable
metadata defaults to `$XDG_STATE_HOME/splinterm`, or
`$HOME/.local/state/splinterm` when `XDG_STATE_HOME` is unset. Policy files remain
under user configuration and are not written by the daemon.

A daemon restart terminates daemon-owned child processes. The packaged unit
allows 90 seconds for the daemon's bounded 30-second HUP and 30-second TERM
process-group grace periods, exit reconciliation, final metadata save, and socket
removal before systemd may force cleanup.
Persisted topology and launch metadata may remain, but saved commands are never
automatically executed; restoration requires an explicit authorized `restore`,
`restore-dojo`, or `restore-lair` command. Explicitly Saved and Pinned Lairs are
protected from automatic retirement; only fully exited Disposable Lairs are
eligible for the daemon's bounded capacity-retirement policy. Saving or pinning
is metadata-only and never starts, stops, resizes, or detaches a process. Saved
split ratios are proportional layout authority, while stored rows and columns
are bounded launch hints rather than a promise of identical pixel geometry.
Audit retention is daemon-lifetime-only and resets after restart.

To terminate every daemon-owned shell, move the complete session database to a
timestamped backup, restart the service, and wait for its socket in one guarded
command:

```bash
splinterm reset
```

The interactive command asks for confirmation. Use `splinterm reset --yes` only
for an already-approved unattended reset. It reports the reversible backup path;
policy and user configuration remain untouched.

For a consistent backup, stop the service, copy the state and policy files while
preserving owner/mode, then start it again:

```bash
systemctl --user stop splinterd.service
# Copy ~/.local/state/splinterm and ~/.config/splinterm using trusted local tools.
systemctl --user start splinterd.service
```

Before package upgrades, stop the daemon if its shells may be discarded. Private
protocol compatibility does not promise process migration across versions. After
upgrade run `systemctl --user daemon-reload` and start the service. If startup
fails, inspect the user journal, verify `XDG_RUNTIME_DIR`/`HOME`, validate the
policy, and ensure no unsafe stale object occupies the socket path. Do not remove
unknown files or weaken ownership checks to force startup.

`splinterm upgrade` reports whether the running daemon could hand its terminals
to the installed build without a restart, and `splinterm upgrade --now` starts
that live upgrade. Nix installations are refused for all of Splinterm 0.2.0, and
whenever the command reports live upgrade as unavailable, the restart procedure
above remains the upgrade path. The command never restarts the daemon itself.
See [Daemon upgrade](cli.md#daemon-upgrade) for its prompts, trust rule, and
exit statuses.

## Accepted 0.2 upgrade contract (launcher states implemented)

The guarded in-place re-exec contract in
[ADR 0011](adr/0011-guarded-in-place-daemon-reexec.md) is enabled for `0.2.0`
through `splinterm upgrade --now`; the preceding restart procedure remains
authoritative wherever live upgrade is unavailable, such as for Nix
installations. The desktop launcher now implements the matching, idle-restart,
active-confirmation, and `0.1.x` bootstrap states below; see
[Desktop launcher and the running daemon](packaging.md#desktop-launcher-and-the-running-daemon).
The accepted `0.2.0` target distinguishes four paths:

- **matching** — the installed and running build identities match;
- **compatible** — explicit handoff and checkpoint ranges overlap, so a guarded
  in-place handoff can run. The first initiator is the explicit
  `splinterm upgrade --now` command; performing it automatically on the next
  human launcher invocation is planned for later;
- **idle incompatible** — no live Splint can be lost, so a bounded restart is
  allowed; and
- **active incompatible** — launch blocks until the user explicitly confirms a
  destructive restart showing the exact affected Splint count.

The first upgrade from `0.1.x` to a handoff-capable `0.2.0` daemon is a separate
one-time bootstrap path because the running old daemon cannot gain handoff
support retroactively. It always requires confirmation, including when the old
daemon may be idle; because a `0.1.x` daemon cannot report its live Splints, the
count shown is a best-effort count of its running workload units. Package scriptlets only install
files and report the boundary; they do not restart or hand off a user's service.

During a compatible handoff, trusted graphical chrome reports **input paused**.
The exact sealed replacement-client snapshot restores the Window's bounded
ordered-tab, active-tab, and focused-pane record, recreates its per-Dojo connections, and
fully resnapshots before the previously active pane accepts typing again without
a click or tab switch. A transferred, stale, invalid, or conflicting controller-
resume claim remains view-only and uses the ordinary Request Control flow. Remote and automation clients must
reconnect and reauthenticate without inheriting old-generation authority.

This planned-upgrade continuity does not cover daemon crash, logout without
lingering, reboot, or host loss. Under ADR 0012, body-bearing handoff checkpoints
use only anonymous sealed memory-backed descriptors, and `0.2.0` durable recovery
remains recipe-only and stores no terminal grids, scrollback bodies, image
bodies, parser state, replies, or input. See
[ADR 0011](adr/0011-guarded-in-place-daemon-reexec.md) and
[ADR 0012](adr/0012-defer-durable-terminal-archives.md).

"Memory-backed" means that a checkpoint is an anonymous, sealed `memfd`, passed
between daemon generations only as an open descriptor. It has no path: no file
in the runtime, state, or configuration directories and no `/dev/shm` entry is
named for it, and it ends with the last process holding it, including a
candidate killed mid-handoff. The generation that commits the adoption or
rollback releases every checkpoint. Terminal bodies stay out of the durable
Topology (primary, backup, and quarantined copies), the daemon's log stream,
its audit records, and its metadata responses. The `end_to_end` and
`handoff_exec_adoption` integration tests check these points with a sentinel
terminal body.

It does not mean that the bytes never reach a disk. Like the live terminal
state that `splinterd` always keeps in memory, checkpoint pages can be written
to swap or a hibernation image, and they appear in a core dump or crash report
of a daemon process that holds one. Splinterm does not lock that memory or
turn off core dumps. Swap encryption and the system's core-dump and crash
collection settings control those paths.
