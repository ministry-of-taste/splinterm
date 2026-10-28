from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
PINNED_FOOT_WORKFLOW = ROOT / ".github/workflows/foot-oracle-pinned.yml"
MANDATORY = (
    "preflight",
    "changes",
    "static",
    "workspace-tests",
    "daemon-tests",
    "mcp-tests",
    "package-automation",
    "renderer-contracts",
    "nix",
    "arch-warning-check",
)
# The only skip check accepts: a pull request whose changes job selected
# exactly nix=false. Pushes and merge groups always require nix success.
NIX_SKIP_ALLOWED = (
    "(github.event_name == 'pull_request' && needs.changes.outputs.nix == 'false'"
    " && needs.nix.result == 'skipped')"
)


def failed_condition(job: str) -> str:
    if job == "nix":
        return f"(needs.nix.result != 'success' && !{NIX_SKIP_ALLOWED})"
    return f"needs.{job}.result != 'success'"


def succeeded_condition(job: str) -> str:
    if job == "nix":
        return f"(needs.nix.result == 'success' || {NIX_SKIP_ALLOWED})"
    return f"needs.{job}.result == 'success'"


class CiWorkflowTests(unittest.TestCase):
    def test_required_check_is_always_run_and_fail_closed(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        check = workflow[workflow.index("  check:") :]
        self.assertIn("if: ${{ always() }}", check)
        for job in MANDATORY:
            self.assertIn(f"- {job}", check)
        failed = " || ".join(failed_condition(job) for job in MANDATORY)
        succeeded = " && ".join(succeeded_condition(job) for job in MANDATORY)
        self.assertIn(f"if: ${{{{ {failed} }}}}", check)
        self.assertIn(f"if: ${{{{ {succeeded} }}}}", check)
        self.assertEqual(check.count("needs.nix.result == 'skipped'"), 2)

    def test_aggregator_model_rejects_failed_cancelled_and_skipped_dependencies(self) -> None:
        def succeeds(results: dict[str, str], event: str, nix_output: str) -> bool:
            nix_skip_allowed = (
                event == "pull_request"
                and nix_output == "false"
                and results["nix"] == "skipped"
            )
            return all(
                results[job] == "success" or (job == "nix" and nix_skip_allowed)
                for job in MANDATORY
            )

        successful = {job: "success" for job in MANDATORY}
        for event in ("push", "pull_request", "merge_group"):
            self.assertTrue(succeeds(successful, event, "true"))
        for job in ("daemon-tests", "changes", "nix"):
            for result in ("failure", "cancelled", "skipped"):
                for event in ("push", "merge_group"):
                    with self.subTest(job=job, result=result, event=event):
                        changed = dict(successful, **{job: result})
                        self.assertFalse(succeeds(changed, event, "false"))
        skipped_nix = dict(successful, nix="skipped")
        self.assertTrue(succeeds(skipped_nix, "pull_request", "false"))
        for nix_output in ("true", ""):
            self.assertFalse(succeeds(skipped_nix, "pull_request", nix_output))
        for result in ("failure", "cancelled"):
            self.assertFalse(
                succeeds(dict(successful, nix=result), "pull_request", "false")
            )

    def test_merge_queue_and_superseded_pull_requests(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        triggers = workflow.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertIn("  merge_group:\n    types: [checks_requested]\n", triggers)
        self.assertIn("  pull_request:\n", triggers)
        self.assertIn('    branches: [main, "maint/0.1"]\n', triggers)
        concurrency = workflow.split("\nconcurrency:\n", 1)[1].split("\n\n", 1)[0]
        # Only pull requests share a per-ref group; pushes and merge groups get
        # a unique per-run group so no run is cancelled or replaced while pending.
        self.assertIn(
            "group: ci-${{ github.workflow }}-${{ github.event_name }}-"
            "${{ github.event_name == 'pull_request' && github.ref || github.run_id }}",
            concurrency,
        )
        self.assertIn(
            "cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
            concurrency,
        )
        # No job may opt out of merge groups or pushes by event name except the
        # path-gated nix job and the pull-request-only checkout in changes.
        self.assertEqual(workflow.count("github.event_name != 'pull_request'"), 1)
        self.assertNotIn("github.event_name == 'merge_group'", workflow)
        # The only push-specific expressions keep the Cachix write token and
        # upload on authority-branch pushes; no job is skipped by them.
        self.assertEqual(workflow.count("github.event_name == 'push'"), 1)
        self.assertEqual(workflow.count("github.event_name != 'push'"), 1)
        self.assertIn(
            "authToken: ${{ github.event_name == 'push' && secrets.CACHIX_AUTH_TOKEN || '' }}",
            workflow,
        )

    def test_changes_job_selects_nix_from_the_pull_request_range(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        block = workflow[workflow.index("  changes:\n") : workflow.index("  static:\n")]
        self.assertIn("nix: ${{ steps.select.outputs.nix }}", block)
        self.assertIn("fetch-depth: 0", block)
        self.assertIn("BASE_SHA: ${{ github.event.pull_request.base.sha }}", block)
        self.assertIn("HEAD_SHA: ${{ github.event.pull_request.head.sha }}", block)
        self.assertIn("set -euo pipefail", block)
        self.assertIn(
            'git diff --no-renames --name-only "$BASE_SHA...$HEAD_SHA"', block
        )
        self.assertNotIn("uses: dorny", block)
        self.assertNotIn("continue-on-error", block)
        self.assertEqual(
            [line.strip() for line in block.splitlines() if "uses:" in line],
            ["- uses: actions/checkout@v5"],
        )
        # Non-pull-request events select nix before any diff is attempted.
        self.assertLess(
            block.index('if [ "$EVENT_NAME" != pull_request ]; then'),
            block.index("git diff"),
        )
        # A pull request with a missing revision (an empty fork payload)
        # selects nix rather than diffing an empty range into nix=false.
        missing = block.index('if [ -z "$BASE_SHA" ] || [ -z "$HEAD_SHA" ]; then')
        self.assertLess(missing, block.index("git diff"))
        self.assertLess(
            missing, block.index("echo 'nix=true'", missing), block.index("git diff")
        )
        pattern = re.search(r"grep -Eq '([^']+)' changed-paths.txt", block).group(1)
        selected = re.compile(pattern)
        for path in (
            "nix/package.nix",
            "flake.nix",
            "flake.lock",
            "packaging/PKGBUILD",
            "dist/systemd/splinterd.service",
            "Cargo.lock",
            "Cargo.toml",
            "crates/splinterd/Cargo.toml",
            "rust-toolchain.toml",
            ".github/workflows/ci.yml",
            # Inputs nix/package.nix installs, patches, or runs.
            "config/splinterm/presets.toml",
            "tools/automation/splinterm-dojo-picker.py",
            "tools/package/generate-package-provenance.py",
            # nix/package.nix patches these sources with substituteInPlace.
            "crates/splinterm/src/app/local_service.rs",
            "crates/splinterm/src/remote.rs",
            "crates/splinterm/src/renderer/fonts.rs",
            "crates/splinterm/src/wayland.rs",
        ):
            with self.subTest(path=path):
                self.assertIsNotNone(selected.search(path))
        for path in (
            "crates/splinterd/src/main.rs",
            "crates/splinterm/src/wayland/tabs.rs",
            "crates/splinterm/src/remote.rs.orig",
            "docs/nixos.md",
            "tools/release/test_ci_workflow.py",
            ".github/workflows/site.yml",
            "xnix/file",
            "Cargo.toml.orig",
        ):
            with self.subTest(path=path):
                self.assertIsNone(selected.search(path))

    def test_each_frozen_rust_job_fetches_the_locked_graph_first(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        jobs = ("static", "workspace-tests", "daemon-tests", "mcp-tests")
        boundaries = (*jobs, "package-automation")
        self.assertEqual(workflow.count("run: cargo fetch --locked"), len(jobs))
        for job, following in zip(jobs, boundaries[1:]):
            block = workflow[
                workflow.index(f"  {job}:") : workflow.index(f"  {following}:")
            ]
            self.assertLess(
                block.index("run: cargo fetch --locked"),
                block.index("cargo ", block.index("run: cargo fetch --locked") + 1),
            )
            self.assertIn("--frozen", block)

    def test_daemon_test_jobs_build_the_required_pty_helper_first(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        helper = "cargo build --frozen -p splinterm-pty --bin splinterm-pty-child"
        self.assertEqual(workflow.count(helper), 2)
        for job, following, test_step in (
            ("workspace-tests", "daemon-tests", "Run workspace unit"),
            ("daemon-tests", "mcp-tests", "Run daemon integration target"),
        ):
            block = workflow[
                workflow.index(f"  {job}:") : workflow.index(f"  {following}:")
            ]
            self.assertLess(block.index(helper), block.index(test_step))

    def test_every_rust_integration_target_is_named_exactly_once(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        targets = []
        for path in (ROOT / "crates").glob("*/tests/*.rs"):
            if "data" not in path.parts:
                targets.append(path.stem)
        self.assertGreater(len(targets), 10)
        for target in sorted(targets):
            with self.subTest(target=target):
                occurrences = re.findall(
                    rf"--test\s+{re.escape(target)}(?=\s|\\|$)", workflow
                )
                self.assertEqual(len(occurrences), 1)
        self.assertIn(
            "cargo test --frozen --workspace --lib --bins --examples", workflow
        )
        self.assertIn("cargo test --frozen --workspace --doc", workflow)

    def test_pkgbuild_check_python_commands_have_exact_ci_equivalents(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        pkgbuild = (ROOT / "packaging/PKGBUILD").read_text(encoding="utf-8")
        check_body = pkgbuild.split("check() (", 1)[1].split("\n)", 1)[0]
        python_targets = re.findall(r"python -m unittest ([^\s]+)", check_body)
        self.assertEqual(len(python_targets), 9)
        for target in python_targets:
            with self.subTest(target=target):
                self.assertEqual(workflow.count(target), 1)
        self.assertIn("cargo test --frozen --workspace", check_body)
        self.assertTrue(
            all("--frozen" in line for line in workflow.splitlines() if "cargo test " in line)
        )
        self.assertTrue(
            all(
                "--test-threads=1" in line or line.rstrip().endswith("\\")
                for line in workflow.splitlines()
                if "cargo test " in line
            )
        )

    def test_agent_runner_contracts_are_mandatory_and_retain_failure_evidence(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        block = workflow[
            workflow.index("  package-automation:") : workflow.index("  renderer-contracts:")
        ]
        step = block.split(
            "      - name: Run agent runner contracts without remote services\n", 1
        )[1].split("      - name: Upload focused", 1)[0]
        command = "python -m unittest discover -s tools/agents -p 'test_*.py' -v"
        self.assertEqual(workflow.count(command), 1)
        self.assertIn(command, step)
        self.assertIn("set -o pipefail", step)
        self.assertIn("2>&1 | tee remote-agent-tests.log", step)
        self.assertNotIn("--host", step)
        self.assertNotIn("submit", step)
        self.assertNotIn("continue-on-error", block)
        self.assertIn(
            "path: |\n            package-release-tests.log\n            remote-agent-tests.log",
            block,
        )
        static = workflow[workflow.index("  static:") : workflow.index("  workspace-tests:")]
        self.assertIn("tools/agents/provision-arch-worker.sh", static)

    def test_timing_sensitive_jobs_upload_logs_without_retry(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        block = workflow[
            workflow.index("  daemon-tests:") : workflow.index("  package-automation:")
        ]
        self.assertIn("daemon-end-to-end.log", block)
        self.assertIn("mcp-protocol.log", block)
        self.assertNotIn("continue-on-error", block)
        self.assertNotIn("nick-fields/retry", block)

    def test_foot_differentials_are_advisory_not_release_authority(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        standalone = PINNED_FOOT_WORKFLOW.read_text(encoding="utf-8")
        self.assertNotIn("foot-reference", workflow)
        self.assertNotIn("tools/foot-oracle/check-provenance.py --portable", workflow)
        self.assertNotIn("tools/foot-oracle/test_*.py", workflow)
        self.assertNotIn("self-hosted", workflow)
        self.assertIn("Splinterm-owned renderer", workflow)
        self.assertIn(
            "python -m pytest -q tools/foot-oracle/test_run_final_buffer_comparison.py",
            workflow,
        )
        self.assertIn("workflow_dispatch:", standalone)
        self.assertIn("schedule:", standalone)
        self.assertIn("runs-on: ubuntu-latest", standalone)
        self.assertIn(
            "python tools/foot-oracle/check-provenance.py --portable", standalone
        )
        self.assertIn("python -m pytest -q tools/foot-oracle/test_*.py", standalone)
        self.assertIn(
            "runs-on: [self-hosted, linux, x64, splinterm-oracle]", standalone
        )
        self.assertNotIn("\n  push:", standalone)
        self.assertNotIn("\n  pull_request:", standalone)

    def test_flake_stress_bootstraps_before_frozen_repetitions(self) -> None:
        workflow = (ROOT / ".github/workflows/flake-stress.yml").read_text(
            encoding="utf-8"
        )
        fetch = "run: cargo fetch --locked"
        helper = "cargo build --frozen -p splinterm-pty --bin splinterm-pty-child"
        relay = "-p splinterm-relay --bin splinterm-relay"
        self.assertEqual(workflow.count(fetch), 1)
        self.assertEqual(workflow.count(helper), 1)
        self.assertEqual(workflow.count(relay), 1)
        self.assertLess(workflow.index(fetch), workflow.index("--frozen"))
        loop = workflow.index("for ((iteration = 1;")
        self.assertLess(workflow.index(helper), loop)
        for target in (
            "cargo test --frozen -p splinterd --test end_to_end -- --test-threads=1",
            "cargo test --frozen -p splinterm-mcp --test stdio_protocol -- --test-threads=1",
        ):
            self.assertEqual(workflow.count(target), 1)
            self.assertGreater(workflow.index(target), loop)

    def test_flake_stress_is_manual_scheduled_bounded_and_stops_on_failure(self) -> None:
        workflow = (ROOT / ".github/workflows/flake-stress.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("schedule:", workflow)
        self.assertIn("REQUESTED_ITERATIONS <= 25", workflow)
        self.assertIn("set -o pipefail", workflow)
        self.assertNotIn("continue-on-error", workflow)
        self.assertNotIn("nick-fields/retry", workflow)


if __name__ == "__main__":
    unittest.main()
