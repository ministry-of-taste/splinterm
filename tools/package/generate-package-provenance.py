#!/usr/bin/env python3
"""Generate one canonical Splinterm package/build provenance record."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re

COMMIT = re.compile(r"[0-9a-f]{40}")
VERSION = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+-]{0,63}")
KINDS = {"local-committed-head", "release-candidate", "nix-store"}
PRIVATE_PROTOCOL = 45
HANDOFF_PROTOCOL = 1
TERMINAL_CHECKPOINT = 2
DESCRIPTOR_MANIFEST = 1
# Every `integration-test` build of the daemon library carries this marker
# before its build-identity salt (crates/splinterd/src/integration_test_seams.rs,
# Plan 0061 D9). Such a build also honours test-only handoff seams, so no
# packaged daemon or client may contain it.
INTEGRATION_TEST_MARKER = b"splinterm-integration-test-salt:"
# Every daemon built with the TEST-ONLY `vm-acceptance-live-handoff` feature
# carries this marker (`VM_ACCEPTANCE_MARKER` in
# crates/splinterd/src/handoff_requests.rs, Plan 0061 M3.17). Such a daemon
# treats window continuity as ready and performs live handoff before the
# release enables it, so no release or Nix package may contain it. Only an
# explicitly requested local VM acceptance package
# (`--allow-vm-acceptance-live-handoff`, local-committed-head provenance)
# may, and then its daemon must.
VM_ACCEPTANCE_MARKER = b"splinterm-vm-acceptance-live-handoff:"
VM_ACCEPTANCE_PROVENANCE = "local-committed-head"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(64 * 1024):
            digest.update(block)
    return digest.hexdigest()


def contains_marker(path: Path, marker: bytes) -> bool:
    overlap = len(marker) - 1
    tail = b""
    with path.open("rb") as stream:
        while block := stream.read(64 * 1024):
            if marker in tail + block:
                return True
            tail = block[-overlap:]
    return False


def contains_integration_test_marker(path: Path) -> bool:
    return contains_marker(path, INTEGRATION_TEST_MARKER)


def contains_vm_acceptance_marker(path: Path) -> bool:
    return contains_marker(path, VM_ACCEPTANCE_MARKER)


def check_vm_acceptance(
    provenance: str, daemon: Path, client: Path, allow_vm_acceptance: bool
) -> bool:
    """Refuse a test-only VM acceptance build anywhere but an explicitly
    requested local package; return whether this is one."""
    marked = [
        binary for binary in (daemon, client) if contains_vm_acceptance_marker(binary)
    ]
    if marked and provenance != VM_ACCEPTANCE_PROVENANCE:
        raise ValueError(
            f"{marked[0]} was built with the test-only vm-acceptance-live-handoff "
            "feature; release and Nix packages must never enable it"
        )
    if marked and not allow_vm_acceptance:
        raise ValueError(
            f"{marked[0]} was built with the test-only vm-acceptance-live-handoff "
            "feature; only an explicitly requested local VM acceptance package "
            "(--allow-vm-acceptance-live-handoff) may contain it"
        )
    if allow_vm_acceptance and provenance != VM_ACCEPTANCE_PROVENANCE:
        raise ValueError(
            "a VM acceptance live-handoff package is test-only and must have "
            f"{VM_ACCEPTANCE_PROVENANCE} provenance"
        )
    if allow_vm_acceptance and daemon not in marked:
        raise ValueError(
            "a VM acceptance live-handoff package was requested, but the daemon "
            "was not built with the vm-acceptance-live-handoff feature"
        )
    return bool(marked)


def pair_identity(commit: str, provenance: str, version: str) -> str:
    if COMMIT.fullmatch(commit) is None or commit == "0" * 40:
        raise ValueError("source commit must be nonzero lowercase hexadecimal")
    if provenance not in KINDS:
        raise ValueError("package provenance kind is unsupported")
    if VERSION.fullmatch(version) is None:
        raise ValueError("build version is malformed")
    digest = hashlib.sha256()
    digest.update(b"splinterm-package-pair-v1\0")
    digest.update(commit.encode("ascii"))
    digest.update(b"\0")
    digest.update(provenance.encode("ascii"))
    digest.update(b"\0")
    digest.update(version.encode("ascii"))
    for value in (
        PRIVATE_PROTOCOL,
        PRIVATE_PROTOCOL,
        HANDOFF_PROTOCOL,
        HANDOFF_PROTOCOL,
        TERMINAL_CHECKPOINT,
        TERMINAL_CHECKPOINT,
        DESCRIPTOR_MANIFEST,
        DESCRIPTOR_MANIFEST,
    ):
        digest.update(value.to_bytes(2, "big"))
    return digest.hexdigest()


def generate(
    commit: str,
    provenance: str,
    version: str,
    daemon: Path,
    client: Path,
    allow_vm_acceptance: bool = False,
) -> dict[str, object]:
    if not daemon.is_file() or not client.is_file():
        raise ValueError("daemon and client must be regular files")
    for binary in (daemon, client):
        if contains_integration_test_marker(binary):
            raise ValueError(
                f"{binary} was built with the splinterd integration-test feature; "
                "packaged builds must never enable it"
            )
    check_vm_acceptance(provenance, daemon, client, allow_vm_acceptance)
    return {
        "architecture": "x86_64",
        "build_version": version,
        "client_sha256": sha256(client),
        "daemon_sha256": sha256(daemon),
        "package": "splinterm",
        "pair_build_identity": pair_identity(commit, provenance, version),
        "provenance": provenance,
        "schema": 1,
        "source_commit": commit,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--commit", required=True)
    result.add_argument("--provenance", required=True)
    result.add_argument("--version", required=True)
    result.add_argument("--daemon", required=True, type=Path)
    result.add_argument("--client", required=True, type=Path)
    result.add_argument("--output", required=True, type=Path)
    result.add_argument(
        "--allow-vm-acceptance-live-handoff",
        action="store_true",
        help=(
            "TEST-ONLY: this local package's daemon is a VM acceptance build "
            "(never for release or Nix packages)"
        ),
    )
    return result


def main() -> int:
    arguments = parser().parse_args()
    try:
        record = generate(
            arguments.commit,
            arguments.provenance,
            arguments.version,
            arguments.daemon,
            arguments.client,
            arguments.allow_vm_acceptance_live_handoff,
        )
    except (OSError, ValueError) as error:
        print(error, file=__import__("sys").stderr)
        return 1
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(
        json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    arguments.output.chmod(0o644)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
