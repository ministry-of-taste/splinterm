# NixOS installation

The repository flake builds the **complete Wayland desktop terminal**, not a
headless-only edition. The default package includes `splinterm`, `splinterd`,
`splinterm-relay`, `splinterm-pty-child`, the desktop entry/icon, and the Dojo
launcher aliases. The same package works as a headless SSH session host without
a compositor. Omarchy is not required; its optional integration commands are
not part of NixOS setup.

Initial packaging targets `x86_64-linux`. Other architectures are not advertised
until built and tested. A native Wayland desktop is required for GUI windows;
headless operation requires no graphical session. This packaging work does not
establish graphical acceptance on every NixOS compositor.

## Recommended: NixOS module

Add Splinterm to your configuration flake and commit the resulting `flake.lock`:

```nix
inputs.splinterm.url = "github:ministry-of-taste/splinterm/<reviewed-revision>";
```

Replace `<reviewed-revision>` with `v0.1.1` or a later reviewed release or
revision. Nix support has been available since 0.1.1; the original `v0.1.0` tag
predates it and must not be used for these instructions.
The public release installer and Arch packages are **not** NixOS installers.
The package's own pinned nixpkgs supplies its Rust/native toolchain; following a
host's older nixpkgs input may not provide a sufficiently recent Rust compiler.

Import the module into each desired `nixosConfigurations.<host>.modules` list:

```nix
inputs.splinterm.nixosModules.default
```

Then enable it in a host module:

```nix
{
  programs.splinterm.enable = true;
}
```

Rebuild NixOS normally from an external terminal or SSH connection, **not a shell
owned by the daemon being replaced**. The module installs the full package and
user service, enables Fontconfig, and adds JetBrains Mono Nerd Font plus CJK and
emoji fonts. It does not select a compositor, change your default terminal,
configure SSH authorization, enable lingering, or modify home-directory files.

Launch **Splinterm** from the application menu. Its launcher starts the user
daemon on demand; Recent Dojos and Reopen Last Dojo use the same package. To
start it from an existing shell:

```sh
splinterm-xdg-terminal-exec
```

`nix run .# -- --help` runs the raw CLI, not the daemon-starting desktop wrapper.
Installing only the package with `nix profile install` does not register its
systemd user unit with NixOS. Use the module for the supported desktop setup.

## Headless hosts

Use the same module and package, but omit the desktop font integration and start
the daemon automatically:

```nix
{
  programs.splinterm = {
    enable = true;
    desktopIntegration = false;
    startAtLogin = true;
  };

  # Optional: start this user's manager at boot and keep it after logout.
  users.users.operator.linger = true;
}
```

Replace `operator` with an existing account. `startAtLogin` enables the service
for all user managers; lingering is explicitly per user. The GUI remains in the
package, so a desktop profile can simply restore `desktopIntegration = true`.
No local daemon TCP listener or firewall opening is needed; remote access uses
OpenSSH. Configure the policy separately using `docs/remote.md`.

The unit retains the upstream `--require-workload-cgroups` requirement,
`app-splinterm.slice`, resource limits, SIGHUP reload and SIGINT cleanup. It
reads the optional owner-controlled `~/.config/splinterm/daemon.env`, matching
the Arch unit. Do not blindly add service sandboxing that blocks user PTYs,
Unix sockets, user-manager D-Bus, or workload scope creation.

## Remote profiles targeting NixOS

On each connecting client, set the remote executable explicitly:

```toml
version = 1

[remotes.server]
host = "server"
executable = "/run/current-system/sw/bin/splinterm"
```

`executable` defaults to `/usr/bin/splinterm` for existing Arch profiles. It is
an absolute path on the **remote** host, not a local file or arbitrary command.
Only ASCII letters, digits, `/`, `.`, `_`, and `-` are accepted; empty, `.` and
`..` components are rejected. No whitespace, tilde expansion, shell operators,
or user-supplied arguments are accepted. The relay arguments remain fixed.
Both ends must run protocol-compatible versions. The connecting client also
needs a version supporting the new profile field.

Use the resolved store executable and its SHA-256 digest when authorizing the
relay or MCP adapter in policy, not an assumed `/usr/bin` identity:

```sh
readlink -f /run/current-system/sw/bin/splinterm-relay
sha256sum /run/current-system/sw/bin/splinterm-relay
```

Review/update policy after package changes. A NixOS activation does not silently
grant a new executable automation authority.

## Optional MCP adapter

The default package does not include MCP. Select the complete matching variant
rather than mixing binaries from different derivations:

```nix
{ pkgs, ... }: {
  programs.splinterm.package =
    inputs.splinterm.packages.${pkgs.stdenv.hostPlatform.system}.splinterm-with-mcp;
}
```

Here `inputs` must be in your module's lexical scope or passed through your
configuration's `specialArgs`. This variant includes the main package and the
adapter; it does not configure an MCP host or grant policy permissions. See
`docs/mcp.md` for explicit setup.

## Executable identity and upgrades

The client, daemon, relay and PTY helper are unwrapped sibling ELF binaries in
one immutable store output. NixOS profile symlinks lead to those binaries; the
package does not rename them to `.wrapped` files. Desktop scripts and service
units use matching store paths, and native helpers such as `fc-match` are bound
to their Nix dependencies without an `LD_LIBRARY_PATH` wrapper.

A store generation change does not migrate a running daemon's processes.
Schedule upgrades, stop the old user daemon, switch configuration, and restart
it; close and reopen old GUI windows. Otherwise old daemon/new client identity
checks may reject connections even when the protocol version is unchanged.
A rollback must restore both the package/service generation and any associated
policy identities. Persistent shell processes are not restored by rollback.
Live daemon upgrade (`splinterm upgrade --now`) is not supported for Nix
installations in Splinterm 0.2.0; `splinterm upgrade` reports this, and the
restart procedure above remains the upgrade path.

## Build and non-graphical checks

From a Git checkout containing the flake:

```sh
nix build .#splinterm
nix flake check
nix build .#splinterm-with-mcp
```

Commit provenance comes from the Git flake revision; a dirty checkout is for
development only and is not a release artifact. `nix/package.nix` also accepts
an explicit 40-character `sourceRevision` for non-flake source packaging; do not
invent a revision for a source snapshot.

A clean build installs `share/splinterm/package-provenance-v1.json` in its store
output, recording the commit, the `nix-store` build kind and the hashes of the
final `splinterd` and `splinterm` binaries. Upgrade handoff will trust a store
generation only through that record. A dirty build (`sourceDirty = true`, set by
the flake for an uncommitted tree) keeps its commit for diagnostics but installs
no record and cannot take part in handoff.

The record shows that a store output's binaries are the ones it describes; it
does not prove who built them. Any user the Nix daemon serves can realise a
store output whose record claims an arbitrary commit, so handoff will only
consider the generation selected by the system or user profile.

Checks cover remote-profile validation, installed desktop-file validity,
unwrapped sibling layout, a private non-graphical daemon with trusted human CLI
access, PTY helper execution and clean shutdown, and desktop/headless/disabled
NixOS module evaluation.
The package's private-daemon smoke does not require a live user manager. The
separate headless VM check below tests the actual module/service integration.
The broader upstream Rust suite remains a separate boundary. Acceptance on real
hosts and approved Wayland graphical testing are additional deployment checks,
not implied by a successful build.

### Headless NixOS integration test

```sh
nix build .#checks.x86_64-linux.headless --print-build-logs
```

This check is also included in `nix flake check`. CI runs it together with the
package and module checks; the required `check` job depends on the Nix job, so a
Nix failure cannot produce a successful release-authority CI result. On pull
requests, the Nix job runs only when Nix, packaging, Cargo manifest, toolchain,
or CI workflow files change. It always runs in the merge queue and on
authority-branch pushes. See
[Merge queue and Nix cache](release-automation.md#merge-queue-and-nix-cache).
Graphical tests remain opt-in and are not launched by CI.

It boots two disposable,
non-graphical NixOS VMs (1.5 GiB RAM and two vCPUs each) on an isolated test
network. The Nix builder needs working KVM access and the `kvm`/`nixos-test`
system features. First use downloads the pinned NixOS VM/test-driver closure.
It does not activate a workstation service or contact production hosts.

The check covers:

- disabled-by-default service and lingering, and explicit manual startup;
- automatic startup under an explicitly lingering test account, without display
  variables or a desktop session;
- owner environment-file loading, configured shell, UID, home, CWD, and PTY;
- strict workload cgroup placement, inherited aggregate task and nested memory
  boundaries, and scope/slice cleanup after exit or daemon shutdown;
- denied machine-mode access without policy, removal of the development bypass,
  and rejected local clients from a different package derivation;
- real SSH login/logout, native remote checks and reconnection, with strict host
  keys, disposable test credentials and the NixOS remote executable path;
- controlled switching between the standard and MCP-enabled package generations
  and back, retaining matching executable/service identities; and
- reboot startup with persisted topology but no automatic workload execution.

Generation switching uses two variants of the same source revision. It tests
store-path/inode and NixOS activation behavior, **not** cross-version protocol
migration or live process preservation. The test driver bounds execution to
15 minutes and tears down its private VMs. A failing build retains its Nix log;
use `nix log` on the failed derivation to inspect the exact failing subtest.

### Opt-in disposable Wayland desktop acceptance

Graphical acceptance requires approval for the complete bounded guest sequence.
These test packages are deliberately **not** part of ordinary `nix flake check`:

```sh
nix build .#desktop-smoke --no-link --print-build-logs
nix build .#desktop-test --no-link --print-build-logs --json
```

Run the smoke first. The full test also repeats that smoke before continuing.
Both use one disposable NixOS VM with Sway, a guest-only virtual GPU, software
rendering, 2 GiB RAM, two vCPUs, and a 15-minute execution limit. There is no host
viewer, workstation input, SSH connection to a real host, or host installation.

The smoke launches the installed desktop entry through GIO with `splinterd`
stopped, verifies matching client/daemon executable paths, enters a shell marker
through the guest virtual keyboard, and verifies visible text with OCR.
The full matrix additionally checks:

- installed New, Dojos, and Reopen desktop actions;
- tab creation/cycling and a split, with shell PID/count assertions;
- resizing with changed PTY geometry and rendering at 1× and 1.5× scale;
- ASCII, Chinese, Japanese, Korean, and color emoji specimens;
- clipboard paste into a shell and copying a pointer-selected text fixture; and
- Ctrl-click URL dispatch to a guest-local recording handler, with no browser
  or external network request.

Reopen must return to the same live shell after the disposable window is closed.
Each case records window ID/PID, focus, workspace, geometry, output scale and
transform, seat state, and test-controlled cursor position. Every input batch
checks the sole owned guest window; unexpected windows or focus/workspace drift
abort the test. Pointer fixtures use OCR bounds from unmodified guest captures.
Cases close only their test windows, stop the private user daemon, check socket
and helper cleanup, and restore an empty workspace 8, scale 1, and cursor origin.
The NixOS driver tears down its private VM on success or failure.

A successful test output contains `acceptance/` with PNG captures, state records,
shell markers, and launcher logs. Inspect the captures as well as the assertions:
OCR proves selected ASCII fixtures are visible, not comprehensive glyph quality.
Font specimens are reprinted after scale changes; this is not a test of a fixed
viewport across terminal reflow. This software-rendered Sway fixture does not
replace acceptance on another compositor, physical GPU, browser, or real host.
Desktop actions exercise their installed `Exec` entries, not a desktop-menu
widget's navigation.
