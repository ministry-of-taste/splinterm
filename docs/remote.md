# SSH stdio relay

Splinterm supports remote automation through authenticated SSH without exposing a
TCP listener. The remote command `splinterm relay --stdio` replaces itself with
the dedicated `/usr/bin/splinterm-relay` transport. That process connects once
to the owner-only local daemon socket and copies opaque private-protocol bytes
between the socket and stdin/stdout.

The relay does not parse terminal content, frames, requests, responses, or
cancellation. It cannot mint authority or claim the SSH client's identity. The
remote client still negotiates the daemon protocol, owns request IDs, observes
revision/resynchronization rules, and sends cancellation frames itself. Image
pixel bodies remain restricted to the executable-verified trusted local UI and
its separate local content channel; public relay automation does not receive
those bodies or turn terminal-supplied Kitty paths/SHM names into host access.
See [images.md](images.md).

## Security boundary

SSH authenticates the host and login account. It does **not** authorize a
Splinterm operation. `splinterd` sees and authorizes the exact local
`splinterm-relay` executable, not the remote machine, SSH key, command line, or
human identity.

A policy rule for the relay delegates its listed scopes, resources, and limits
to every caller who can invoke that relay under the account. Use a dedicated
account or an SSH forced-command/restricted-key configuration when callers must
not receive the account's other capabilities. Those SSH controls are
administrator-owned and are never installed or modified by Splinterm.

The helper has a distinct executable identity from the normal `splinterm` CLI.
Package upgrades change its digest and intentionally require explicit policy
review. Obtain the installed identity with trusted local tools:

```bash
realpath /usr/bin/splinterm-relay
sha256sum /usr/bin/splinterm-relay
```

A minimal read-only rule is:

```json
{
  "schema": "splinterm.policy.v2",
  "rules": [
    {
      "id": "ssh-topology-reader",
      "executable": {
        "path": "/usr/bin/splinterm-relay",
        "sha256": "REPLACE_WITH_REVIEWED_SHA256"
      },
      "scopes": ["topology_metadata_read"],
      "resources": [{"kind": "daemon"}],
      "limits": {"max_results": 64, "deadline_ms": 5000}
    }
  ]
}
```

Create and validate the owner-only policy using the workflow in
[headless.md](headless.md), then request a service reload. Never grant a shell,
interpreter, writable relay copy, wildcard resource, or broader scope merely to
avoid maintaining exact policy.

## Connect through SSH

Pin and verify the server host key before unattended use. A fixed invocation is:

```bash
ssh -T \
  -o StrictHostKeyChecking=yes \
  -o UserKnownHostsFile="$HOME/.ssh/known_hosts" \
  ACCOUNT@HOST \
  /usr/bin/splinterm relay --stdio
```

`-T` is required: the relay rejects terminal stdin or stdout. Keep the remote
command fixed; do not interpolate terminal content or untrusted parameters into
an SSH command string. Relay stdout contains protocol bytes only. Bounded,
body-free diagnostics use stderr and must not be merged into stdout.

The relay requires an absolute normalized socket path. The socket must be a
non-symlink Unix socket owned by the account with mode `0600`, its immediate
canonical directory must be owner-only, the connected peer UID must match, and
a Linux 6.5+ `SO_PEERPIDFD` must bind the live peer to the exact opened
`splinterd` executable adjacent to the relay. The relay monitors that pidfd for
its complete connection lifetime. Unsafe `SPLINTERM_SOCKET` overrides and
same-UID substitute listeners fail closed
and are never used as fallback to another endpoint.

## Lifetime and failure behavior

Input EOF half-closes the daemon write direction and continues draining daemon
output. Daemon EOF, restart, socket failure, broken stdout, SSH death, or relay
death closes the relay connection promptly. The daemon then cancels that
connection's subscriptions and releases its controllers and pending transfers;
daemon-owned shells continue running.

A relay never reconnects automatically after daemon restart. Start a new SSH
relay and renegotiate explicitly. A user service normally ends at logout unless
an administrator enables lingering as described in [headless.md](headless.md).
Splinterm does not enable lingering, alter SSH policy, create keys, or install a
forced command.

The relay uses two fixed 16 KiB copy buffers plus bounded kernel pipe/socket
buffers. Backpressure stalls producers instead of creating an unbounded queue.
Malformed and oversized frames are forwarded unchanged; the daemon remains the
single protocol parser and rejects them under its normal frame limits.

Unix-socket forwarding is not a supported alternative in this slice. Its path
expansion, ownership, cleanup, and peer-identity behavior have not passed the
same review as the stdio relay.

## Native graphical client

The native remote Window uses a distinct transport:

```text
/usr/bin/splinterm relay --graphical-stdio
```

This mode is not byte-compatible with `--stdio`. It negotiates an exact bounded
outer protocol and maps independent logical channels to independently validated
daemon Unix connections. One local OpenSSH child therefore carries topology,
observation, control, and pane-task connections after one authentication. It
does not depend on OpenSSH ControlMaster or server `MaxSessions`.

Every logical daemon connection negotiates `ClientRole::RemoteInteractive`.
OpenSSH authenticates the human user and starts the installed graphical relay
under that remote Unix account; native remote Windows do not use automation
policy. The daemon accepts this role only from the adjacent
`splinterm-relay --graphical-stdio` process. It grants ordinary human terminal
and topology authority while withholding trusted-local compositor focus, image
content, and forced-control privileges. Closing SSH or the local session
releases connection-owned subscriptions and controllers but sends no Splint
termination request. Raw `relay --stdio` automation remains byte-compatible and
continues to negotiate `ClientRole::Automation` under persistent policy.

Select one profile globally to bind the complete native client lifetime to it:

```text
splinterm --remote PROFILE
splinterm --remote PROFILE dojos
splinterm --remote PROFILE reopen
splinterm --remote PROFILE window --lair-id LAIR_ID --dojo-id DOJO_ID
splinterm --remote PROFILE launch [--working-directory REMOTE_PATH] [-- ARGV...]
```

Omitting the subcommand after `--remote PROFILE` opens that endpoint's Recent
Dojo picker. Dojo discovery, tabs, pane snapshots and ordered updates,
resynchronization, scrollback/search, ordinary requested control/input/resize,
and ordinary lifecycle actions all use logical channels on the same SSH child.
New Lairs, Dojos, and Splints may be created, attached, controlled, and rendered
immediately without policy publication or Window restart. Trusted forced control
transfer remains visibly unavailable and is rejected before request construction.
A Window remains bound to exactly one endpoint: it binds when its terminal
opens and never changes, and the profile list, status screen, and Dojo list
that may come first in the same Window hold no endpoint. Local and remote
recency files are distinct, and profile names—not remote titles or CWDs—select the namespace.

Remote creation uses the existing remote-safe launch envelope: absent CWD and
argv cause the remote daemon to select its own home, shell, and defaults.
Split/relaunch inheritance is resolved by the remote daemon from the exact target
Splint, and the `new-dojo-directory`/`new-splint-directory` choices (`home`, a
path, or `~/path`) are resolved there too, against the remote owner's home. Because remote creation and attachment require several bounded protocol
round trips, a split immediately renders a noninteractive `Opening remote pane…`
placeholder before dispatching the mutation. One split may be pending per Dojo
tab. The client-local placeholder cannot receive focus or send terminal/topology
requests, never crosses the relay, and is replaced by the exact authoritative
Splint after attachment. A rejected mutation or unavailable/full command queue
removes only that placeholder and restores the original pane focus.

An explicit `--cwd`/`--working-directory` is an absolute remote path and
structured argv is never rebuilt as a shell string. Local shell settings and
local CWD are never default remote launch state. The envelope's historical Rust
type name is not an authorization role; remote graphical connections remain
human-interactive.

Profile inspection and reachability commands remain non-graphical:

```text
splinterm remote list
splinterm remote inspect PROFILE
splinterm remote check PROFILE
```

`check` starts the fixed graphical relay, negotiates one remote-interactive
channel, sends `Ping` and `ListLairs`, and exits without mapping a Window or
mutating topology. A successful check proves SSH, relay, daemon, and human-role
reachability.

### Open a remote without a terminal

Open a remote from the **Splinterm Remotes** entry in the app menu, the
**Remote Dojos** desktop action, or `splinterm remotes`. One Window lists the
profiles in `remotes.toml` by name and OpenSSH destination. Choosing a profile
shows its connection status, and any SSH prompt, in the same Window; then that
host's Dojos, with **New terminal** (the only row when the host has none); and
then the same Window becomes the remote terminal. Nothing is printed on a
terminal and no other Window opens.

Escape steps back one stage. From the status screen or a failure it returns to
the profiles, cancelling any connection in progress; from the Dojo list it
returns to the profiles and ends that SSH connection; at the profiles it closes
the Window. Closing the Window at any stage ends the launcher and its SSH
connection. If the chosen Dojo has ended by the time it opens, the Dojo list
shows again with the current Dojos. The profiles are read again each time the
list shows, so an edited `remotes.toml` applies on return.

`splinterm remotes` typed in a terminal returns at once with exit status 0 and
prints nothing: it starts the launcher again with no stdin, stdout, or stderr
and in a new session without a controlling terminal, so OpenSSH cannot prompt
on the terminal that ran it. OpenSSH uses an ssh-agent key or your own
`SSH_ASKPASS` first; when neither can answer, Splinterm asks on the status
screen (see [Authentication and host keys](#authentication-and-host-keys)).

**Open Remote…** in the command palette shows the profiles as a pop-up over the
current Window. Choosing one opens exactly one new Window, which shows the
connection status, that host's Dojos, and then the remote terminal; the local
Window only closes the pop-up, and Escape just closes it. The status screen and
its password field never appear inside a local terminal Window. The **Remote
Dojos…** row of the local Recent Dojos picker opens the launcher. That row
appears only when profiles exist or `remotes.toml` is invalid, and a picker
already bound to a remote omits it. An absent, empty, or invalid `remotes.toml`
leaves the profile list empty with a one-line explanation; `splinterm remote
list` reports the parse error.

### Connection status

Every graphical `--remote` launch, and the launcher once a profile is chosen,
first shows a status screen reading "Connecting to DESTINATION…" while OpenSSH
authenticates and the remote daemon answers a ping and lists its Lairs. On
success the same Window goes on to the remote's Dojo picker
(`splinterm --remote PROFILE`, alone or with `dojos`) or the launcher's Dojo
list; other `--remote` commands open their own Window. On failure it shows the
categorized cause, a one-line remedy, the newest sanitized OpenSSH lines, and a
reminder that an ssh-agent key or `SSH_ASKPASS` is the preferred way to
authenticate without a terminal. Press R or Enter to retry once, or C, Q, or
Escape to close (in the launcher, to go back to the profiles); Escape also
cancels a connection in progress. Each retry reloads the profile from
`remotes.toml`. A `--remote` argument that breaks the profile-name rules is
shown as "remote" rather than echoed.

When OpenSSH needs a password, passphrase, or PIN and has no terminal, agent
key, or `SSH_ASKPASS` that can answer, the status screen shows OpenSSH's prompt
text and a masked password field: Enter submits and Escape cancels the
attempt. A yes/no question from OpenSSH takes Y or N instead, and a notice
such as "confirm user presence" stays until OpenSSH continues. The status
screen, the profile list, and the Dojo list never use the input method, so a
typed password never reaches it as composing text; the input method turns on
only when the terminal opens.

OpenSSH's error output appears only in that Window, never on a terminal. When
`splinterm --remote` is started from a terminal, stdin is a terminal, so
Splinterm does not set `SSH_ASKPASS_REQUIRE=force` and OpenSSH may still prompt
for a password or passphrase on that terminal; the Connecting screen says so.
Launches from the app have no terminal and never prompt there.

If a direct `splinterm --remote PROFILE` closes before a connection is made,
whether after a failure, by cancelling with Escape, or by the compositor,
`splinterm` exits with status 1, so a terminal caller can tell that no remote
Window opened. The launcher instead returns to its profiles, or exits 0 when
its Window is closed.

Each failure is also recorded in the client diagnostics log
(`${XDG_STATE_HOME:-~/.local/state}/splinterm/logs/`) as a
`remote_connect_failure` event whose error code names only the category:
`remote_host_key`, `remote_authentication`, `remote_unreachable`,
`remote_installation`, `remote_daemon`, `remote_incompatible`,
`remote_transport`, `remote_profile` (`remotes.toml` could not be loaded or
lacks the profile), or `remote_local_setup` (`SSH_ASKPASS` does not name an
absolute executable, or the local `ssh` could not be started). Errors without a
more specific category are reported as `remote_transport`. Non-graphical
commands such as `remote check PROFILE` still report on the terminal.

## Remote profiles

Profiles live at `${XDG_CONFIG_HOME:-~/.config}/splinterm/remotes.toml`.
`SPLINTERM_REMOTES` selects an explicit file for isolated testing. The schema is
strict and versioned:

```toml
version = 1

[remotes.wintermute]
host = "wintermute"
user = "operator"                    # optional
port = 22                            # optional
identity_files = ["~/.ssh/id_ed25519"]
known_hosts_file = "~/.ssh/known_hosts"
connect_timeout_seconds = 15
executable = "/usr/bin/splinterm"      # optional remote path; this is the default
```

Unknown fields, unsupported versions, more than 64 profiles, unsafe names,
ambiguous host/user tokens, zero ports, timeouts outside 1–300 seconds, more
than eight identity files, documents above 64 KiB, and path values above 4 KiB
fail closed. Identity and known-hosts paths must be absolute or begin with `~/`,
name readable local regular files, and contain no whitespace or control characters.
The optional `executable` is instead an absolute path on the remote host; it is
not checked locally. For a system-installed NixOS package, use
`/run/current-system/sw/bin/splinterm`. It accepts only ASCII letters, digits,
`/`, `.`, `_`, and `-`, with no empty, `.` or `..` components. Relay arguments
remain fixed. Profiles cannot supply arbitrary SSH options, commands, forwarding,
environment, or shell fragments. See `docs/nixos.md` for NixOS installation.

`~/.ssh/config` may still select ordinary aliases, identities, certificates, and
proxy routing. Splinterm's command-line safety options override conflicting
`RemoteCommand`, `LocalCommand`, terminal, stdin, forwarding, and host-key
settings. `remote inspect` prints resolved non-secret settings and structured
argv; it never reads or prints private-key contents.

## Authentication and host keys

Splinterm directly spawns the installed `ssh` executable with piped protocol
stdin/stdout and separate bounded stderr. It does not force batch mode. When a
controlling terminal exists, OpenSSH may use `/dev/tty` for password, key
passphrase, PIN, or hardware-token interaction.

Without a terminal, OpenSSH chooses who answers in this order:

1. **ssh-agent.** OpenSSH always tries agent keys first and asks for nothing
   when one is accepted. This is the preferred setup.
2. **Your own `SSH_ASKPASS`.** Splinterm requires the value to name an absolute
   executable local file, then runs OpenSSH with exactly that program and
   `SSH_ASKPASS_REQUIRE=force`. A set but unusable `SSH_ASKPASS` is reported as
   a local setup failure, not replaced by Splinterm's prompt.
3. **Splinterm's prompt.** For graphical `--remote` launches only, when
   neither of the above is available, Splinterm sets `SSH_ASKPASS` to its own
   client executable with `SSH_ASKPASS_REQUIRE=force`. OpenSSH runs it only if
   it actually needs input, passing the prompt text as the only argument. That
   helper connects back to the status Window of the same launch through a Unix
   socket in a fresh `0700` directory under `XDG_RUNTIME_DIR` (or the temporary
   directory), presents a random 256-bit token unique to that connection
   attempt, prints the answer on stdout for OpenSSH, and exits. It needs Linux
   pidfds (5.3 or later) so OpenSSH can be stopped safely; without them the
   native prompt is unavailable.

Non-graphical commands such as `remote check` never use Splinterm's prompt;
without a terminal or `SSH_ASKPASS`, an interactive prompt is reported as
unavailable.

Splinterm's prompt is a fallback with fixed limits:

- The status Window accepts a prompt only while that launch is connecting, and
  only from a process with the same user ID that descends from that attempt's
  OpenSSH child and presents that attempt's token. It shows one prompt at a
  time. The socket and its directory are removed when negotiation ends, and a
  retry gets a new socket and token.
- OpenSSH's prompt text is shown as quoted text under a header naming the
  destination being connected to (the prompt itself may come from a jump host), with
  control, bidirectional, and zero-width characters removed and at most 8
  lines of 100 characters, because keyboard-interactive servers can choose it.
- The field never echoes: the screen, Window title, and accessibility text say
  only "Password field: empty" or "Password field: entered (hidden)", never
  the text or its length. The status Window does not use an input method
  (IME), so composing text never shows or teaches one a password. Pasting
  into the field is allowed, for password managers; a line break inside
  pasted text ends the input there without submitting. The field has no
  history, scrollback, or copy source, and Splinterm never puts the answer on
  the clipboard.
- Answers are limited to 1023 bytes, kept in buffers that never reallocate,
  wiped after they are handed to OpenSSH or abandoned, and never logged or
  written to diagnostics. The helper process disables core dumps for itself.
- OpenSSH turns any refused prompt into an empty password and asks again, so
  Splinterm never refuses a password prompt. Escape, closing the Window, or
  leaving a password or yes/no prompt unanswered for 120 seconds ends the
  attempt: Splinterm kills OpenSSH (and a nested `ProxyJump` OpenSSH that
  asked) before the prompt closes. This is reported as an authentication
  failure with Retry. A yes/no question needs an explicit Y or N, and N is
  passed to OpenSSH as "no".
- If the client itself dies during a password prompt (a signal, crash, or
  out-of-memory kill), the helper finds no answer and kills the OpenSSH that
  started it rather than letting it send an empty password.

Splinterm never places a password or passphrase in argv, TOML, relay frames,
diagnostics, or application storage. Post-connect SSH authentication and relay
negotiation are bounded by the profile timeout plus a 120-second human/hardware
interaction allowance. Time a password or yes/no prompt in the status Window
spends waiting on you does not count against that bound, so each of several
sequential prompts (a key passphrase, then a password or one-time code) gets
its full 120 seconds; that paused time is capped at 10 minutes per attempt.
Logical-channel admission, private daemon handshake, and
each `remote check` probe use the validated profile timeout.

`StrictHostKeyChecking=yes` is always supplied. Unknown and changed host keys
fail closed. Establish trust explicitly with normal OpenSSH tooling; Splinterm
does not run `ssh-keyscan`, append known-hosts files, or offer accept-anything
behavior.

Because `StrictHostKeyChecking=yes` is always supplied, OpenSSH never asks to
accept an unknown host key through Splinterm's prompt.

Bounded errors distinguish host-key changes/unknown keys, routing or timeout,
authentication failure, unavailable terminal/askpass interaction, missing remote
command, unavailable daemon, relay/daemon identity rejection, outer protocol
mismatch, private protocol mismatch, and generic transport loss. SSH
configuration can still affect routing and authentication, so inspect it with
standard OpenSSH tools when diagnosing aliases or proxies.

## Human authority and automation policy

The native graphical workflow is a human SSH session. After OpenSSH authenticates
the account, the graphical relay receives normal terminal-multiplexer authority
from that account's `splinterd`: session discovery, attachment, input, resize,
creation, splitting, naming, closing, restoration, and reconnection do not
require policy rules. Newly created topology is usable immediately.

Persistent exact-resource policy remains exclusively for machine clients using
`relay --stdio`, JSON/NDJSON, or MCP. Its snapshot behavior does not constrain a
native remote Window. Policy reload disconnects automation-role connections so
they re-evaluate the new generation; it does not disconnect local or remote
human graphical clients.

The human role is not a boundary against software already running as the
account. `splinterd` recognizes the graphical relay by its installed executable
and `--graphical-stdio` mode, and the relay forwards whatever its caller sends,
so any process running as the account can start it and receive the same
authority without a prompt. Policy and consent keep cooperating tools within
what was granted and make their access visible; they do not contain a hostile
process, which can already read the account's files and run its commands.

Remote snapshots must omit image metadata. The client never opens a `.content`
socket for a remote endpoint; unexpected remote image metadata fails the pane
closed before any body request. `PublishGraphicalFocus` is never sent remotely.
SSH/relay/channel loss shuts down the affected local views and drops their
connections, releasing subscriptions and controller leases. It sends no kill,
close, restore, or other process-lifecycle request, so daemon-owned remote
Splints continue running.

When the remote daemon is upgraded live, its logical channels end while the SSH
session and relay stay up. The Window then shows "Reconnecting…" and opens new
logical channels on the same session for up to about 30 seconds, as a local
Window reconnects. Splinterm never starts OpenSSH again or re-authenticates by
itself: when the outer SSH transport or the relay is gone, reconnection fails
and the Window keeps its last screen with "Lost connection to splinterd —
reopen from the launcher".

### Agent access prompts

A remote Window shows the agent access prompts of the computer it is connected
to, so an agent working there can ask for a workspace while you work in the
Window (ADR 0015). The prompt is the same trusted dialog as a local one, started
by the Window itself, and it says that the request is remote: its title, and a
host row naming the SSH profile and the hostname that computer reports. Every
connected Window shows the prompt at once, beside that computer's own dialog
when it has one; the first answer counts and the other dialogs close. This
holds per Window: with three Windows open to the same computer, each prompt
shows three identical dialogs, and answering any one of them closes the others.
A prompt nobody answers closes when the request's time runs out, without
answering.

The Window keeps one relay channel for this, and subscribes again by itself
after the remote daemon is upgraded live. Set
`[integration] remote-agent-prompts = no` in this computer's configuration to
leave the prompts to the remote computer ([configuration](configuration.md)).

## Remote window hostname tag

Available in the 0.1.1 source; publication is pending.

Remote graphical windows show a compact `Remote: hostname` tag in the upper-right
application header. The hostname is the connected daemon's kernel OS nodename,
not the SSH profile/alias, terminal title, OSC data, or working directory. Local
windows have no tag and retain their existing geometry. Hiding remote Dojo tabs
keeps a minimal header for the tag; the tag is noninteractive and does not cover
terminal cells or tab actions. Narrow windows truncate the label by display cells,
retaining the `Remote` prefix when it fits.

This is informational connection-time metadata, **not a trusted security or
connection-health indicator**. Each new Window takes the hostname from its initial
connection to its fixed endpoint; tab selection does not change it. Renaming the
host does not update an existing Window. Transport failure continues to use the
existing shutdown/error lifecycle, not a new reconnection mechanism.

Older daemons that omit the optional handshake field show `Remote`. Empty,
non-UTF-8 OS names, names over 255 UTF-8 bytes, and metadata containing anything
other than letters/numbers, dots, hyphens, or underscores also fall back to
`Remote` (including control, whitespace, and bidi-formatting characters). The
additive private-protocol field does not change protocol negotiation or public
CLI schemas; older clients ignore it. No hostname command, shell lookup, or
`HOSTNAME` environment value is used.
