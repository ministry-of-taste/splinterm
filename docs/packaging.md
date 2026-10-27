# Arch packaging

Release authority, candidate construction, approval boundaries, and the future
n8n notification role are defined in [Release automation](release-automation.md).

Splinterm **0.1.1 is the current stable release** for x86_64 Omarchy/Arch Linux
with native Wayland under Hyprland. Newer release candidates are prereleases,
not new stable releases. Stable notes and immutable assets:

https://github.com/ministry-of-taste/splinterm/releases/tag/v0.1.1

`packaging/PKGBUILD` produces split packages for reviewed local and CI builds.
Its version comes from the checked-out commit; `main` is development code, not
the stable release, and carries the next development version. Published 0.1
releases originate from `maint/0.1`. Use the exact `v0.1.1` tag for a stable
0.1.1 source build, as shown below; do not relabel a development build as stable.
The local source archive and `SKIP` checksum are valid only in that workflow.

The closed `packaging/release-state.json` record identifies `v0.1.1` as the
current public release and predecessor for future candidates. It is the machine-readable authority for that
boundary; candidate construction and promotion both reject a
different supplied tag even when it exists. `docs/status.md` carries the same
exact current-version marker, and release tooling rejects disagreement. The
post-publication release-record PR must update both files atomically before any
later candidate is prepared.

The source-built AUR authority is `packaging/aur/PKGBUILD`; candidate
construction replaces its source-archive checksum with the exact immutable
versioned archive digest. It intentionally omits a `check()` phase. The
recommended prebuilt authority is `packaging/aur-bin/PKGBUILD`; it must already
target the candidate version's immutable `v…` release URL, while candidate
construction binds its commit and checksums to the exact approved packages so
users never compile or test them locally.

During release preparation, checked-in AUR checksums and `_commit` still refer
to the previous artifacts until candidate tooling replaces them. These staged
recipes are not ready for AUR submission or installation. Never invent hashes
for unpublished assets; use the reviewed candidate's generated drafts.

## Versioned AUR installation

**AUR is prerelease-capable, not stable-only.** The same `splinterm` and
`splinterm-bin` package bases distribute stable releases and release candidates;
a candidate can be offered as an upgrade over stable 0.1.1. Check the offered
version before confirming. For stable 0.1.1 specifically, use the exact-tag
build below or its verified release assets instead.

Install the prebuilt main package and optional exact-version MCP adapter with
an AUR helper:

```bash
yay -S splinterm-bin
yay -S splinterm-mcp-bin
```

The source-built alternatives are `splinterm` and `splinterm-mcp`. Migrating to
the `-bin` packages prompts once to replace those conflicting source packages.
`paru` may be used instead of `yay`. AUR availability does not expand the
validated target beyond x86_64 Arch/Omarchy with native Wayland/Hyprland or add
a support-lifetime promise. Check the version offered by your AUR helper before
confirming an upgrade; the package listing, not this guide, owns its current
version.

## One-command versioned release installation

On an x86_64 Arch/Omarchy machine, a clone installs or updates to the newest
published versioned release without compiling locally:

```bash
./install.sh
```

The installer queries the public GitHub release index and considers only
non-draft complete SemVer `v…` tags with one `candidate-manifest.json` asset. It
selects the most recently published qualifying release, verifies the manifest
against the digest recorded by GitHub, then validates the reviewed candidate
identity, repository, version, architecture, exact commit-bound package pair,
and SHA-256 package digests before downloading either package. Historical
`edge-*` releases cannot be selected. **Prereleases are eligible**: this is not
a stable-only selector. To install stable 0.1.1 specifically, use the exact-tag
source build below or the verified assets from its release page.

An authenticated GitHub CLI session is used when one is already available;
otherwise the installer uses anonymous GitHub API and release downloads through
`curl`. Authentication is not required for ordinary public installs.

Before Pacman installation, the script verifies package checksums and matching
split-package versions. It rejects a shadowing user-local client, preserves an
emergency snapshot of replaced binaries for diagnosis or manual recovery, says
before confirmation how many live terminals stopping the daemon ends (see
[Installer confirmation](#installer-confirmation)), restores the daemon after
failure, and
checks Pacman integrity, the desktop entry, and trusted-client sibling identity
after restart. The snapshot is not presented as a Pacman package rollback;
reinstall a previously retained package for a package-consistent downgrade. It
deliberately does not change the default terminal or edit Omarchy configuration.
A fresh installation does not install the optional MCP package; an existing MCP
installation is upgraded to preserve its exact-version dependency. Pass `--yes`
only for an already-approved unattended installation.

## One-command source installation

To compile and install the current committed checkout instead, use an external
terminal such as Foot—not a shell running inside Splinterm:

```bash
./install.sh --source
```

Both source and prebuilt installation refuse before service or package work when
the installer has a `splinterd` or `splinterm-pty-child` ancestor. Stopping the
daemon from one of its own shells would terminate the installer before Pacman or
its recovery path could complete.

This installs missing build/runtime dependencies, builds the committed checkout,
validates package contents, and asks before installation. Pass `--check` (which
implies `--source`) to run the complete `PKGBUILD` `check()` function.

## Build without installing

The package source must be an exact committed snapshot. For stable 0.1.1, create
a separate checkout of the published tag rather than building development
`main` or changing its version metadata:

```bash
git fetch origin tag v0.1.1
git worktree add --detach .worktrees/stable-0.1.1 v0.1.1
cd .worktrees/stable-0.1.1
```

Run the following build and install examples from that stable checkout.
At that tag, `packaging/PKGBUILD` declares `pkgver=0.1.1` and `pkgrel=1`.
For other reviewed commits, use the version declared by that commit rather than these filenames.
The guarded build and validation entry point is:

```bash
tools/package/build-local-package.sh
```

Pass `--no-check` to build and validate package contents without running the
complete package test suite. This is the mode used by `./install.sh --source`;
`./install.sh --check` retains the complete package test suite.

`--vm-acceptance-live-handoff` is for maintainers' live-upgrade VM acceptance
only (`docs/handoff-vm-acceptance.md`). It builds a **test-only** daemon with
the `vm-acceptance-live-handoff` feature, which forced live upgrade on for a
local package build before the release enabled it. Live upgrade is now enabled
in every build, so the feature changes no decision, but the build still carries
its test-only marker. The checked-in recipes never enable that feature. The
package's provenance record and package validation refuse a daemon carrying the
feature's marker in any release-candidate or Nix package, and in a local
package unless it was explicitly requested. Install such a package only in the
VM testbed (`tools/testbed/omarchy-vm.sh package-install --confirm-guest-install
--vm-acceptance-live-handoff`), and never publish or ship it.

Its equivalent manual build from a clean checkout is:

```bash
git archive --format=tar.gz --prefix=splinterm-0.1.1/ \
  -o packaging/splinterm-0.1.1.tar.gz HEAD
```

The archive honors `.gitattributes` `export-ignore` entries; website source and
website deployment automation are intentionally excluded from package release
inputs. Continue the manual equivalent with:

```bash
(
  cd packaging
  makepkg --cleanbuild --syncdeps --noconfirm
)
```

`makepkg` builds release binaries with the lockfile, runs workspace tests, and
creates the main package plus the explicitly optional `splinterm-mcp` split
package without installing either. Inspect them with:

```bash
pacman -Qlp packaging/splinterm-0.1.1-1-x86_64.pkg.tar.zst
pacman -Qlp packaging/splinterm-mcp-0.1.1-1-x86_64.pkg.tar.zst
namcap packaging/PKGBUILD packaging/*.pkg.tar.zst   # optional
```

Renderer checks use the same four checksum-pinned JetBrains Mono fonts as CI.
`makepkg` fetches these immutable sources alongside the local archive and verifies
their SHA-256 hashes. During `check()` only, a private Fontconfig file selects
these fixtures plus installed Noto fallback fonts, with a build-local font cache.
Ambient user/system Fontconfig settings cannot substitute a newer JetBrains Mono.
The fixtures are neither installed nor shipped; runtime font dependencies and
user font selection are unchanged. The fixture generator requires makepkg's
already-verified sources. Even `--nocheck` builds still fetch the declared sources.

The local archive checksum is intentionally `SKIP`: the archive is generated
from the reviewed local Git commit, is never downloaded, and the package's
`.BUILDINFO`/`.PKGINFO` record the actual build. The separate checked-in AUR
recipe uses the immutable versioned release asset and real SHA-256 checksums; do
not submit the local-build recipe to the AUR.

## Installed layout

- `/usr/bin/splinterm`, `/usr/bin/splinterd`, the dedicated adjacent
  `/usr/bin/splinterm-relay` SSH transport (byte-transparent `--stdio` and
  bounded `--graphical-stdio` modes), and `/usr/bin/splinterm-pty-child`;
- optional split package `splinterm-mcp`, containing only the independently
  policy-authorized `/usr/bin/splinterm-mcp`, its setup guide, and notices;
- `/usr/bin/splinterm-xdg-terminal-exec`, which preserves structured XDG argv
  and selects configured commandless or always-transient command-bearing launches, its
  canonical `splinterm-dojos` and `splinterm-reopen` Dojo UX aliases, the
  compatibility `splinterm-sessions` alias, the public-CLI-only
  `/usr/bin/splinterm-dojo-picker` reference client and compatibility
  `/usr/bin/splinterm-session-picker` alias, and the optional
  `/usr/bin/generate-omarchy-theme.py` JSON exporter;
- the terminal desktop entry, the **Splinterm Remotes** app-menu entry
  (`com.oldjobobo.splinterm.remotes.desktop`, which runs `splinterm remotes`
  and is not a terminal emulator entry), AppStream metadata, scalable icon,
  and user service;
- the release README, CLI/usage/configuration/accessibility guides, built-in Omarchy keymap
  documentation, preset guide and example schema, optional shell-integration
  instructions, headless lifecycle/policy guidance, terminal-image matrix, and
  integration snippets under `/usr/share/doc/splinterm/`; and
- MIT/project third-party notices under `/usr/share/licenses/splinterm/`.

The package does not modify a user's home, enable a service or lingering,
replace their terminal preference, edit SSH policy, or edit
`/usr/share/omarchy`. The desktop launcher starts `splinterd.service` on demand
and never restarts a daemon that may still run shells without asking; see
[Desktop launcher and the running daemon](#desktop-launcher-and-the-running-daemon).
The headless unit reads an optional owner-controlled environment file, reloads
policy with SIGHUP, and stops with SIGINT so the daemon can reap its shells and
remove its socket cleanly.
The local and remote packages must be version-compatible, and the relay must
remain adjacent to the exact packaged `splinterd` executable because every
logical graphical channel repeats executable-identity validation. See
[headless.md](headless.md) for the complete service, policy, backup, and
recovery workflow, [remote.md](remote.md) for both policy-scoped SSH relay modes,
[integrations.md](integrations.md) for reference-client and in-Splint workflows,
and [mcp.md](mcp.md) for the optional adapter's host and digest-policy setup.

## Optional user integration

Splinterm follows the active Omarchy Quattro theme natively; no theme hook or
user-generated palette is required. The explicit unified integration makes
Splinterm the current user's XDG default terminal, gives its Window Omarchy's
`terminal` tag, and enables the guarded screensaver adapter:

```bash
splinterm integration omarchy enable
splinterm integration omarchy status
```

`disable` removes only exact managed objects and restores the previous
`xdg-terminals.list` object byte-for-byte, including a relative or dangling
symlink. Components already configured externally are reported as ready but are
never claimed or removed. A versioned state journal under
`${XDG_STATE_HOME:-~/.local/state}/splinterm/integrations/` prevents guessing
after an interrupted transaction.

Without the `terminal` tag, Omarchy's universal `Super+C` sends `Ctrl+C`
(interrupt) to Splinterm, so the `post_install` and `post_upgrade` messages of
`splinterm.install` recommend the command and explain why. The AUR source and
binary recipes carry byte-identical copies. The client also shows one desktop
notification under Omarchy 4 while the tag is missing; see
[integrations.md](integrations.md#why-the-terminal-tag-matters).

Package installation still changes none of these user files. The reference
terminal-list snippet remains `/usr/share/doc/splinterm/xdg-terminals.list` for
manual setups.

The default launcher always creates a fresh terminal. Use the **Recent Dojos**
desktop action or `splinterm-dojos` to choose an existing running Dojo, and
**Reopen Last Dojo** or `splinterm-reopen` for the last locally remembered Dojo
whose complete pane layout is still running. `splinterm-sessions` remains a
compatibility alias. These aliases
also start the user daemon on demand and pass the same daemon check as the
default launcher. They never restore exited processes. The
**Remote Dojos** action and the separate **Splinterm Remotes** app-menu entry
both run `splinterm remotes`: one Window that lists the profiles in
`remotes.toml`, connects, lists that host's Dojos, and becomes the remote
terminal. It needs no local daemon. See
`configuration.md` for the suggested Omarchy shortcut split.

## Desktop launcher and the running daemon

A package upgrade replaces the files under `/usr` but leaves the running
`splinterd`, and every shell it owns, on the previous build. Package scriptlets
never restart user services. Before it opens anything,
`splinterm-xdg-terminal-exec` (and the `splinterm-dojos`, `splinterm-reopen`,
and `splinterm-sessions` aliases) asks the running daemon for its own report:
its build identity and its exact count of live terminals. It compares that
identity with the build of the `splinterm` it runs (on packaged systems
`/usr/bin/splinterm`, installed together with `/usr/bin/splinterd`). A failed
`ping` is never taken as permission to restart.

The launcher also identifies the process behind the socket without its
cooperation: its process ID from the socket's peer credentials, and the device
and inode of its executable.

| Running daemon | What the launcher does |
| --- | --- |
| None is listening | Starts `splinterd.service`, waits up to 5 seconds for it, and opens the Window. |
| The installed build | Opens the Window. |
| Another build that reports no live terminal | Restarts the service without asking, then opens the Window. |
| Another build with live terminals | Asks in a trusted dialog; restarts only on an explicit confirmation. |
| Cannot be read (another protocol, such as a 0.1.x daemon; no answer within 5 seconds; refused as busy; an unusable answer), after one retry | Asks in the same dialog. The count comes from the running `splinterm-splint*.scope` workload units and is labelled best effort ("ends about N shells"); when none can be counted, it says it ends all running shells. |
| Cannot be read, but its executable is the installed `splinterd` | Fails as busy ("try again shortly"); never asks, never restarts. |
| Cannot be read, and the client is not the `splinterm` beside the daemon's executable (a development build or a copy earlier on `PATH`), or its process cannot be identified | Fails with an explanation; never asks. |
| Refuses the client as untrusted, or the socket cannot be used | Fails with an explanation; nothing is started or restarted. |

Only a plain interactive launch may ask. A launch with an explicit app ID,
such as the Omarchy screensaver's `--app-id=org.omarchy.screensaver`, or with a
command after `--` may be unattended, so it never shows the dialog: any
question is answered **Not now** with a message on standard error, and nothing
opens. An idle daemon is still restarted automatically for such a launch.

The dialog is titled "Splinterm — Upgrade" and follows the rules of the
`splinterm upgrade` dialog. It names the running and installed versions, the
exact number of shells a restart ends, and, when the running daemon reports
that nothing blocks a live upgrade, says to choose **Not now** and run
`splinterm upgrade` instead. `Enter`, `Escape`, `D`, or the red left button
answer **Not now**, the default; only `R` or the green right button chooses
**Restart now, end N shells**. The dialog appears without taking keyboard
focus, and its keys stay inert until you click inside it or press Ctrl+Alt+G
([Trusted dialogs](usage.md#trusted-dialogs)); keys and the restart click count
only about 600 ms after that. Once the dialog is armed, `R` acts when it goes
down: a hold that begins before arming is ignored, but one that begins after
arming counts as a press. Key repeats never count. Closing the dialog or no
answer within 120 seconds means **Not now**; with no answer the launcher prints
"splinterm: no answer within 120 seconds, so nothing was restarted." instead of
a failure, and exits as for **Not now**.
Before restarting, the launcher reads the daemon again and asks again if what
the restart would end has changed. **Not now** opens nothing and changes
nothing; the launcher exits with status 3. Without a graphical session, it
prints the same explanation and does not restart.

A restart is bound to the daemon process that was reported idle or confirmed:
it happens only while that process is `splinterd.service`'s main process
(`systemctl --user show -p MainPID`). Otherwise the launcher reads the daemon
again, and fails if the service never owns it, since restarting the service
would not replace that daemon. Concurrent launches take a lock
(`$XDG_RUNTIME_DIR/splinterm-launcher.lock`) around the check, the restart, and
the check after it, so two launches cannot restart the daemon twice. A restart
runs `systemctl --user daemon-reload` and `systemctl --user restart
splinterd.service`. If the restarted daemon is still not the installed build,
the launcher fails with "no further restart" rather than restarting again;
check `command -v splinterm` and the unit's `ExecStart`. The launcher never
starts a live upgrade itself. An explicit development socket
(`SPLINTERM_SOCKET`) is only pinged, never started or restarted.

### Bootstrap boundary from 0.1.x

A 0.1.x daemon predates live upgrade and cannot report its shells to a 0.2
client or hand them over. Moving from 0.1.x to 0.2.0 therefore needs one final
restart, which ends every running shell and the programs in them; save work
first. The launcher treats a 0.1.x daemon as unreadable and asks before that
restart, even when the daemon may be idle, with a best-effort count of its
running workload units. It asks only from the new `splinterm` beside the old
daemon's (now replaced) executable. Later compatible 0.2.x upgrades can
use `splinterm upgrade` where the running daemon reports live upgrade
available. After any restart, reopen existing Splinterm Windows: a running
client keeps the old binary, which no longer matches the installed trusted
client.

## Live installation and rollback

Building and inspecting does not require installation. The guarded local
upgrade command validates the newest package under `packaging/`, shows the
installed and candidate versions, states what installing ends (see
[Installer confirmation](#installer-confirmation)), installs with Pacman,
reloads the user unit, and restores the daemon's previous running state:

```bash
tools/package/upgrade-local-package.sh
```

To first build and validate a package from the clean, committed checkout:

```bash
tools/package/upgrade-local-package.sh --build
```

The command uses `sudo` because it is intended to run in an interactive
terminal, matching Omarchy's privilege convention. Pass `--yes` only for an
already-approved unattended invocation.

### Installer confirmation

Both `tools/package/upgrade-local-package.sh` and `./install.sh` stop a running
daemon before Pacman replaces it, so both ask first and say what that ends:

- no daemon running: installing ends nothing;
- a daemon reporting its count (through the read-only `splinterm upgrade`
  report): the exact number of live terminals that end, or that none do;
- a daemon that cannot report it, such as 0.1.x: every running shell ends,
  with a best-effort count of running `splinterm-splint*.scope` workload units
  when there are any.

The prompt also names a downgrade. Cancelling exits with status 0 before any
stop, install, reload, or start: the daemon and its shells keep running and
nothing changes. After confirmation, the installers authenticate `sudo` before
stopping anything, so a refused or failed `sudo` ends no shell, and Pacman runs
with `--noconfirm` so that it cannot ask again once the daemon is stopped.
Installing never hands shells to the new build; a live upgrade is a separate
`splinterm upgrade --now` against an already installed build.

**Downgrade.** Installing an older package is the same confirmed stop, install,
and start. An older daemon cannot read saved topology written in a newer
format: it logs that metadata recovery failed, renames those files to
`topology.invalid-*` in its state directory instead of deleting them, and
starts with no saved sessions. Reinstall the newer package to read them again.

**Downgrading to 0.1.x.** The 0.1 launcher restarts an incompatible daemon
without asking, ending every running shell. Before downgrading to 0.1.x, stop
`splinterd.service` yourself from a terminal Splinterm does not own, or use one
of the guarded installers, which stop it only after your confirmation. Never
downgrade with `pacman -U` alone while shells run.

**Interrupted installation.** If the installer exits early after stopping the
daemon, whether from an error, a failed Pacman transaction, or Ctrl-C, its exit
handler reloads the user unit and starts the daemon again from whatever is
installed; the shells that the stop ended do not come back. A `SIGKILL` skips
that handler and leaves the daemon stopped; the next launch starts it. A Pacman
transaction that did not complete is repaired by installing the package again.

Trusted graphical authority requires `/usr/bin/splinterm` to match the device
and inode of the client sibling adjacent to the running `/usr/bin/splinterd`.
A Window opened before an upgrade replaces that sibling keeps working, including
new tabs and panes, while it runs, until it is live-upgraded or reopened. New
launches use the new install. Verify the new daemon/client sibling identity
before launching graphical acceptance; graphical testing remains a separately
approved, guarded operation.

The equivalent manual lifecycle for the stable 0.1.1 packages built above is
shown below. Run it from a terminal not owned by `splinterd`; stopping the daemon
ends its child processes. Saved topology does not checkpoint running programs.

```bash
systemctl --user stop splinterd.service
sudo pacman -U packaging/splinterm-0.1.1-1-x86_64.pkg.tar.zst
systemctl --user daemon-reload
systemctl --user start splinterd.service
```

`splinterm upgrade` reports whether the running daemon could instead hand its
terminals to the newly installed build without a restart, and
`splinterm upgrade --now` does so. The 0.1.x packages above predate live
upgrade, so for them the restart above is the upgrade path; see
[Daemon upgrade](cli.md#daemon-upgrade). Without the manual restart, the next
desktop launch asks before restarting; see
[Desktop launcher and the running daemon](#desktop-launcher-and-the-running-daemon).

Install the adapter only when an MCP host will be configured:

```bash
sudo pacman -U packaging/splinterm-mcp-0.1.1-1-x86_64.pkg.tar.zst
```

The guarded upgrade script upgrades `splinterm-mcp` only when that optional
package is already installed; it never opts a user in. Remove it independently
with `sudo pacman -Rns splinterm-mcp`. Remove the main package with
`sudo pacman -Rns splinterm`. User-owned config, explicit theme overrides,
policy, and durable state are deliberately not deleted by package scripts.
