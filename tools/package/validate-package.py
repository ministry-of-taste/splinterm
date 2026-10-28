#!/usr/bin/env python3
"""Validate the private Arch package without installing it."""

from __future__ import annotations

import argparse
import configparser
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import time

REQUIRED = {
    "usr/bin/generate-omarchy-theme.py",
    "usr/bin/splinterd",
    "usr/bin/splinterm",
    "usr/bin/splinterm-dojo-picker",
    "usr/bin/splinterm-dojos",
    "usr/bin/splinterm-relay",
    "usr/bin/splinterm-reopen",
    "usr/bin/splinterm-session-picker",
    "usr/bin/splinterm-sessions",
    "usr/bin/splinterm-pty-child",
    "usr/bin/splinterm-xdg-terminal-exec",
    "usr/lib/splinterm/integrations/omarchy-launch-screensaver",
    "usr/lib/systemd/user/app-splinterm.slice",
    "usr/lib/systemd/user/splinterd.service",
    "usr/share/applications/com.oldjobobo.splinterm.desktop",
    "usr/share/applications/com.oldjobobo.splinterm.remotes.desktop",
    "usr/share/icons/hicolor/scalable/apps/com.oldjobobo.splinterm.svg",
    "usr/share/metainfo/com.oldjobobo.splinterm.metainfo.xml",
    "usr/share/splinterm/omarchy/screensaver.ini",
    "usr/share/splinterm/package-provenance-v1.json",
    "usr/share/doc/splinterm/README.md",
    "usr/share/doc/splinterm/automation.md",
    "usr/share/doc/splinterm/cli.md",
    "usr/share/doc/splinterm/config.ini",
    "usr/share/doc/splinterm/configuration.md",
    "usr/share/doc/splinterm/headless.md",
    "usr/share/doc/splinterm/integrations.md",
    "usr/share/doc/splinterm/images.md",
    "usr/share/doc/splinterm/omarchy-tmux-reference.md",
    "usr/share/doc/splinterm/packaging.md",
    "usr/share/doc/splinterm/presets.example.toml",
    "usr/share/doc/splinterm/presets.md",
    "usr/share/doc/splinterm/remote.md",
    "usr/share/doc/splinterm/usage.md",
    "usr/share/doc/splinterm/accessibility.md",
    "usr/share/doc/splinterm/theme.json",
    "usr/share/doc/splinterm/xdg-terminals.list",
    "usr/share/licenses/splinterm/LICENSE",
    "usr/share/licenses/splinterm/THIRD_PARTY.md",
}
MCP_REQUIRED = {
    "usr/bin/splinterm-mcp",
    "usr/share/doc/splinterm/mcp.md",
    "usr/share/licenses/splinterm-mcp/LICENSE",
    "usr/share/licenses/splinterm-mcp/THIRD_PARTY.md",
}
PRIVATE_PROTOCOL_VERSION = 46
PACKAGE_PAIR_CAPABILITIES = (
    PRIVATE_PROTOCOL_VERSION, PRIVATE_PROTOCOL_VERSION, 1, 1, 2, 2, 1, 1
)
PACKAGE_PROVENANCE_KEYS = {
    "schema", "package", "architecture", "provenance", "source_commit",
    "build_version", "pair_build_identity", "daemon_sha256", "client_sha256",
}

# The provenance generator owns the test-only build markers (Plan 0061 D9
# and M3.17); validation searches the packaged binaries for the same bytes.
GENERATOR_SPEC = importlib.util.spec_from_file_location(
    "splinterm_package_provenance_generator",
    Path(__file__).resolve().with_name("generate-package-provenance.py"),
)
assert GENERATOR_SPEC is not None and GENERATOR_SPEC.loader is not None
GENERATOR = importlib.util.module_from_spec(GENERATOR_SPEC)
GENERATOR_SPEC.loader.exec_module(GENERATOR)

EXECUTABLES = {
    "usr/bin/generate-omarchy-theme.py",
    "usr/bin/splinterd",
    "usr/bin/splinterm",
    "usr/bin/splinterm-dojo-picker",
    "usr/bin/splinterm-dojos",
    "usr/bin/splinterm-mcp",
    "usr/bin/splinterm-relay",
    "usr/bin/splinterm-reopen",
    "usr/bin/splinterm-session-picker",
    "usr/bin/splinterm-sessions",
    "usr/bin/splinterm-pty-child",
    "usr/bin/splinterm-xdg-terminal-exec",
    "usr/lib/splinterm/integrations/omarchy-launch-screensaver",
}
RUNTIME_DEPENDENCIES = {
    "fontconfig",
    "freetype2",
    "gcc-libs",
    "glibc",
    "hicolor-icon-theme",
    "libxkbcommon",
    "noto-fonts-cjk",
    "noto-fonts-emoji",
    "pixman",
    "python",
    "ttf-jetbrains-mono-nerd",
    "wayland",
    "xdg-terminal-exec",
}


def run(command: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
    kwargs.setdefault("check", True)
    kwargs.setdefault("text", True)
    return subprocess.run(command, **kwargs)


def package_entries(package: Path) -> set[str]:
    listing = run(["bsdtar", "-tf", str(package)], capture_output=True).stdout
    return {line.removeprefix("./").rstrip("/") for line in listing.splitlines() if line}


def package_metadata(package: Path) -> str:
    return run(["bsdtar", "-xOf", str(package), ".PKGINFO"], capture_output=True).stdout


# The launcher acts only on `splinterm launcher-check` (Plan 0037 Milestone 4):
# 0 ready, 3 not now, 10 start the service, 11 restart it. The fake client
# returns the scripted statuses in order and every action lands in one log, so
# each case asserts the launcher's complete, ordered side effects. A status
# ending in "c" stands for an answer given in the trusted dialog (logged, and
# refused under --no-ask); "kill" makes the check die from SIGKILL.
LAUNCHER_CHECK = "launcher-check"
UNATTENDED_CHECK = "launcher-check --no-ask"
START = "systemctl --user start splinterd.service"
RELOAD = "systemctl --user daemon-reload"
RESTART = "systemctl --user restart splinterd.service"
AFTER_START = "launcher-check --after start"
AFTER_RESTART = "launcher-check --after restart"
UNATTENDED_AFTER_RESTART = "launcher-check --no-ask --after restart"
CONFIRMED = "dialog: 11"
DECLINED = "dialog: 3"
LAUNCH = "xdg-launch"
SCREENSAVER = ("--app-id=org.omarchy.screensaver", "--", "omarchy-screensaver")

# (name, launcher arguments, scripted check statuses, expected exit, expected
#  log, stderr fragment)
LAUNCHER_CASES = (
    ("matching build launches directly", (), [0], 0,
     [LAUNCHER_CHECK, LAUNCH], ""),
    ("no daemon: start once, then launch", (), [10, 0], 0,
     [LAUNCHER_CHECK, START, AFTER_START, LAUNCH], ""),
    ("idle other build: restart without asking", (), [11, 0], 0,
     [LAUNCHER_CHECK, RELOAD, RESTART, AFTER_RESTART, LAUNCH], ""),
    ("active other build, restart confirmed in the dialog", (), ["11c", 0], 0,
     [LAUNCHER_CHECK, CONFIRMED, RELOAD, RESTART, AFTER_RESTART, LAUNCH], ""),
    ("active other build, Not now: no side effects", (), ["3c"], 3,
     [LAUNCHER_CHECK, DECLINED], ""),
    ("unreadable 0.1 daemon, Not now: a failed ping restarts nothing", (), ["3c"], 3,
     [LAUNCHER_CHECK, DECLINED], ""),
    ("started daemon with an idle other build restarts once", (), [10, 11, 0], 0,
     [LAUNCHER_CHECK, START, AFTER_START, RELOAD, RESTART, AFTER_RESTART, LAUNCH], ""),
    ("started daemon asks, and Not now opens nothing", (), [10, "3c"], 3,
     [LAUNCHER_CHECK, START, AFTER_START, DECLINED], ""),
    ("started daemon never ready: no restart", (), [10, 1], 1,
     [LAUNCHER_CHECK, START, AFTER_START], ""),
    ("still not the installed build after a restart: no second restart", (), [11, 1], 1,
     [LAUNCHER_CHECK, RELOAD, RESTART, AFTER_RESTART], ""),
    ("a check status out of contract after a restart is not retried", (), [11, 11], 1,
     [LAUNCHER_CHECK, RELOAD, RESTART, AFTER_RESTART], "after the restart; no further restart"),
    ("an older client without the check restarts nothing", (), [2], 1,
     [LAUNCHER_CHECK], "status 2; nothing was restarted"),
    ("a check killed by a signal restarts nothing", (), ["kill"], 1,
     [LAUNCHER_CHECK], "status 137; nothing was restarted"),
    ("an untrusted or busy check fails closed", (), [1], 1,
     [LAUNCHER_CHECK], ""),
    ("the screensaver never asks", SCREENSAVER, [3], 3,
     [UNATTENDED_CHECK], ""),
    ("the screensaver still restarts an idle daemon", SCREENSAVER, [11, 0], 0,
     [UNATTENDED_CHECK, RELOAD, RESTART, UNATTENDED_AFTER_RESTART, LAUNCH], ""),
    ("a command launch never asks", ("--", "/usr/bin/true"), [3], 3,
     [UNATTENDED_CHECK], ""),
    ("an explicit app ID alone never asks", ("--app-id=org.example.tool",), [0], 0,
     [UNATTENDED_CHECK, LAUNCH], ""),
    ("a working directory stays interactive", ("--working-directory=/tmp",), [0], 0,
     [LAUNCHER_CHECK, LAUNCH], ""),
)


def write_launcher_fakes(fake: Path) -> None:
    (fake / "splinterm").write_text(
        "#!/bin/sh\n"
        "state=$SPLINTERM_TEST_STATE\n"
        "case $1 in\n"
        "  ping) printf 'ping\\n' >>\"$state/log\"; [ -f \"$state/ping-ok\" ]; exit ;;\n"
        "  launcher-check)\n"
        "    arguments=\" $* \"\n"
        "    printf '%s\\n' \"$*\" >>\"$state/log\"\n"
        "    set -- $(cat \"$state/statuses\")\n"
        "    [ $# -gt 0 ] || { echo 'launcher-check called too often' >&2; exit 99; }\n"
        "    status=$1; shift; printf '%s ' \"$@\" >\"$state/statuses\"\n"
        "    case $status in\n"
        "      kill) kill -KILL $$ ;;\n"
        "      *c)\n"
        "        case $arguments in\n"
        "          *' --no-ask '*) echo 'asked under --no-ask' >&2; exit 98 ;;\n"
        "        esac\n"
        "        printf 'dialog: %s\\n' \"${status%c}\" >>\"$state/log\"\n"
        "        exit \"${status%c}\" ;;\n"
        "    esac\n"
        "    exit \"$status\" ;;\n"
        "esac\n"
        "printf '%s\\n' \"$1\" >>\"$state/log\"\n"
        "printf '%s\\n' \"$@\" >\"$SPLINTERM_TEST_RECORD\"\n",
        encoding="utf-8",
    )
    (fake / "systemctl").write_text(
        "#!/bin/sh\n"
        "printf 'systemctl %s\\n' \"$*\" >>\"$SPLINTERM_TEST_STATE/log\"\n"
        "[ ! -f \"$SPLINTERM_TEST_STATE/systemctl-fails\" ]\n",
        encoding="utf-8",
    )
    for executable in (fake / "splinterm", fake / "systemctl"):
        executable.chmod(0o755)


def run_launcher_case(
    launcher: Path,
    environment: dict[str, str],
    state: Path,
    statuses: list[int | str],
    ping_ok: bool = False,
    systemctl_fails: bool = False,
    arguments: tuple[str, ...] = (),
) -> tuple[subprocess.CompletedProcess[str], list[str]]:
    for leftover in state.iterdir():
        leftover.unlink()
    (state / "statuses").write_text(" ".join(map(str, statuses)), encoding="utf-8")
    (state / "log").write_text("", encoding="utf-8")
    if ping_ok:
        (state / "ping-ok").touch()
    if systemctl_fails:
        (state / "systemctl-fails").touch()
    result = run(
        [str(launcher), *arguments],
        env=environment,
        timeout=10,
        check=False,
        capture_output=True,
    )
    return result, (state / "log").read_text(encoding="utf-8").splitlines()


def validate_launcher(root: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="splinterm-launcher-") as directory:
        fake = Path(directory)
        state = fake / "state"
        runtime = fake / "runtime"
        record = fake / "record"
        write_launcher_fakes(fake)
        state.mkdir()
        runtime.mkdir(mode=0o700)
        environment = os.environ.copy()
        environment.pop("SPLINTERM_SOCKET", None)
        environment.update(
            PATH=f"{fake}:{root / 'usr/bin'}:{environment['PATH']}",
            SPLINTERM_TEST_STATE=str(state),
            SPLINTERM_TEST_RECORD=str(record),
            XDG_RUNTIME_DIR=str(runtime),
        )
        launcher = root / "usr/bin/splinterm-xdg-terminal-exec"

        for name, arguments, statuses, code, expected, fragment in LAUNCHER_CASES:
            for ping_ok in (False, True):
                if record.exists():
                    record.unlink()
                result, log = run_launcher_case(
                    launcher, environment, state, statuses, ping_ok, arguments=arguments
                )
                assert result.returncode == code, (name, result)
                assert log == expected, (name, log)
                assert fragment in result.stderr, (name, result.stderr)
                # Nothing opens unless the check said the daemon is this build.
                assert record.exists() == (LAUNCH in expected), (name, record.exists())
                # The removed fallback: the launcher never pings to decide,
                # whether a ping would succeed or fail.
                assert "ping" not in log, (name, log)

        result, log = run_launcher_case(
            launcher, environment, state, [10], systemctl_fails=True
        )
        assert result.returncode == 1, result
        assert log == [LAUNCHER_CHECK, START], log
        assert "could not start splinterd.service" in result.stderr, result.stderr
        result, log = run_launcher_case(
            launcher, environment, state, [11], systemctl_fails=True
        )
        assert result.returncode == 1, result
        assert log == [LAUNCHER_CHECK, RELOAD], log
        assert "could not restart splinterd.service" in result.stderr, result.stderr

        # Concurrent launches decide one at a time: a held launcher lock
        # delays the check rather than letting two launches restart.
        lock = runtime / "splinterm-launcher.lock"
        holder = subprocess.Popen(["flock", str(lock), "sleep", "1"])
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and not lock.exists():
                time.sleep(0.01)
            time.sleep(0.1)
            started = time.monotonic()
            result, log = run_launcher_case(launcher, environment, state, [0])
            waited = time.monotonic() - started
        finally:
            holder.wait(timeout=10)
        assert result.returncode == 0, result
        assert log == [LAUNCHER_CHECK, LAUNCH], log
        assert waited >= 0.5, waited

        # A development socket is only pinged: never checked, started, or
        # restarted.
        development = dict(environment, SPLINTERM_SOCKET=str(fake / "dev.sock"))
        result, log = run_launcher_case(launcher, development, state, [])
        assert result.returncode == 1, result
        assert log == ["ping"], log
        assert "development socket is unavailable" in result.stderr, result.stderr
        result, log = run_launcher_case(launcher, development, state, [], ping_ok=True)
        assert result.returncode == 0, result
        assert log == ["ping", LAUNCH], log

        for arguments, expected in (
            ((), ["xdg-launch"]),
            (("--working-directory=/tmp/a b",), ["xdg-launch", "--working-directory=/tmp/a b"]),
            (
                (
                    "--working-directory=/tmp/a b",
                    "--app-id=org.omarchy.screensaver",
                    "--",
                    "/usr/bin/printf",
                    "",
                    "$(must-not-run)",
                ),
                [
                    "xdg-launch",
                    "--working-directory=/tmp/a b",
                    "--app-id=org.omarchy.screensaver",
                    "--",
                    "/usr/bin/printf",
                    "",
                    "$(must-not-run)",
                ],
            ),
        ):
            # Arguments survive the start and restart paths unchanged.
            for statuses in ([0], [10, 0], [11, 0]):
                result, _ = run_launcher_case(
                    launcher, environment, state, statuses, arguments=arguments
                )
                assert result.returncode == 0, result
                assert record.read_text(encoding="utf-8").splitlines() == expected

        for alias, expected in (
            ("splinterm-dojos", ["dojos"]),
            ("splinterm-sessions", ["dojos"]),
            ("splinterm-reopen", ["reopen"]),
        ):
            result, log = run_launcher_case(root / "usr/bin" / alias, environment, state, [0])
            assert result.returncode == 0, result
            assert record.read_text(encoding="utf-8").splitlines() == expected
            # Every alias passes the same daemon state boundary, and may ask.
            assert log == [LAUNCHER_CHECK, expected[0]], log
            result, log = run_launcher_case(root / "usr/bin" / alias, environment, state, ["3c"])
            assert result.returncode == 3, result
            assert log == [LAUNCHER_CHECK, DECLINED], log


def sealed_preflight_report(binary: Path, role: int) -> bytes:
    target = os.memfd_create(
        "splinterm-package-preflight",
        os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING | 0x0010,  # MFD_EXEC
    )
    try:
        with binary.open("rb") as source:
            while block := source.read(64 * 1024):
                remaining = memoryview(block)
                while remaining:
                    remaining = remaining[os.write(target, remaining):]
        os.fchmod(target, 0o755)
        fcntl.fcntl(
            target,
            fcntl.F_ADD_SEALS,
            fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL,
        )
        parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        process = None
        try:
            process = subprocess.Popen(
                [str(binary), "--splinterm-internal-preflight-launcher-v1"],
                stdin=child,
                stdout=subprocess.DEVNULL,
                stderr=target,
                env={},
                close_fds=True,
            )
            child.close()
            nonce = bytes(range(1, 33))
            parent.settimeout(5)
            parent.sendall(b"SPLTPF01" + struct.pack(">HBB", 1, role, 1) + nonce)
            report = parent.recv(99)
            status = process.wait(timeout=5)
            assert status == 0
            assert len(report) == 98
            assert report[:8] == b"SPLTPF01"
            assert struct.unpack(">HBB", report[8:12]) == (1, role, 1)
            assert report[12:44] == nonce
            assert parent.recv(1) == b""
            return report
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait()
            parent.close()
            child.close()
    finally:
        os.close(target)


def validate_package_provenance(root: Path) -> None:
    path = root / "usr/share/splinterm/package-provenance-v1.json"
    assert path.is_file() and not path.is_symlink()
    assert path.stat().st_size <= 4096
    assert path.stat().st_mode & 0o022 == 0
    record = json.loads(path.read_text(encoding="utf-8"))
    assert isinstance(record, dict) and set(record) == PACKAGE_PROVENANCE_KEYS
    assert record["schema"] == 1
    assert record["package"] == "splinterm"
    assert record["architecture"] == "x86_64"
    assert record["provenance"] in {"local-committed-head", "release-candidate"}
    assert re.fullmatch(r"[0-9a-f]{40}", record["source_commit"])
    assert record["source_commit"] != "0" * 40
    assert re.fullmatch(r"[0-9A-Za-z][0-9A-Za-z.+-]{0,63}", record["build_version"])
    daemon = root / "usr/bin/splinterd"
    client = root / "usr/bin/splinterm"
    assert record["daemon_sha256"] == hashlib.sha256(daemon.read_bytes()).hexdigest()
    assert record["client_sha256"] == hashlib.sha256(client.read_bytes()).hexdigest()

    digest = hashlib.sha256()
    digest.update(b"splinterm-package-pair-v1\0")
    digest.update(record["source_commit"].encode("ascii"))
    digest.update(b"\0")
    digest.update(record["provenance"].encode("ascii"))
    digest.update(b"\0")
    digest.update(record["build_version"].encode("ascii"))
    for value in PACKAGE_PAIR_CAPABILITIES:
        digest.update(value.to_bytes(2, "big"))
    assert record["pair_build_identity"] == digest.hexdigest()

    daemon_report = sealed_preflight_report(daemon, 1)
    client_report = sealed_preflight_report(client, 2)
    assert daemon_report[44:76] == client_report[44:76]
    assert daemon_report[44:76].hex() == record["pair_build_identity"]
    expected_version = tuple(int(part) for part in record["build_version"].split("-", 1)[0].split("."))
    assert len(expected_version) == 3
    assert struct.unpack(">HHH", daemon_report[76:82]) == expected_version
    assert daemon_report[82:] == client_report[82:]


def validate_test_only_builds(root: Path, allow_vm_acceptance: bool) -> bool:
    """Refuse packaged binaries built with a test-only feature.

    No packaged binary may carry the integration-test salt marker. A daemon
    built with the TEST-ONLY `vm-acceptance-live-handoff` feature (Plan 0061
    M3.17) is accepted only in a local-committed-head package validated with
    `--allow-vm-acceptance-live-handoff`, whose daemon must then carry it.
    Returns whether this is such a package.
    """
    record = json.loads(
        (root / "usr/share/splinterm/package-provenance-v1.json").read_text(
            encoding="utf-8"
        )
    )
    binaries = [
        root / "usr/bin" / name
        for name in (
            "splinterd",
            "splinterm",
            "splinterm-relay",
            "splinterm-pty-child",
            "splinterm-mcp",
        )
        if (root / "usr/bin" / name).is_file()
    ]
    for binary in binaries:
        if GENERATOR.contains_integration_test_marker(binary):
            raise AssertionError(
                f"{binary.name} was built with the splinterd integration-test feature"
            )
    daemon = root / "usr/bin/splinterd"
    client = root / "usr/bin/splinterm"
    try:
        marked = GENERATOR.check_vm_acceptance(
            record["provenance"], daemon, client, allow_vm_acceptance
        )
    except ValueError as error:
        raise AssertionError(str(error)) from error
    for binary in binaries:
        if binary not in (daemon, client) and GENERATOR.contains_vm_acceptance_marker(binary):
            raise AssertionError(
                f"{binary.name} carries the test-only vm-acceptance-live-handoff marker"
            )
    return marked


def validate_systemd_unit(root: Path) -> None:
    unit = (root / "usr/lib/systemd/user/splinterd.service").read_text(encoding="utf-8")
    required = {
        "EnvironmentFile=-%h/.config/splinterm/daemon.env",
        "UnsetEnvironment=SPLINTERM_ENABLE_DEV_ATTACH",
        "ExecStart=/usr/bin/splinterd --require-workload-cgroups",
        "ExecReload=/usr/bin/kill -HUP $MAINPID",
        "TasksMax=2048",
        "MemoryHigh=75%",
        "KillSignal=SIGINT",
        "KillMode=mixed",
        "TimeoutStopSec=90",
    }
    missing = {line for line in required if line not in unit.splitlines()}
    assert not missing, (
        "systemd unit is missing headless safety or resource settings: "
        f"{sorted(missing)}"
    )
    assert "graphical-session.target" not in unit
    unset_environment = {
        name
        for line in unit.splitlines()
        if line.startswith("UnsetEnvironment=")
        for name in line.partition("=")[2].split()
    }
    assert {"DISPLAY", "WAYLAND_DISPLAY"}.isdisjoint(unset_environment)
    for inherited_shell_restriction in (
        "NoNewPrivileges=",
        "PrivateTmp=",
        "RestrictSUIDSGID=",
        "UMask=",
    ):
        assert inherited_shell_restriction not in unit

    workload_slice = (
        root / "usr/lib/systemd/user/app-splinterm.slice"
    ).read_text(encoding="utf-8")
    slice_lines = set(workload_slice.splitlines())
    required_slice = {
        "StopWhenUnneeded=yes",
        "[Slice]",
        "MemoryHigh=75%",
    }
    missing_slice = required_slice - slice_lines
    assert not missing_slice, (
        "workload slice is missing aggregate guards: "
        f"{sorted(missing_slice)}"
    )
    assert not any(line.startswith("TasksMax=") for line in slice_lines), (
        "workload units must inherit the user manager task limit"
    )
    assert not any(line.startswith("MemoryMax=") for line in slice_lines)


def validate_headless_runtime(daemon: Path, client: Path, picker: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="splinterm-headless-") as directory:
        runtime = Path(directory)
        socket = runtime / "splinterd.sock"
        policy = runtime / "policy.json"
        executable = client.resolve()
        policy.write_text(
            json.dumps({
                "schema": "splinterm.policy.v2",
                "rules": [{
                    "id": "package-headless-read",
                    "executable": {
                        "path": str(executable),
                        "sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
                    },
                    "scopes": ["topology_metadata_read"],
                    "resources": [{"kind": "daemon"}],
                    "limits": {"max_results": 16},
                }],
            }),
            encoding="utf-8",
        )
        policy.chmod(0o600)
        environment = os.environ.copy()
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "SPLINTERM_ENABLE_DEV_ATTACH"):
            environment.pop(name, None)
        environment.update(
            HOME=str(runtime),
            XDG_CONFIG_HOME=str(runtime / "config"),
            XDG_RUNTIME_DIR=str(runtime),
            XDG_STATE_HOME=str(runtime / "state"),
            SPLINTERM_SOCKET=str(socket),
            SPLINTERM_POLICY=str(policy),
        )
        process = subprocess.Popen(
            [str(daemon)],
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    stderr = process.stderr.read() if process.stderr else ""
                    raise AssertionError(f"headless daemon exited during startup: {stderr[-2000:]}")
                if socket.exists():
                    ping = subprocess.run(
                        [str(client), "ping"],
                        env=environment,
                        capture_output=True,
                        text=True,
                        timeout=5,
                    )
                    if ping.returncode == 0:
                        break
                time.sleep(0.02)
            else:
                raise AssertionError("headless daemon did not become ready")

            listed = subprocess.run(
                [str(client), "list"],
                env=environment,
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert listed.returncode == 0, listed.stderr

            picked = subprocess.run(
                [sys.executable, str(picker), "--splinterm", str(client), "list"],
                env=environment,
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert picked.returncode == 0, picked.stderr
            assert picked.stdout == ""

            spoofed = environment.copy()
            spoofed.update(
                SPLINTERM_LAIR_ID="018f4d8c-2a18-4b31-8c2f-9e7c5de77101",
                SPLINTERM_DOJO_ID="018f4d8c-2a18-4b31-8c2f-9e7c5de77102",
                SPLINTERM_SPLINT_ID="018f4d8c-2a18-4b31-8c2f-9e7c5de77103",
                SPLINTERM_SPLINT_INCARNATION="1",
            )
            denied = subprocess.run(
                [
                    str(client),
                    "--output",
                    "json",
                    "--schema-major",
                    "2",
                    "--timeout-ms",
                    "5000",
                    "new",
                    "context-is-not-authority",
                    "--",
                    "/bin/true",
                ],
                env=spoofed,
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert denied.returncode == 3, (denied.stdout, denied.stderr)
            denied_document = json.loads(denied.stdout)
            assert denied_document["ok"] is False
            assert denied_document["error"]["code"] == "unauthorized"
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=90)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        assert process.returncode == 0
        assert not socket.exists(), "headless daemon left its socket after clean shutdown"


def validate_picker_runtime(daemon: Path, client: Path, picker: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="splinterm-picker-runtime-") as directory:
        runtime = Path(directory)
        socket = runtime / "splinterd.sock"
        policy = runtime / "policy.json"
        executable = client.resolve()
        identity = {
            "path": str(executable),
            "sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
        }

        def install_rule(
            rule_id: str,
            scopes: list[str],
            resources: list[dict[str, object]],
            max_spawn_count: int | None = None,
        ) -> None:
            limits: dict[str, int] = {
                "max_returned_rows": 16,
                "max_returned_bytes": 8 * 1024 * 1024,
                "max_live_subscriptions": 1,
            }
            if max_spawn_count is not None:
                limits["max_spawn_count"] = max_spawn_count
            policy.write_text(
                json.dumps({
                    "schema": "splinterm.policy.v2",
                    "rules": [{
                        "id": rule_id,
                        "executable": identity,
                        "scopes": scopes,
                        "resources": resources,
                        "limits": limits,
                    }],
                }),
                encoding="utf-8",
            )
            policy.chmod(0o600)

        install_rule(
            "picker-bootstrap",
            ["topology_metadata_read", "process_spawn", "topology_layout_mutate"],
            [{"kind": "daemon"}],
            1,
        )
        environment = os.environ.copy()
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "SPLINTERM_ENABLE_DEV_ATTACH"):
            environment.pop(name, None)
        environment.update(
            HOME=str(runtime),
            XDG_CONFIG_HOME=str(runtime / "config"),
            XDG_RUNTIME_DIR=str(runtime),
            XDG_STATE_HOME=str(runtime / "state"),
            SPLINTERM_SOCKET=str(socket),
            SPLINTERM_POLICY=str(policy),
        )
        process = subprocess.Popen(
            [str(daemon)],
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )

        def machine(arguments: list[str], env: dict[str, str] = environment) -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                [
                    str(client),
                    "--output",
                    "json",
                    "--schema-major",
                    "2",
                    "--timeout-ms",
                    "10000",
                    *arguments,
                ],
                env=env,
                capture_output=True,
                text=True,
                timeout=15,
            )

        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    stderr = process.stderr.read() if process.stderr else ""
                    raise AssertionError(f"picker daemon exited during startup: {stderr[-2000:]}")
                if socket.exists():
                    ping = subprocess.run(
                        [str(client), "ping"],
                        env=environment,
                        capture_output=True,
                        text=True,
                        timeout=5,
                    )
                    if ping.returncode == 0:
                        break
                time.sleep(0.02)
            else:
                raise AssertionError("picker daemon did not become ready")

            created = machine([
                "new",
                "picker-reference",
                "--",
                "/bin/sh",
                "-c",
                "printf 'picker-parent-ready\\n'; exec sleep 30",
            ])
            assert created.returncode == 0, (created.stdout, created.stderr)
            parent = json.loads(created.stdout)["resource"]
            context = environment.copy()
            context.update(
                SPLINTERM_LAIR_ID=parent["lair_id"],
                SPLINTERM_DOJO_ID=parent["dojo_id"],
                SPLINTERM_SPLINT_ID=parent["splint_id"],
                SPLINTERM_SPLINT_INCARNATION=str(parent["incarnation"]),
            )
            install_rule(
                "picker-split",
                ["topology_metadata_read", "process_spawn", "topology_layout_mutate"],
                [
                    {"kind": "daemon"},
                    {
                        "kind": "splint",
                        "splint_id": parent["splint_id"],
                        "incarnation": parent["incarnation"],
                    },
                ],
                1,
            )
            process.send_signal(signal.SIGHUP)
            time.sleep(0.2)

            selected = subprocess.run(
                [sys.executable, str(picker), "--splinterm", str(client), "context"],
                env=context,
                capture_output=True,
                text=True,
                timeout=15,
            )
            assert selected.returncode == 0, (selected.stdout, selected.stderr)
            assert json.loads(selected.stdout)["incarnation"] == parent["incarnation"]

            split = subprocess.run(
                [
                    sys.executable,
                    str(picker),
                    "--splinterm",
                    str(client),
                    "split-context",
                    "--axis",
                    "vertical",
                    "--side",
                    "second",
                    "--",
                    "/bin/sh",
                    "-c",
                    "printf 'picker-child-ready\\n'; exec sleep 30",
                ],
                env=context,
                capture_output=True,
                text=True,
                timeout=15,
            )
            assert split.returncode == 0, (split.stdout, split.stderr)
            child = json.loads(split.stdout)["resource"]
            child_context = context.copy()
            child_context.update(
                SPLINTERM_LAIR_ID=child["lair_id"],
                SPLINTERM_DOJO_ID=child["dojo_id"],
                SPLINTERM_SPLINT_ID=child["splint_id"],
                SPLINTERM_SPLINT_INCARNATION=str(child["incarnation"]),
            )
            install_rule(
                "picker-read-child",
                ["topology_metadata_read", "terminal_visible_read", "terminal_subscribe"],
                [
                    {"kind": "daemon"},
                    {
                        "kind": "splint",
                        "splint_id": child["splint_id"],
                        "incarnation": child["incarnation"],
                    },
                ],
            )
            process.send_signal(signal.SIGHUP)
            time.sleep(0.2)

            snapshot = subprocess.run(
                [
                    sys.executable,
                    str(picker),
                    "--splinterm",
                    str(client),
                    "snapshot-context",
                ],
                env=child_context,
                capture_output=True,
                text=True,
                timeout=15,
            )
            assert snapshot.returncode == 0, (snapshot.stdout, snapshot.stderr)
            snapshot_document = json.loads(snapshot.stdout)
            text = "".join(
                cell["text"]
                for row in snapshot_document["data"]["rows"]
                for cell in row["cells"]
            )
            assert "picker-child-ready" in text

            denied = subprocess.run(
                [
                    sys.executable,
                    str(picker),
                    "--splinterm",
                    str(client),
                    "send-context",
                    "literal-input",
                ],
                env=child_context,
                capture_output=True,
                text=True,
                timeout=15,
            )
            assert denied.returncode == 3, (denied.stdout, denied.stderr)
            assert denied.stdout == ""
            assert "unauthorized" in denied.stderr
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=90)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        assert process.returncode == 0
        assert not socket.exists(), "picker daemon left its socket after clean shutdown"

def encode_private_frame(document: dict[str, object]) -> bytes:
    body = json.dumps(document, separators=(",", ":")).encode("utf-8")
    assert 0 < len(body) <= 16 * 1024 * 1024
    return len(body).to_bytes(4, "big") + body


def read_exact_fd(descriptor: int, length: int, timeout: float = 10) -> bytes:
    deadline = time.monotonic() + timeout
    chunks = bytearray()
    while len(chunks) < length:
        remaining = deadline - time.monotonic()
        assert remaining > 0, "timed out reading relay output"
        readable, _, _ = select.select([descriptor], [], [], remaining)
        assert readable, "timed out reading relay output"
        chunk = os.read(descriptor, length - len(chunks))
        assert chunk, "relay output closed during a private frame"
        chunks.extend(chunk)
    return bytes(chunks)


def read_private_frame(pipe) -> dict[str, object]:
    descriptor = pipe.fileno()
    length = int.from_bytes(read_exact_fd(descriptor, 4), "big")
    assert 0 < length <= 16 * 1024 * 1024
    return json.loads(read_exact_fd(descriptor, length))


def validate_relay_runtime(daemon: Path, client: Path, relay: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="splinterm-relay-package-") as directory:
        runtime = Path(directory)
        socket = runtime / "splinterd.sock"
        policy = runtime / "policy.json"
        executable = relay.resolve()
        policy.write_text(
            json.dumps({
                "schema": "splinterm.policy.v2",
                "rules": [{
                    "id": "package-relay-read",
                    "executable": {
                        "path": str(executable),
                        "sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
                    },
                    "scopes": ["topology_metadata_read", "topology_subscribe"],
                    "resources": [{"kind": "daemon"}],
                    "limits": {"max_results": 16, "max_live_subscriptions": 1},
                }],
            }),
            encoding="utf-8",
        )
        policy.chmod(0o600)
        environment = os.environ.copy()
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "SPLINTERM_ENABLE_DEV_ATTACH"):
            environment.pop(name, None)
        environment.update(
            HOME=str(runtime),
            XDG_CONFIG_HOME=str(runtime / "config"),
            XDG_RUNTIME_DIR=str(runtime),
            XDG_STATE_HOME=str(runtime / "state"),
            SPLINTERM_SOCKET=str(socket),
            SPLINTERM_POLICY=str(policy),
        )
        daemon_process = subprocess.Popen(
            [str(daemon)],
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        relay_process = None
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and not socket.exists():
                assert daemon_process.poll() is None, "relay test daemon exited during startup"
                time.sleep(0.02)
            assert socket.exists(), "relay test daemon did not create its socket"
            inherited = open("/dev/null", "rb")
            os.set_inheritable(inherited.fileno(), True)
            inherited_fd = inherited.fileno()
            inherited_target = os.readlink(f"/proc/self/fd/{inherited_fd}")
            relay_process = subprocess.Popen(
                [str(client), "relay", "--stdio"],
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                pass_fds=(inherited_fd,),
            )
            inherited.close()
            assert relay_process.stdin is not None
            assert relay_process.stdout is not None
            relay_process.stdin.write(encode_private_frame({
                "type": "hello",
                "minimum_version": PRIVATE_PROTOCOL_VERSION,
                "maximum_version": PRIVATE_PROTOCOL_VERSION,
                "role": "automation",
            }))
            relay_process.stdin.flush()
            hello = read_private_frame(relay_process.stdout)
            assert (
                hello["type"] == "hello"
                and hello["version"] == PRIVATE_PROTOCOL_VERSION
            )
            inherited_path = Path(f"/proc/{relay_process.pid}/fd/{inherited_fd}")
            assert not inherited_path.exists() or os.readlink(inherited_path) != inherited_target

            relay_process.stdin.write(encode_private_frame({
                "type": "request",
                "request_id": 1,
                "request": {"type": "inspect_topology"},
            }))
            relay_process.stdin.flush()
            topology = read_private_frame(relay_process.stdout)
            assert topology["type"] == "response" and topology["request_id"] == 1

            relay_process.stdin.write(encode_private_frame({
                "type": "request",
                "request_id": 2,
                "request": {
                    "type": "create_lair",
                    "expected_topology_revision": 0,
                    "name": "must-be-denied",
                    "launch": {
                        "cwd": str(runtime),
                        "command": ["/bin/true"],
                        "shell": None,
                        "login_shell": False,
                        "scrollback_lines": 100,
                    },
                },
            }))
            relay_process.stdin.flush()
            denied = read_private_frame(relay_process.stdout)
            assert denied["type"] == "error"
            assert denied["request_id"] == 2
            assert denied["error"]["code"] == "unauthorized"

            relay_process.stdin.write(encode_private_frame({
                "type": "request",
                "request_id": 3,
                "request": {"type": "subscribe_topology"},
            }))
            relay_process.stdin.flush()
            subscribed = read_private_frame(relay_process.stdout)
            assert subscribed["type"] == "response" and subscribed["request_id"] == 3, subscribed

            relay_process.kill()
            relay_process.wait(timeout=10)
            assert daemon_process.poll() is None

            relay_process = subprocess.Popen(
                [str(client), "relay", "--stdio"],
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert relay_process.stdin is not None
            assert relay_process.stdout is not None
            relay_process.stdin.write(encode_private_frame({
                "type": "hello",
                "minimum_version": PRIVATE_PROTOCOL_VERSION,
                "maximum_version": PRIVATE_PROTOCOL_VERSION,
                "role": "automation",
            }))
            relay_process.stdin.flush()
            assert read_private_frame(relay_process.stdout)["type"] == "hello"
            relay_process.stdin.write(encode_private_frame({
                "type": "cancel",
                "request_id": 30,
            }))
            relay_process.stdin.flush()
            cancelled = read_private_frame(relay_process.stdout)
            assert cancelled["type"] == "error" and cancelled["request_id"] == 30
            assert cancelled["error"]["code"] == "request_not_found"
            relay_process.stdin.write(encode_private_frame({
                "type": "request",
                "request_id": 31,
                "request": {"type": "subscribe_topology"},
            }))
            relay_process.stdin.flush()
            replacement_subscription = read_private_frame(relay_process.stdout)
            assert replacement_subscription["type"] == "response"
            assert replacement_subscription["request_id"] == 31

            daemon_process.send_signal(signal.SIGINT)
            daemon_process.wait(timeout=90)
            assert daemon_process.returncode == 0
            relay_process.wait(timeout=10)
            assert relay_process.returncode == 0
            stderr = relay_process.stderr.read() if relay_process.stderr else b""
            assert not stderr
            assert not socket.exists()

            daemon_process = subprocess.Popen(
                [str(daemon)],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and not socket.exists():
                assert daemon_process.poll() is None, "restarted relay test daemon exited"
                time.sleep(0.02)
            assert socket.exists(), "restarted relay test daemon did not create its socket"

            restarted_relay = subprocess.Popen(
                [str(client), "relay", "--stdio"],
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert restarted_relay.stdin is not None
            assert restarted_relay.stdout is not None
            restarted_relay.stdin.write(encode_private_frame({
                "type": "hello",
                "minimum_version": PRIVATE_PROTOCOL_VERSION,
                "maximum_version": PRIVATE_PROTOCOL_VERSION,
                "role": "automation",
            }))
            restarted_relay.stdin.flush()
            assert read_private_frame(restarted_relay.stdout)["type"] == "hello"
            restarted_relay.stdin.close()
            restarted_relay.wait(timeout=10)
            assert restarted_relay.returncode == 0
            assert not (restarted_relay.stderr.read() if restarted_relay.stderr else b"")

            malformed_relay = subprocess.Popen(
                [str(client), "relay", "--stdio"],
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert malformed_relay.stdin is not None
            malformed_relay.stdin.write((16 * 1024 * 1024 + 1).to_bytes(4, "big"))
            malformed_relay.stdin.flush()
            malformed_relay.stdin.close()
            malformed_relay.wait(timeout=10)
            assert malformed_relay.returncode == 0
            assert not (malformed_relay.stderr.read() if malformed_relay.stderr else b"")

            broken_output_relay = subprocess.Popen(
                [str(client), "relay", "--stdio"],
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert broken_output_relay.stdin is not None
            assert broken_output_relay.stdout is not None
            broken_output_relay.stdin.write(encode_private_frame({
                "type": "hello",
                "minimum_version": PRIVATE_PROTOCOL_VERSION,
                "maximum_version": PRIVATE_PROTOCOL_VERSION,
                "role": "automation",
            }))
            broken_output_relay.stdin.flush()
            broken_output_relay.stdout.close()
            broken_output_relay.wait(timeout=10)
            broken_output_relay.stdin.close()
            assert broken_output_relay.returncode == 1
            broken_stderr = (
                broken_output_relay.stderr.read() if broken_output_relay.stderr else b""
            )
            assert b"daemon-to-stdout relay failed" in broken_stderr
        finally:
            if relay_process is not None and relay_process.poll() is None:
                relay_process.kill()
                relay_process.wait(timeout=5)
            if daemon_process.poll() is None:
                daemon_process.send_signal(signal.SIGINT)
                try:
                    daemon_process.wait(timeout=90)
                except subprocess.TimeoutExpired:
                    daemon_process.kill()
                    daemon_process.wait(timeout=5)
        assert daemon_process.returncode == 0
        assert not socket.exists(), "relay test daemon left its socket after shutdown"


def validate_theme_generator(root: Path) -> None:
    colors = {
        "accent": "#010203", "bg": "#101112", "darker_bg": "#000000",
        "selection": "#202122", "muted": "#303132", "color8": "#404142",
        "fg": "#d0d1d2",
        "bright_fg": "#ffffff", "red": "#800000", "yellow": "#808000",
        "green": "#008000", "cyan": "#008080", "blue": "#000080",
        "magenta": "#800080", "bright_red": "#ff0000",
        "bright_yellow": "#ffff00", "bright_green": "#00ff00",
        "bright_cyan": "#00ffff", "bright_blue": "#0000ff",
        "bright_magenta": "#ff00ff",
    }
    with tempfile.TemporaryDirectory(prefix="splinterm-theme-") as directory:
        directory = Path(directory)
        source = directory / "colors.toml"
        source.write_text("".join(f'{key} = "{value}"\n' for key, value in colors.items()))
        (directory / "foot.ini").write_text("[colors-dark]\nalpha=0.85\n")
        output = directory / "theme.json"
        run([str(root / "usr/bin/generate-omarchy-theme.py"), str(source), "--output", str(output)])
        generated = json.loads(output.read_text(encoding="utf-8"))
        assert generated["background"] == colors["bg"]
        assert generated["cursor"] == colors["accent"]
        assert generated["selection"] == colors["selection"]
        assert generated["active_tab_background"] == colors["color8"]
        assert generated["active_tab_foreground"] == colors["fg"]
        assert generated["alpha"] == 0.85
        assert len(generated["ansi"]) == 16


def validate_packaged_omarchy_workflows(root: Path) -> None:
    binary = root / "usr/bin/splinterm"
    docs = root / "usr/share/doc/splinterm"
    with tempfile.TemporaryDirectory(prefix="splinterm-package-omarchy-") as directory:
        temporary = Path(directory)
        config = temporary / "config.ini"
        config.write_text("[key-bindings]\nprofile=omarchy-tmux\n", encoding="utf-8")
        environment = os.environ.copy()
        environment.update({
            "HOME": str(temporary),
            "XDG_CONFIG_HOME": str(temporary / "config"),
            "XDG_STATE_HOME": str(temporary / "state"),
            "XDG_RUNTIME_DIR": str(temporary / "runtime"),
            "SPLINTERM_CONFIG": str(config),
        })
        (temporary / "runtime").mkdir(mode=0o700)

        keymap = run(
            [str(binary), "keymap", "show"],
            env=environment,
            capture_output=True,
        ).stdout
        assert "Prefix [" in keymap
        assert "copy-mode.enter" in keymap
        assert "Prefix ?" in keymap
        assert "app.binding-help" in keymap

        presets = run(
            [str(binary), "preset", "list"],
            env=environment,
            capture_output=True,
        ).stdout
        for name in ("omarchy.t", "omarchy.tdl", "omarchy.tds", "omarchy.tdlm", "omarchy.tsl"):
            assert name in presets
        run(
            [str(binary), "preset", "check", str(docs / "presets.example.toml")],
            env=environment,
            capture_output=True,
        )

        shell = run(
            [str(binary), "preset", "shell-init", "omarchy", "--shell", "bash"],
            env=environment,
            capture_output=True,
        ).stdout
        for function in ("s", "sdl", "sds", "sdlm", "ssl"):
            assert f"{function}()" in shell
        assert "eval " not in shell
        assert "sh -c" not in shell


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("package", type=Path)
    parser.add_argument("--mcp-package", required=True, type=Path)
    parser.add_argument(
        "--allow-vm-acceptance-live-handoff",
        action="store_true",
        help=(
            "TEST-ONLY: accept a local VM acceptance package whose daemon forces "
            "live handoff on (never for release packages)"
        ),
    )
    args = parser.parse_args()
    package = args.package.resolve()
    mcp_package = args.mcp_package.resolve()
    for candidate in (package, mcp_package):
        if not candidate.is_file() or candidate.stat().st_size > 256 * 1024 * 1024:
            raise SystemExit(f"package is missing or unexpectedly large: {candidate}")

    entries = package_entries(package)
    mcp_entries = package_entries(mcp_package)
    missing = REQUIRED - entries
    assert not missing, f"missing package paths: {sorted(missing)}"
    missing_mcp = MCP_REQUIRED - mcp_entries
    assert not missing_mcp, f"missing MCP package paths: {sorted(missing_mcp)}"
    assert "usr/bin/splinterm-mcp" not in entries, "MCP adapter must remain an opt-in split package"
    forbidden = [
        entry for entry in entries | mcp_entries
        if entry.startswith(("home/", "etc/", "usr/share/omarchy/"))
    ]
    assert not forbidden, f"package mutates forbidden paths: {forbidden}"

    with tempfile.TemporaryDirectory(prefix="splinterm-package-root-") as directory:
        root = Path(directory)
        run(["bsdtar", "-xf", str(package), "-C", str(root)])
        run(["bsdtar", "-xf", str(mcp_package), "-C", str(root)])
        for relative in EXECUTABLES:
            mode = (root / relative).stat().st_mode
            assert mode & stat.S_IXUSR, f"{relative} is not executable"
        assert os.readlink(root / "usr/bin/splinterm-session-picker") == "splinterm-dojo-picker"
        for binary in ("splinterm", "splinterd", "splinterm-mcp", "splinterm-relay", "splinterm-pty-child"):
            result = run(["ldd", str(root / "usr/bin" / binary)], capture_output=True)
            assert "not found" not in result.stdout

        desktop = root / "usr/share/applications/com.oldjobobo.splinterm.desktop"
        metadata = root / "usr/share/metainfo/com.oldjobobo.splinterm.metainfo.xml"
        run(["desktop-file-validate", str(desktop)])
        run(["appstreamcli", "validate", "--no-net", str(metadata)])
        desktop_config = configparser.ConfigParser(interpolation=None, strict=True)
        desktop_config.optionxform = str
        with desktop.open(encoding="utf-8") as desktop_file:
            desktop_config.read_file(desktop_file)
        assert desktop_config["Desktop Entry"]["Exec"] == "splinterm-xdg-terminal-exec"
        assert desktop_config["Desktop Entry"]["Actions"] == "New;Dojos;Reopen;Remotes;"
        assert desktop_config["Desktop Entry"]["Icon"] == "com.oldjobobo.splinterm"
        assert desktop_config["Desktop Entry"]["X-TerminalArgAppId"] == "--app-id="
        assert desktop_config["Desktop Action New"]["Exec"] == "splinterm-xdg-terminal-exec --new"
        assert desktop_config["Desktop Action Dojos"]["Name"] == "Recent Dojos"
        assert desktop_config["Desktop Action Dojos"]["Exec"] == "splinterm-dojos"
        assert desktop_config["Desktop Action Reopen"]["Exec"] == "splinterm-reopen"
        assert desktop_config["Desktop Action Remotes"]["Name"] == "Remote Dojos"
        assert desktop_config["Desktop Action Remotes"]["Exec"] == "splinterm remotes"
        remotes_desktop = root / "usr/share/applications/com.oldjobobo.splinterm.remotes.desktop"
        run(["desktop-file-validate", str(remotes_desktop)])
        remotes_config = configparser.ConfigParser(interpolation=None, strict=True)
        remotes_config.optionxform = str
        with remotes_desktop.open(encoding="utf-8") as remotes_file:
            remotes_config.read_file(remotes_file)
        assert remotes_config.sections() == ["Desktop Entry"], "Splinterm Remotes has no actions"
        remotes_entry = remotes_config["Desktop Entry"]
        assert remotes_entry["Name"] == "Splinterm Remotes"
        assert remotes_entry["Exec"] == "splinterm remotes"
        assert remotes_entry["TryExec"] == "splinterm"
        assert remotes_entry["Icon"] == "com.oldjobobo.splinterm"
        assert remotes_entry["Terminal"] == "false"
        assert "TerminalEmulator" not in remotes_entry["Categories"].split(";")
        assert "Actions" not in remotes_entry
        assert not any(key.startswith("X-TerminalArg") for key in remotes_entry)

        screensaver_profile = root / "usr/share/splinterm/omarchy/screensaver.ini"
        profile = configparser.ConfigParser(interpolation=None, strict=True)
        with screensaver_profile.open(encoding="utf-8") as profile_file:
            profile.read_file(profile_file)
        assert profile["main"]["font"] == "JetBrains Mono Nerd Font:style=Regular"
        assert profile["main"]["font-point-size"] == "18"
        assert profile["main"]["font-sizing-policy"] == "physical-dpi"
        assert {
            profile["main"][name]
            for name in ("padding-left", "padding-right", "padding-top", "padding-bottom")
        } == {"0"}
        assert profile["colors"]["alpha"] == "1.0"
        assert profile["colors"]["blur"] == "no"
        profile_environment = os.environ.copy()
        profile_environment["SPLINTERM_CONFIG"] = str(screensaver_profile)
        run(
            [str(root / "usr/bin/splinterm"), "config", "check"],
            env=profile_environment,
            capture_output=True,
        )

        pkginfo = package_metadata(package)
        dependencies = {
            line.split(" = ", 1)[1].split(">", 1)[0].split("<", 1)[0].split("=", 1)[0]
            for line in pkginfo.splitlines() if line.startswith("depend = ")
        }
        assert RUNTIME_DEPENDENCIES <= dependencies
        mcp_pkginfo = package_metadata(mcp_package)
        assert "pkgname = splinterm-mcp" in mcp_pkginfo
        assert any(
            line.startswith("depend = splinterm=")
            for line in mcp_pkginfo.splitlines()
        ), "MCP split package must depend on the exact main package version"
        validate_systemd_unit(root)
        validate_package_provenance(root)
        vm_acceptance = validate_test_only_builds(
            root, args.allow_vm_acceptance_live_handoff
        )
        validate_headless_runtime(
            root / "usr/bin/splinterd",
            root / "usr/bin/splinterm",
            root / "usr/bin/splinterm-dojo-picker",
        )
        validate_picker_runtime(
            root / "usr/bin/splinterd",
            root / "usr/bin/splinterm",
            root / "usr/bin/splinterm-dojo-picker",
        )
        validate_relay_runtime(
            root / "usr/bin/splinterd",
            root / "usr/bin/splinterm",
            root / "usr/bin/splinterm-relay",
        )
        validate_theme_generator(root)
        validate_launcher(root)
        validate_packaged_omarchy_workflows(root)
        run([sys.executable, str(Path(__file__).with_name("validate-mcp-package.py")), str(root)], timeout=180)

    print(f"Package validation passed: {package} + {mcp_package}")
    if vm_acceptance:
        print(
            "TEST-ONLY VM acceptance package. "
            "Install it only in the VM testbed; never publish or ship it."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
