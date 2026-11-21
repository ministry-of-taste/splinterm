---
title: Remote access
description: Attach native Wayland windows to remote Splinterm daemons over SSH, or relay scoped automation without opening a network port.
---

Splinterm runs over your existing OpenSSH setup—`splinterd` never opens a network listener of its own. There are two separate SSH workflows:

- **Native remote windows** for humans working on another Linux machine running `splinterd`.
- **The stdio automation relay** for scripts and tools that need policy-scoped remote access.

Human SSH windows and machine automation don't share the same trust path: OpenSSH authenticates your interactive session, while machine relay clients still require an explicit policy on the remote host.

## 1. Define a remote profile

Create `${XDG_CONFIG_HOME:-~/.config}/splinterm/remotes.toml`:

```toml
version = 1

[remotes.wintermute]
host = "wintermute"
user = "operator"
port = 22
identity_files = ["~/.ssh/id_ed25519"]
known_hosts_file = "~/.ssh/known_hosts"
connect_timeout_seconds = 15
```

`remotes.toml` uses a strict schema—it doesn't accept raw SSH flags, port forwarding, or shell fragments. Standard host aliases, certificates, ssh-agent keys, and `ProxyJump` routing in your `~/.ssh/config` still work normally, with Splinterm applying safe defaults (such as strict host-key checking).

Check your profile before opening a window:

```bash
splinterm remote list
splinterm remote inspect wintermute  # shows resolved settings and exact SSH argv
splinterm remote check wintermute    # read-only SSH, relay, and daemon health probe
```

## 2. Open a remote window

From a terminal:

```bash
splinterm --remote wintermute          # opens the remote Recent Dojos picker
splinterm --remote wintermute dojos
splinterm --remote wintermute reopen
splinterm --remote wintermute launch --working-directory /srv/project
splinterm --remote wintermute window --lair-id LAIR_ID --dojo-id DOJO_ID
```

One window stays bound to one remote host and runs over a single OpenSSH child process. You can list, attach, split, search, and restore remote Dojos just like local ones. While a new remote pane initializes, Splinterm briefly displays an `Opening remote pane…` placeholder.

### Launching without a terminal

You don't need a shell open first to connect to a remote host. Any of these launch the graphical remote flow directly:

- The **Splinterm Remotes** app-menu entry or **Remote Dojos** desktop action
- Running `splinterm remotes` (returns immediately and opens the launcher window)
- Choosing **Remote Dojos…** inside local Recent Dojos
- Running **Open Remote…** from the command palette (opens a profile chooser and launches the chosen host in one new window)

The launcher window walks through three stages—**profile list → connecting status → remote Dojo picker**—and then turns into your remote terminal. Press `Escape` to step back one stage (which cleanly closes the SSH connection if you back out of a host).

### Authentication and SSH prompts

Splinterm never stores passwords, never runs `ssh-keyscan`, and always fails closed on unknown or changed host keys.

- **Started from a terminal:** OpenSSH can prompt on that terminal as usual (passwords, passphrases, hardware token PINs).
- **Started graphically (no controlling terminal):** OpenSSH tries `ssh-agent` first, then your own `SSH_ASKPASS` if configured, and finally Splinterm's built-in prompt inside the connection status window.
- **Built-in prompt:** Shows OpenSSH's prompt with a masked input field (pasting from a password manager works; `Y`/`N` for yes/no prompts; `Enter` submits; `Escape` cancels). Answers pass over a private per-attempt socket to that exact OpenSSH child, are wiped immediately after use, and are never logged. Closing the window or letting two minutes pass cancels OpenSSH cleanly without sending an empty password.
- **Connection errors:** If a connection fails, the status window stays open with the failure category, a one-line fix hint, and recent sanitized OpenSSH output. Press `R` or `Enter` to retry, or `C`/`Q`/`Escape` to close.

## Authority and boundaries

- **Human session behavior:** Once SSH authenticates your remote account, your remote window gets normal multiplexer authority (attach, split, type, resize, restore). Forced controller takeover (`Ctrl+Shift+U`) confirms in that remote window itself (never on the daemon host, and requires `integration.remote-agent-prompts=yes`). Remote windows do not publish local compositor focus or transfer terminal images.
- **If SSH drops:** Your local window closes and releases any input controller leases, but sends no kill requests—all remote shells stay alive in the remote `splinterd`.
- **Agent prompts across SSH:** By default (`integration.remote-agent-prompts=yes`), a remote window surfaces agent access prompts from the remote machine so you can approve or deny them where you're working.
- **Not a local sandbox:** Any process already running as your remote Unix account can invoke the installed graphical relay (`--graphical-stdio`) with your account's normal authority. Policy and consent bound cooperating tools; they don't contain hostile processes already running as your user.

:::note[No remote image transfer]
Remote graphical sessions intentionally omit Sixel/Kitty/iTerm2 image bodies. Unexpected image metadata fails the affected pane closed rather than opening an unvetted content channel.
:::

## Machine automation over SSH

Automated tools use a separate stdio relay entry point:

```bash
ssh -T \
  -o StrictHostKeyChecking=yes \
  ACCOUNT@HOST \
  /usr/bin/splinterm relay --stdio
```

On the remote host, `splinterd` authorizes the exact installed `splinterm-relay` executable against your owner-controlled policy—an SSH login alone grants no machine operations. Use a dedicated account or restricted SSH key when relay callers shouldn't inherit full shell access. Diagnostics stay on stderr so stdout remains pure protocol bytes.

For the complete profile schema, reconnect semantics, and relay contract, see [`docs/remote.md`](https://github.com/ministry-of-taste/splinterm/blob/main/docs/remote.md).
