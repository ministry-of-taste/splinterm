#!/usr/bin/env python3
"""Validate the checked-in automation and MCP public-contract fixtures."""

from __future__ import annotations

import hashlib
import json
import sys
from collections.abc import Iterator
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator
from jsonschema.exceptions import SchemaError
from referencing import Registry, Resource
from referencing.exceptions import Unresolvable

ROOT = Path(__file__).resolve().parents[2]
AUTOMATION_SCHEMA_DIR = ROOT / "dist" / "schemas" / "v2"
AUTOMATION_FIXTURE_DIR = ROOT / "tests" / "automation" / "fixtures"
MCP_SCHEMA_DIR = ROOT / "dist" / "schemas" / "mcp" / "v2"
MCP_FIXTURE_DIR = ROOT / "tests" / "mcp" / "fixtures"
MAX_JSON_BYTES = 1024 * 1024
EXPECTED_AUTOMATION_SCHEMAS = {
    "audit-record.schema.json",
    "cli-envelope.schema.json",
    "cli-event.schema.json",
    "policy.schema.json",
}
MCP_TOOLS = (
    "splinterm.ping",
    "splinterm.list_lairs",
    "splinterm.inspect_topology",
    "splinterm.inspect_splint",
    "splinterm.read_terminal",
    "splinterm.read_scrollback",
    "splinterm.search_scrollback",
    "splinterm.request_access",
    "splinterm.request_lair_access",
    "splinterm.authorization_status",
    "splinterm.revoke_access",
    "splinterm.inspect_audit",
    "splinterm.create_lair",
    "splinterm.split_splint",
    "splinterm.new_dojo",
    "splinterm.relaunch_splint",
    "splinterm.restore_splint",
    "splinterm.restore_dojo",
    "splinterm.restore_lair",
    "splinterm.close_splint",
    "splinterm.close_dojo",
    "splinterm.kill_splint",
    "splinterm.set_split_ratio",
    "splinterm.rename_lair",
    "splinterm.rename_dojo",
    "splinterm.rename_splint",
    "splinterm.set_dojo_default_focus",
    "splinterm.acquire_control",
    "splinterm.request_control_transfer",
    "splinterm.decide_control_transfer",
    "splinterm.release_control",
    "splinterm.input",
    "splinterm.resize",
    "splinterm.report_agent_status",
)
MCP_RESOURCES = ("topology", "terminal", "control")
MCP_ERROR_CODES = (
    "authentication_failed",
    "handshake_required",
    "incompatible_version",
    "invalid_request",
    "unsupported_schema",
    "consent_unavailable",
    "consent_denied",
    "unauthorized",
    "confirmation_required",
    "controller_unavailable",
    "control_transfer_unavailable",
    "stale_topology",
    "not_found",
    "stale_incarnation",
    "invalid_argument",
    "resource_limit",
    "cancelled",
    "timeout",
    "internal",
)
MCP_MUTATION_TOOLS = {
    "splinterm.request_access",
    "splinterm.request_lair_access",
    "splinterm.revoke_access",
    "splinterm.create_lair",
    "splinterm.split_splint",
    "splinterm.new_dojo",
    "splinterm.relaunch_splint",
    "splinterm.restore_splint",
    "splinterm.restore_dojo",
    "splinterm.restore_lair",
    "splinterm.close_splint",
    "splinterm.close_dojo",
    "splinterm.kill_splint",
    "splinterm.set_split_ratio",
    "splinterm.rename_lair",
    "splinterm.rename_dojo",
    "splinterm.rename_splint",
    "splinterm.set_dojo_default_focus",
    "splinterm.acquire_control",
    "splinterm.request_control_transfer",
    "splinterm.decide_control_transfer",
    "splinterm.release_control",
    "splinterm.input",
    "splinterm.resize",
    "splinterm.report_agent_status",
}
# ADR 0014: a workspace request that waits for the person is a successful
# result that committed nothing. Only a variant whose status is exactly
# "pending" may say committed=false; every other variant still commits.
MCP_PENDING_RESULT_TOOLS = {
    "splinterm.request_lair_access",
}
MCP_CONFIRMED_TOOLS = {
    "splinterm.revoke_access",
    "splinterm.close_splint",
    "splinterm.close_dojo",
    "splinterm.kill_splint",
}
MCP_UNTRUSTED_OUTPUT_TOOLS = {
    "splinterm.list_lairs",
    "splinterm.inspect_topology",
    "splinterm.inspect_splint",
    "splinterm.read_terminal",
    "splinterm.read_scrollback",
    "splinterm.search_scrollback",
}
MCP_TERMINAL_OUTPUT_TOOLS = {
    "splinterm.read_terminal",
    "splinterm.read_scrollback",
    "splinterm.search_scrollback",
}
MCP_OUTPUT_RESOURCE_DEFS = {
    "splinterm.ping": "daemon_resource",
    "splinterm.list_lairs": "topology_resource",
    "splinterm.inspect_topology": "topology_resource",
    "splinterm.inspect_splint": "logical_splint_resource",
    "splinterm.read_terminal": "terminal_resource",
    "splinterm.read_scrollback": "terminal_resource",
    "splinterm.search_scrollback": "terminal_resource",
    "splinterm.request_access": "authorization_resource",
    "splinterm.request_lair_access": ("lair_authorization_resource", "lair_resource"),
    "splinterm.authorization_status": "splint_resource",
    "splinterm.revoke_access": (
        "authorization_resource",
        "lair_authorization_resource",
    ),
    "splinterm.inspect_audit": "audit_resource",
    "splinterm.create_lair": "lair_resource",
    "splinterm.split_splint": "splint_resource",
    "splinterm.new_dojo": "dojo_resource",
    "splinterm.relaunch_splint": "splint_resource",
    "splinterm.restore_splint": "splint_resource",
    "splinterm.restore_dojo": "dojo_resource",
    "splinterm.restore_lair": "lair_resource",
    "splinterm.close_splint": "splint_resource",
    "splinterm.close_dojo": "dojo_resource",
    "splinterm.kill_splint": "splint_resource",
    "splinterm.set_split_ratio": "splint_resource",
    "splinterm.rename_lair": "lair_resource",
    "splinterm.rename_dojo": "dojo_resource",
    "splinterm.rename_splint": "splint_resource",
    "splinterm.set_dojo_default_focus": "dojo_resource",
    "splinterm.acquire_control": "control_resource",
    "splinterm.request_control_transfer": "control_resource",
    "splinterm.decide_control_transfer": "control_resource",
    "splinterm.release_control": "control_resource",
    "splinterm.input": "control_resource",
    "splinterm.resize": "control_resource",
    "splinterm.report_agent_status": "agent_status_resource",
}
MCP_RESOURCE_IDENTITY_DEFS = {
    "topology": "topology_resource",
    "terminal": "terminal_resource",
    "control": "control_resource",
}
MCP_FORBIDDEN_OUTPUT_PROPERTIES = {
    "argv",
    "body",
    "capability_token",
    "controller_id",
    "cwd",
    "daemon_id",
    "daemon_request_id",
    "environment",
    "full_argv",
    "input",
    "policy_body",
    "query",
    "raw_bytes",
    "request_id",
    "subscription_id",
    "terminal_bytes",
    "transfer_id",
}
EXPECTED_MCP_SCHEMAS = {
    "common.schema.json",
    "error.schema.json",
    *(f"resources/{resource}.schema.json" for resource in MCP_RESOURCES),
    *(
        f"tools/{tool.removeprefix('splinterm.')}.{direction}.schema.json"
        for tool in MCP_TOOLS
        for direction in ("input", "output")
    ),
}
EXPECTED_MCP_SCHEMA_SHA256 = {
    'common.schema.json': '381f0dbd8422fd5b1acefb568982b33bd6cf8bae06f7ced1cdb0c4136a8b7413',
    'error.schema.json': 'db5e68a6494912da6a114d055e8b5ae0426051f3f39ab9a5721d9b482e0307fe',
    'resources/control.schema.json': 'bf27457edc5eb95e7635ae4204ca462d319f1bdb64056f667a3409cf11bdcc34',
    'resources/terminal.schema.json': '54015be8b7cfa3880fb98e6c10448a93aa4750935b9f4f70ba9588bf6da302f8',
    'resources/topology.schema.json': '118e980740e399e391bdd0e52d49bc2a14eeb53c829035db485e828789ad3839',
    'tools/acquire_control.input.schema.json': 'bd216c2dfcd3986454d464e19a094f55ec55bafddc4357db58a7b08b02f11aff',
    'tools/acquire_control.output.schema.json': '5918e96c02ace8ccae4e93aa9c1659f7bb962784c17ec93e92907d00322e7e4f',
    'tools/authorization_status.input.schema.json': '7e55181e0b1e47d7b899625964398fa1e71e895219c8ca4527f9b58ae4da967f',
    'tools/authorization_status.output.schema.json': '34215f4f7905bbc557d7b4628709c79d95ba49e5c34e1e7f7a24756d10fd1262',
    'tools/close_dojo.input.schema.json': '7ef1e2abfa3b42b31ec78cba43e70c9dc948c7383b2f992a2ad6b51d71629e60',
    'tools/close_dojo.output.schema.json': 'a921e694a1c4f6cc7b2f16b7c1cd07fd1519e9c6b23fe042670c8e009662b9f0',
    'tools/close_splint.input.schema.json': 'd4bc7afbcfa9467e4bd6610d0f2198ab65372f8f2ee81ce72a347cff9648fe5a',
    'tools/close_splint.output.schema.json': '576a0e99b94579b8b36b3ae2db3aa7f1746adac4c6320b8e0088c0e4a9cfdc2e',
    'tools/create_lair.input.schema.json': 'a7e999e49e0774f821b5a8ebcf9b29029f217890032a0273083794800519b81c',
    'tools/create_lair.output.schema.json': '557707f92f7bfd92e8210d92c3fda0a957d48611748d2b78dde348b13376f81f',
    'tools/decide_control_transfer.input.schema.json': '5a7850fa934828ce9ef9cbbf06ae45f36b988526419ec680de463776a1a66b0f',
    'tools/decide_control_transfer.output.schema.json': 'da0732232dcdfb0db30de9a288a1afd8603ce46a40d493f33c8061b90616e6d1',
    'tools/input.input.schema.json': 'd6efb17cb06b9203dc438a93583af8540ed228879171ce839e6b9477802d0b94',
    'tools/input.output.schema.json': 'a05fb11845b1608909b103b94fb623300361edc4847fff5c8bea5a05a3ca21d1',
    'tools/inspect_audit.input.schema.json': 'c8d137b259cd0518e521326d3e56433883635de94743d6b9f2c428ff4b5e66bd',
    'tools/inspect_audit.output.schema.json': '61a80e47dd290aa22c3d4d29ee09c17db00191f4242c87630cc52fe6e61d2e8f',
    'tools/inspect_splint.input.schema.json': '902a3b772f508aa3f083f8530668c238fd3e5a8bd6716e464eb71e8017a10ed0',
    'tools/inspect_splint.output.schema.json': 'd60092876bda05e5371d827954d963201c690be60ae1991f38af50630156aa99',
    'tools/inspect_topology.input.schema.json': 'db3a7bd4e72d5975eaf9c19c41fc83d472f28f2df6a9775d56fa53d2207f92b5',
    'tools/inspect_topology.output.schema.json': 'dc7113be2e11d285818884cbf03d352dcb06606e6c618fec86887a680d11e580',
    'tools/kill_splint.input.schema.json': '125bef6e18229f26bc88c49b3ecf3201187ebcdf49c748fb17c6feca892a247f',
    'tools/kill_splint.output.schema.json': 'bb5213e42d26129c1e887b0b0f84da9813b5aff7a13744217ec7dc294573249c',
    'tools/list_lairs.input.schema.json': '47a14072a82ee7f9365e50bc293c408d6ea1f19652b5ec7aa6324092b26f0a88',
    'tools/list_lairs.output.schema.json': '5192bb0f845301d7e42b95af226b5a90be5c2851dd218132c9815ee919e13ba7',
    'tools/new_dojo.input.schema.json': '3680df87054940803a8346157a5f280c876515423057651b29c3e456be1241e9',
    'tools/new_dojo.output.schema.json': 'cb7ffc4f64cccbd790c720d3d44329217c660aec417a6e77e6e9ee35352e0b3b',
    'tools/ping.input.schema.json': '5a7b4c41ee38f501c742ae046bbf3ab28324153c405660014658ec793bb27f9c',
    'tools/ping.output.schema.json': '4635778ab43ce5e096c61606986af3380b56c88f8edcec0f414e596100e84d71',
    'tools/read_scrollback.input.schema.json': '59e71f0b8a8f01d82f4bf57dd79b2a7414b9d5bee385214162f88a5130e84391',
    'tools/read_scrollback.output.schema.json': '4e4e81777014dd236200ae2fe31c5213e7cdb1e2444fb58e9ac206d428fae511',
    'tools/read_terminal.input.schema.json': 'fa676197877e461f65af235d9950eda15838ae9259751daaa5a2323f06adc06b',
    'tools/read_terminal.output.schema.json': 'e279db0a0dcbf0f8e957d69bd9959080a8c431014641422d6591276c8b1c5544',
    'tools/relaunch_splint.input.schema.json': '017535302da3a14cbf143436b5480343fec67d7212ff1ed93e52485eba091430',
    'tools/relaunch_splint.output.schema.json': '7de79ba51a5f95bf018a215fb747d7c098d9cf0e798ca3c288abdd3af89b173d',
    'tools/release_control.input.schema.json': '60d9ce35f58e068a4de6a0a3631e319bfffb84296645735208bdc5877502a432',
    'tools/release_control.output.schema.json': '65c51e50cd88f110df2a132834e5d8de657189224a8a7cf2747b886803fb4d18',
    'tools/rename_dojo.input.schema.json': '2d10df7f0b07ed1eb8012d6af093f13362a27f50aebba08112d5cdc5f82c19b0',
    'tools/rename_dojo.output.schema.json': 'e1a457e7e79fcf20067a478a519ad7239c1de00d89d5fcee2b882d86409207fa',
    'tools/rename_lair.input.schema.json': 'e7899be586d32ecdfad2261b08051bf3f95ddda46e0ff62efbe60f05801462c6',
    'tools/rename_lair.output.schema.json': 'e3b427c306ac80808dc15b08053da509132b48caaa49d0754e4604f3e32af833',
    'tools/rename_splint.input.schema.json': 'be55780fd7d00722d4a01a2df1c879ca6b693658730cd5491226a9fa4c7c8a39',
    'tools/rename_splint.output.schema.json': '456d8be17a96531b3ecf42d804395539418e11c830bfe5156f74a630a3437237',
    'tools/report_agent_status.input.schema.json': '6f5610e21ee0eff5193bdef4aa30e705852f5c4e8ee0a15acc509155ba30d4e5',
    'tools/report_agent_status.output.schema.json': '519c75e937b7b9e7b65a32ae0d7b5186baa681ac6742186cd9445936f21d5c49',
    'tools/request_access.input.schema.json': '97aaf9b747784f6274a40b7de3f00f8a85bcf905719f91ceb2701993d8de5041',
    'tools/request_access.output.schema.json': 'cda84ab8391295f4c13c8ab0d9f25e9388df5f52f84992d6d46c56400facec1c',
    'tools/request_lair_access.input.schema.json': '5bd105c65f842d670d651beeef505e6890d8290432f63a11c083ab036cdaa376',
    'tools/request_lair_access.output.schema.json': '25fc6f69261cf81341240bbf5770a5047f05d42d4a2eca09d01bedf8221bf08d',
    'tools/request_control_transfer.input.schema.json': '02d45ae645149890df20a4ab68a3216696db2bc2c61c569febc9ea9635ff582f',
    'tools/request_control_transfer.output.schema.json': 'a0f46afac3df5d2f0ebed0266e1ecfb145fdc685d018bffc2a08c3c0c99d1360',
    'tools/resize.input.schema.json': '6c3bfa8444814d73957345ad829343f062c8a8150c8c027a023e2362a1417cad',
    'tools/resize.output.schema.json': '2a5d90a468047ee3bfedc7f94f27881f50783f032cd2637e09c251462cc404f4',
    'tools/restore_dojo.input.schema.json': '72cbacd6d2442566ff33db0819f1d9b9e1bcddef33fe5f3d169237b07f5fb1bc',
    'tools/restore_dojo.output.schema.json': 'b5bc5da3cf8def494028b862ea17d963a52f8a335d8bd8dfbd982ebee5764e80',
    'tools/restore_lair.input.schema.json': 'b47d9b107fbd9ce1d430415d39752718b43c3d84541b1dcb35b007ffebef38b5',
    'tools/restore_lair.output.schema.json': '74be50cf48d11925c926f8926af723bdb258729df0b22ac5a15683d942f51657',
    'tools/restore_splint.input.schema.json': '76e5784d61bf083030b0b1a89143b4a79dfd8dcbf323c9dd60a52139bb347c57',
    'tools/restore_splint.output.schema.json': '1123357d4b581fda43cdce190f9197c0459cfe3b7a01bce01e1cc4bd8d36fca3',
    'tools/revoke_access.input.schema.json': '8dcecdd55e6b6919116c7de6e161880185e273c3eec26cdbe459ce9d0cd11792',
    'tools/revoke_access.output.schema.json': '9762aec9ecf592bee8bbb338f229ba46fca15a40eb7c295d760d28fdc7fbbe14',
    'tools/search_scrollback.input.schema.json': 'c017396b6cee51a5dd912880433a3160930e689ba3b729b4dfcdec8e21c188f1',
    'tools/search_scrollback.output.schema.json': '3bf75ea45fab25129c8b73162df81f09afaf80f43712a6a1a748308d01fe1643',
    'tools/set_dojo_default_focus.input.schema.json': 'bbf4448205eab1d34421ab9a80896104ee7780d4cd7e12daa0f4e22d64dc0d9e',
    'tools/set_dojo_default_focus.output.schema.json': '53c451b6fc762119283ca1baad7eb68e7ee35370e180941eb46b7b15e41541c4',
    'tools/set_split_ratio.input.schema.json': 'fc5d00996678fda87640036f94202a8b6953b656069caba79d19584387f17326',
    'tools/set_split_ratio.output.schema.json': '06563f468beaae5f723442cf0903be004af0c702b3b4f2013ea71ee4ad38a65a',
    'tools/split_splint.input.schema.json': 'd4310853bc73451885d148644b2126a461a1658ac5b91a823aa3377d4785e4bc',
    'tools/split_splint.output.schema.json': 'e3febd6e3d5a2e8e83b920da48f682202d2afe145e5655d9fb8f056f218dc85b',
}
EXPECTED_MCP_VALID_FIXTURES = {
    *(f"{tool.removeprefix('splinterm.')}.input.json" for tool in MCP_TOOLS),
    *(f"{tool.removeprefix('splinterm.')}.output.json" for tool in MCP_TOOLS),
    *(f"resource-{resource}.json" for resource in MCP_RESOURCES),
    *(f"error-{error_code.replace('_', '-')}.json" for error_code in MCP_ERROR_CODES),
    # Additive optional output fields keep the reviewed per-tool fixtures intact.
    "inspect_splint-agent-status.output.json",
    "inspect_topology-agent-status.output.json",
    # Plan 0069: a foreground app name, and a rename undone with use_app_name.
    "inspect_splint-foreground-app.output.json",
    "inspect_topology-foreground-app.output.json",
    "rename_splint-use-app-name.input.json",
    "report_agent_status-clear.output.json",
    # A first clear for an incarnation is a no-op the daemon numbers 0.
    "report_agent_status-first-clear.output.json",
    # ADR 0014: a level, a cancel, a pending answer, a cancelled request, and
    # a remembered grant.
    "request_lair_access-level.input.json",
    "request_lair_access-cancel.input.json",
    "request_lair_access-pending.output.json",
    "request_lair_access-cancelled.output.json",
    "request_lair_access-remembered.output.json",
    # Plan 0067 M2: terminal reads default to plain text; the reviewed
    # per-tool fixtures keep the per-cell form.
    "read_terminal-text.output.json",
    "read_scrollback-text.output.json",
}
EXPECTED_MCP_INVALID_FIXTURES = {
    "argv-echo.json",
    "body-echo.json",
    "duplicate-provenance-mismatch.json",
    "excessive-audit-limit.json",
    "excessive-row-limit.json",
    "false-confirmation.json",
    "false-terminal-trust.json",
    "hostile-title-false-trust.json",
    "inspect_splint-hostile-foreground-app.output.json",
    "input-echo.json",
    "malformed-controller-handle.json",
    "malformed-cursor.json",
    "malformed-transfer-handle.json",
    "malformed-uuid.json",
    "missing-confirmation.json",
    "missing-provenance.json",
    "missing-request-access-incarnation.json",
    "noncanonical-grant-id.json",
    "noncanonical-uuid.json",
    "open-output-object.json",
    "oversized-error-text.json",
    "oversized-input-text.json",
    "private-daemon-id.json",
    "query-echo.json",
    "raw-bytes-leak.json",
    "rename_splint-title-and-use-app-name.input.json",
    "report_agent_status-control-detail.input.json",
    "report_agent_status-detail-echo.json",
    "report_agent_status-unknown-state.input.json",
    "request_lair_access-pending-committed.output.json",
    "request_lair_access-unknown-level.input.json",
    "request_lair_access-unsupported-scope.input.json",
    "resource-open-metadata.json",
    "resource-private-controller-id.json",
    "uncommitted-mutation-success.json",
    "unknown-input-field.json",
    "uppercase-error-code.json",
    "wrong-resource-kind.json",
}


class ContractError(ValueError):
    """A checked-in contract artifact is malformed or has the wrong expectation."""


def load_json(path: Path) -> Any:
    size = path.stat().st_size
    if size == 0 or size > MAX_JSON_BYTES:
        raise ContractError(f"{path}: JSON size {size} is outside 1..{MAX_JSON_BYTES}")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ContractError(f"{path}: cannot read JSON: {error}") from error


def assert_inventory(label: str, actual: set[str], expected: set[str]) -> None:
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        raise ContractError(f"{label} inventory mismatch; missing={missing}, extra={extra}")


def load_schema_documents(
    schema_dir: Path, expected: set[str]
) -> dict[str, dict[str, Any]]:
    actual = {
        path.relative_to(schema_dir).as_posix()
        for path in schema_dir.rglob("*.schema.json")
    }
    assert_inventory(f"{schema_dir.relative_to(ROOT)} schema", actual, expected)

    schemas: dict[str, dict[str, Any]] = {}
    for name in sorted(actual):
        schema = load_json(schema_dir / name)
        if not isinstance(schema, dict):
            raise ContractError(f"{name}: schema document must be an object")
        expected_id = (
            "https://splinterm.oldjobobo.com/schemas/"
            f"{schema_dir.relative_to(ROOT / 'dist' / 'schemas').as_posix()}/{name}"
        )
        if schema.get("$schema") != "https://json-schema.org/draft/2020-12/schema":
            raise ContractError(f"{name}: $schema must select canonical Draft 2020-12")
        if schema.get("$id") != expected_id:
            raise ContractError(
                f"{name}: $id must be path-derived canonical ID {expected_id!r}"
            )
        try:
            Draft202012Validator.check_schema(schema)
        except SchemaError as error:
            raise ContractError(
                f"{name}: invalid Draft 2020-12 schema: {error.message}"
            ) from error
        schemas[name] = schema
    return schemas


def build_validators(
    schemas: dict[str, dict[str, Any]],
) -> dict[str, Draft202012Validator]:
    resources = []
    schema_ids: set[str] = set()
    for name, schema in schemas.items():
        schema_id = schema.get("$id")
        if not isinstance(schema_id, str):
            raise ContractError(f"{name}: schema must declare a string $id")
        if schema_id in schema_ids:
            raise ContractError(f"{name}: duplicate schema $id {schema_id!r}")
        schema_ids.add(schema_id)
        resources.append((schema_id, Resource.from_contents(schema)))
    registry = Registry().with_resources(resources)
    return {
        name: Draft202012Validator(schema, registry=registry)
        for name, schema in schemas.items()
    }


def iter_schema_nodes(
    value: Any, path: tuple[str | int, ...] = ()
) -> Iterator[tuple[tuple[str | int, ...], dict[str, Any]]]:
    """Yield every schema node and its JSON path."""
    if isinstance(value, dict):
        yield path, value
        for key, child in value.items():
            yield from iter_schema_nodes(child, (*path, key))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from iter_schema_nodes(child, (*path, index))


def require_schema_condition(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def validate_mcp_schema_contract(schemas: dict[str, dict[str, Any]]) -> None:
    """Audit security-sensitive invariants across the complete MCP inventory."""
    assert_inventory(
        "MCP reviewed schema hash",
        set(schemas),
        set(EXPECTED_MCP_SCHEMA_SHA256),
    )
    for name, schema in schemas.items():
        canonical = json.dumps(
            schema, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode("utf-8")
        actual_hash = hashlib.sha256(canonical).hexdigest()
        expected_hash = EXPECTED_MCP_SCHEMA_SHA256[name]
        if actual_hash != expected_hash:
            raise ContractError(
                f"{name}: reviewed structural hash mismatch; "
                f"expected={expected_hash}, actual={actual_hash}"
            )

    resources = [
        (schema["$id"], Resource.from_contents(schema)) for schema in schemas.values()
    ]
    registry = Registry().with_resources(resources)

    for name, schema in schemas.items():
        resolver = registry.resolver(base_uri=schema["$id"])
        for path, node in iter_schema_nodes(schema):
            location = "/".join(str(part) for part in path) or "<root>"
            if node.get("type") == "object":
                require_schema_condition(
                    node.get("additionalProperties") is False,
                    f"{name}:{location}: object schema must be closed",
                )
            reference = node.get("$ref")
            if isinstance(reference, str):
                try:
                    resolver.lookup(reference)
                except Unresolvable as error:
                    raise ContractError(
                        f"{name}:{location}: unresolved $ref {reference!r}: {error}"
                    ) from error

            if name.startswith(("tools/", "resources/")):
                forbidden = MCP_FORBIDDEN_OUTPUT_PROPERTIES.intersection(
                    node.get("properties", {})
                )
                if name.endswith(".output.schema.json") or name.startswith(
                    "resources/"
                ):
                    require_schema_condition(
                        not forbidden,
                        f"{name}:{location}: forbidden public output fields "
                        f"{sorted(forbidden)}",
                    )

    common_defs = schemas["common.schema.json"]["$defs"]
    require_schema_condition(
        tuple(common_defs["tool_name"]["enum"]) == MCP_TOOLS,
        "common.schema.json: tool_name inventory or order is not exact",
    )
    require_schema_condition(
        tuple(common_defs["error_code"]["enum"]) == MCP_ERROR_CODES,
        "common.schema.json: error_code inventory or order is not exact",
    )

    for tool in MCP_TOOLS:
        stem = tool.removeprefix("splinterm.")
        input_schema = schemas[f"tools/{stem}.input.schema.json"]["allOf"][0]
        output_overlay = schemas[f"tools/{stem}.output.schema.json"]["allOf"][1]
        output_properties = output_overlay["properties"]
        data_schema = output_properties["data"]
        if "$ref" in data_schema:
            data_schema = resolver.lookup(data_schema["$ref"]).contents
        data_schemas = data_schema.get("oneOf", [data_schema])

        require_schema_condition(
            input_schema.get("type") == "object"
            and input_schema.get("additionalProperties") is False,
            f"{tool}: input root must be a closed object",
        )
        require_schema_condition(
            output_properties["tool"].get("const") == tool,
            f"{tool}: output tool discriminator is not exact",
        )
        require_schema_condition(
            bool(data_schemas)
            and all(
                schema.get("type") == "object"
                and schema.get("additionalProperties") is False
                for schema in data_schemas
            ),
            f"{tool}: output data must contain only closed objects",
        )

        expected_trust = (
            "untrusted_terminal_data"
            if tool in MCP_UNTRUSTED_OUTPUT_TOOLS
            else "trusted_metadata"
        )
        require_schema_condition(
            output_properties["content_trust"].get("const") == expected_trust,
            f"{tool}: output trust label is not exact",
        )
        expected_resource_defs = MCP_OUTPUT_RESOURCE_DEFS[tool]
        if isinstance(expected_resource_defs, str):
            expected_resource_defs = (expected_resource_defs,)
        expected_resource_refs = {
            "https://splinterm.oldjobobo.com/schemas/mcp/v2/common.schema.json#/$defs/"
            f"{definition}"
            for definition in expected_resource_defs
        }
        resource_schema = output_properties.get("resource", {})
        actual_resource_refs = (
            {resource_schema["$ref"]}
            if "$ref" in resource_schema
            else {choice.get("$ref") for choice in resource_schema.get("oneOf", [])}
        )
        require_schema_condition(
            actual_resource_refs == expected_resource_refs,
            f"{tool}: output resource identity is not exact",
        )
        if tool in MCP_TERMINAL_OUTPUT_TOOLS:
            require_schema_condition(
                all(
                    "provenance" not in schema.get("properties", {})
                    for schema in data_schemas
                )
                and MCP_OUTPUT_RESOURCE_DEFS[tool] == "terminal_resource",
                f"{tool}: provenance must be carried once by terminal resource identity",
            )
        if tool in MCP_MUTATION_TOOLS:

            def commits(schema: dict[str, Any]) -> bool:
                committed = schema["properties"]["committed"].get("const")
                if committed is True:
                    return True
                return (
                    tool in MCP_PENDING_RESULT_TOOLS
                    and committed is False
                    and "status" in schema.get("required", [])
                    and schema["properties"]["status"].get("const") == "pending"
                )

            require_schema_condition(
                all(
                    "committed" in schema.get("required", []) and commits(schema)
                    for schema in data_schemas
                ),
                f"{tool}: successful mutation must require committed=true",
            )
        if tool in MCP_CONFIRMED_TOOLS:
            require_schema_condition(
                "confirm" in input_schema.get("required", [])
                and input_schema["properties"]["confirm"].get("const") is True,
                f"{tool}: destructive input must require confirm=true",
            )

    identity_properties = {
        "daemon_resource": {"kind"},
        "topology_resource": {"kind", "topology_revision"},
        "lair_resource": {"kind", "lair_id", "topology_revision"},
        "dojo_resource": {"kind", "lair_id", "dojo_id", "topology_revision"},
        "splint_resource": {
            "kind", "lair_id", "dojo_id", "splint_id", "incarnation",
            "topology_revision",
        },
        "logical_splint_resource": {
            "kind", "lair_id", "dojo_id", "splint_id", "current_incarnation",
            "last_incarnation", "topology_revision",
        },
        "terminal_resource": {
            "kind", "lair_id", "dojo_id", "splint_id", "incarnation",
            "topology_revision", "terminal_revision", "history_generation",
        },
        "authorization_resource": {
            "kind", "lair_id", "dojo_id", "splint_id", "incarnation",
            "grant_id", "authorization_revision",
        },
        "lair_authorization_resource": {
            "kind", "lair_id", "topology_revision", "authorization_revision",
        },
        "audit_resource": {"kind"},
        "control_resource": {
            "kind", "lair_id", "dojo_id", "splint_id", "incarnation",
            "control_revision",
        },
        "agent_status_resource": {"kind", "splint_id", "incarnation"},
    }
    for definition, expected_properties in identity_properties.items():
        identity = common_defs[definition]
        require_schema_condition(
            identity.get("additionalProperties") is False
            and set(identity.get("properties", {})) == expected_properties
            and set(identity.get("required", [])) == expected_properties,
            f"common.schema.json: {definition} identity inventory is not exact",
        )

    require_schema_condition(
        common_defs["grant_id"]
        == {"type": "string", "pattern": "^[1-9][0-9]{0,19}$"},
        "common.schema.json: grant_id must be a canonical public nonzero decimal string",
    )
    topology_shapes = {
        "topology_data": {"lairs"},
        "topology_lair": {"lair_id", "name", "dojos"},
        "topology_dojo": {"dojo_id", "name", "default_focus_splint_id", "splints"},
        "topology_splint": {
            "splint_id", "current_incarnation", "last_incarnation", "title", "state",
        },
    }
    # Additive optional fields: present only when the daemon holds a value.
    topology_optional = {"topology_splint": {"agent_status", "foreground_app"}}
    for definition, expected_properties in topology_shapes.items():
        topology = common_defs[definition]
        optional = topology_optional.get(definition, set())
        require_schema_condition(
            topology.get("additionalProperties") is False
            and set(topology.get("properties", {})) == expected_properties | optional
            and set(topology.get("required", [])) == expected_properties,
            f"common.schema.json: {definition} full topology DTO is not exact",
        )

    for resource_name, identity_def in MCP_RESOURCE_IDENTITY_DEFS.items():
        resource = schemas[f"resources/{resource_name}.schema.json"]["allOf"][0]
        expected_ref = (
            "https://splinterm.oldjobobo.com/schemas/mcp/v2/common.schema.json#/$defs/"
            f"{identity_def}"
        )
        require_schema_condition(
            resource["properties"].get("resource", {}).get("$ref") == expected_ref
            and "resource" in resource.get("required", []),
            f"{resource_name} resource contents must require exact resource identity",
        )

    terminal_resource = schemas["resources/terminal.schema.json"]["allOf"][0]
    require_schema_condition(
        terminal_resource["properties"]["content_trust"].get("const")
        == "untrusted_terminal_data"
        and "provenance" not in terminal_resource.get("properties", {}),
        "terminal resource must use untrusted trust and single resource provenance",
    )
    topology_resource = schemas["resources/topology.schema.json"]["allOf"][0]
    require_schema_condition(
        topology_resource["properties"]["content_trust"].get("const")
        == "untrusted_terminal_data"
        and topology_resource["properties"]["data"].get("$ref", "").endswith(
            "#/$defs/topology_data"
        ),
        "topology resource must carry bounded untrusted full topology data",
    )

    for name, schema in schemas.items():
        if not (name.endswith(".output.schema.json") or name.startswith("resources/")):
            continue
        property_names = {
            property_name
            for _, node in iter_schema_nodes(schema)
            for property_name in node.get("properties", {})
        }
        if property_names.intersection({"title", "name"}):
            root = schema["allOf"][0] if name.startswith("resources/") else schema["allOf"][1]
            require_schema_condition(
                root["properties"]["content_trust"].get("const")
                == "untrusted_terminal_data",
                f"{name}: attacker-controlled title/name output must be untrusted",
            )


def validate_automation_fixture(
    path: Path,
    validators: dict[str, Draft202012Validator],
    *,
    should_pass: bool,
) -> None:
    fixture = load_json(path)
    if not isinstance(fixture, dict) or set(fixture) != {"$schema_file", "document"}:
        raise ContractError(f"{path}: fixture must contain only $schema_file and document")
    schema_file = fixture["$schema_file"]
    if schema_file not in validators:
        raise ContractError(f"{path}: unknown schema {schema_file!r}")

    errors = sorted(
        validators[schema_file].iter_errors(fixture["document"]),
        key=lambda error: tuple(str(part) for part in error.absolute_path),
    )
    if should_pass and errors:
        raise ContractError(f"{path}: expected valid document: {errors[0].message}")
    if not should_pass and not errors:
        raise ContractError(f"{path}: invalid fixture unexpectedly passed")


def validate_mcp_fixture(
    path: Path,
    validators: dict[str, Draft202012Validator],
    *,
    should_pass: bool,
) -> None:
    fixture = load_json(path)
    expected_keys = {"$schema_file", "document"}
    if not should_pass:
        expected_keys.add("$expected_keyword")
    if not isinstance(fixture, dict) or set(fixture) != expected_keys:
        raise ContractError(f"{path}: fixture keys must be exactly {sorted(expected_keys)}")
    schema_file = fixture["$schema_file"]
    if schema_file not in validators:
        raise ContractError(f"{path}: unknown schema {schema_file!r}")

    errors = list(validators[schema_file].iter_errors(fixture["document"]))
    if should_pass and errors:
        raise ContractError(f"{path}: expected valid document: {errors[0].message}")
    if not should_pass:
        if not errors:
            raise ContractError(f"{path}: invalid fixture unexpectedly passed")
        expected_keyword = fixture["$expected_keyword"]
        actual = sorted({str(error.validator) for error in errors})
        if not isinstance(expected_keyword, str) or actual != [expected_keyword]:
            raise ContractError(
                f"{path}: expected only {expected_keyword!r} failures; "
                f"actual keywords={actual}"
            )


def validate_automation_contract() -> tuple[int, int, int]:
    schemas = load_schema_documents(
        AUTOMATION_SCHEMA_DIR, EXPECTED_AUTOMATION_SCHEMAS
    )
    validators = build_validators(schemas)
    valid = sorted((AUTOMATION_FIXTURE_DIR / "valid").glob("*.json"))
    invalid = sorted((AUTOMATION_FIXTURE_DIR / "invalid").glob("*.json"))
    if not valid or not invalid:
        raise ContractError("both automation fixture sets must be non-empty")
    for path in valid:
        validate_automation_fixture(path, validators, should_pass=True)
    for path in invalid:
        validate_automation_fixture(path, validators, should_pass=False)
    return len(valid), len(invalid), len(validators)


def validate_mcp_contract() -> tuple[int, int, int]:
    schemas = load_schema_documents(MCP_SCHEMA_DIR, EXPECTED_MCP_SCHEMAS)
    validate_mcp_schema_contract(schemas)
    validators = build_validators(schemas)
    valid_paths = sorted((MCP_FIXTURE_DIR / "valid").glob("*.json"))
    invalid_paths = sorted((MCP_FIXTURE_DIR / "invalid").glob("*.json"))
    assert_inventory(
        "MCP valid fixture", {path.name for path in valid_paths}, EXPECTED_MCP_VALID_FIXTURES
    )
    assert_inventory(
        "MCP invalid fixture",
        {path.name for path in invalid_paths},
        EXPECTED_MCP_INVALID_FIXTURES,
    )
    for path in valid_paths:
        validate_mcp_fixture(path, validators, should_pass=True)
    for path in invalid_paths:
        validate_mcp_fixture(path, validators, should_pass=False)
    return len(valid_paths), len(invalid_paths), len(validators)


def main() -> int:
    try:
        automation = validate_automation_contract()
        mcp = validate_mcp_contract()
    except (ContractError, OSError) as error:
        print(f"contract validation failed: {error}", file=sys.stderr)
        return 1

    print(
        f"Validated {automation[0]} valid and {automation[1]} invalid automation "
        f"fixtures against {automation[2]} schemas."
    )
    print(
        f"Validated {mcp[0]} valid and {mcp[1]} invalid MCP fixtures against "
        f"{mcp[2]} schemas ({len(MCP_TOOLS)} tools, {len(MCP_RESOURCES)} resources, "
        f"{len(MCP_ERROR_CODES)} error codes)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
