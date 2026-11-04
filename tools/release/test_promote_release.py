from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock
from urllib.error import HTTPError

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/release/promote-release.py"
SPEC = importlib.util.spec_from_file_location("promote_release", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

REPOSITORY = "ministry-of-taste/splinterm"
COMMIT = "a" * 40
VERSION = "1.2.3-alpha4"
RUN_ID = 12345
BRANCH = "main"


class CandidateFixture:
    def __init__(
        self, root: Path, previous_version_tag: str = "v0.2.0",
        version: str = VERSION,
    ) -> None:
        self.root = root
        self.assets = MODULE.expected_assets(COMMIT, version)
        records = []
        for relative, kind in self.assets.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"{relative}\n".encode())
            records.append(
                {"kind": kind, "path": relative, "sha256": MODULE.sha256(path)}
            )
        records.sort(key=lambda value: value["path"])
        self.manifest = {
            "schema": 1,
            "state": "candidate",
            "publishable": False,
            "repository": REPOSITORY,
            "commit": COMMIT,
            "version": version,
            "package_version": version.replace("-", ""),
            "tag": f"v{version}",
            "architecture": "x86_64",
            "previous_version_tag": previous_version_tag,
            "workflow_run": f"https://github.com/{REPOSITORY}/actions/runs/{RUN_ID}",
            "ci": {
                "workflow": "CI",
                "workflow_path": ".github/workflows/ci.yml",
                "event": "push",
                "branch": BRANCH,
                "commit": COMMIT,
                "run_id": 67890,
                "run_url": f"https://github.com/{REPOSITORY}/actions/runs/67890",
                "check_job": "check",
                "status": "completed",
                "conclusion": "success",
            },
            "assets": records,
        }
        manifest_path = root / "candidate-manifest.json"
        manifest_path.write_text(
            json.dumps(self.manifest, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        self.manifest_sha256 = MODULE.sha256(manifest_path)
        lines = [f"{record['sha256']}  {record['path']}" for record in records]
        lines.append(f"{self.manifest_sha256}  candidate-manifest.json")
        (root / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")


class PromoteReleaseTests(unittest.TestCase):
    def test_source_run_requires_matching_release_branch_and_one_artifact(self) -> None:
        run = {
            "id": RUN_ID,
            "event": "workflow_dispatch",
            "status": "completed",
            "conclusion": "success",
            "path": ".github/workflows/release-candidate.yml",
            "head_branch": "main",
            "head_sha": COMMIT,
            "repository": {"full_name": REPOSITORY},
        }
        artifacts = {
            "artifacts": [
                {
                    "id": 99,
                    "name": f"splinterm-{VERSION}-candidate-{COMMIT}",
                    "expired": False,
                    "workflow_run": {"id": RUN_ID, "head_sha": COMMIT},
                }
            ]
        }
        for branch in sorted(MODULE.RELEASE_BRANCHES):
            run["head_branch"] = branch
            self.assertEqual(
                MODULE.validate_source_run(
                    run, artifacts, REPOSITORY, RUN_ID, branch
                ),
                (99, COMMIT),
            )
        artifacts["artifacts"].append(
            {
                "id": 100,
                "name": "diagnostics",
                "expired": False,
                "workflow_run": {"id": RUN_ID, "head_sha": COMMIT},
            }
        )
        with self.assertRaisesRegex(ValueError, "exactly one unexpired"):
            MODULE.validate_source_run(
                run, artifacts, REPOSITORY, RUN_ID, "maint/0.1"
            )
        artifacts["artifacts"].pop()
        run["head_branch"] = "main"
        with self.assertRaisesRegex(ValueError, "head_branch"):
            MODULE.validate_source_run(
                run, artifacts, REPOSITORY, RUN_ID, "maint/0.1"
            )
        with self.assertRaisesRegex(ValueError, "release authority"):
            MODULE.validate_source_run(
                run, artifacts, REPOSITORY, RUN_ID, "feature"
            )

    def test_candidate_closes_over_exact_files_and_hashes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-promotion-") as value:
            fixture = CandidateFixture(Path(value))
            promotion = MODULE.verify_candidate(
                fixture.root,
                REPOSITORY,
                RUN_ID,
                COMMIT,
                BRANCH,
                fixture.manifest_sha256,
            )
            self.assertEqual(promotion["commit"], COMMIT)
            self.assertEqual(promotion["release_title"], f"Splinterm {VERSION}")
            self.assertIs(promotion["release_prerelease"], True)
            self.assertEqual(
                promotion["release_notes_sha256"],
                MODULE.sha256(fixture.root / "RELEASE-NOTES.md"),
            )
            self.assertEqual(len(promotion["public_assets"]), 5)
            self.assertIn("candidate-manifest.json", promotion["public_assets"])
            self.assertIn("SHA256SUMS", promotion["public_assets"])
            self.assertNotIn("aur-source/PKGBUILD", promotion["public_assets"])

    def test_release_classification_rejects_malformed_versions(self) -> None:
        for version in (None, "", "v0.1.0", "0.1", "0.1.0\n"):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "malformed"):
                MODULE.is_prerelease(version)

    def test_stable_and_prerelease_candidates_bind_publication_state(self) -> None:
        for version in ("0.1.0", "0.1.0-alpha1", "0.1.0-beta3", "0.1.0-rc.3"):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as value:
                candidate = Path(value) / "candidate"
                fixture = CandidateFixture(candidate, version=version)
                promotion = MODULE.verify_candidate(
                    fixture.root, REPOSITORY, RUN_ID, COMMIT, BRANCH, fixture.manifest_sha256
                )
                expected = "-" in version
                self.assertIs(promotion["release_prerelease"], expected)
                downloads = Path(value) / "downloads"
                downloads.mkdir()
                names = set(promotion["public_assets"])
                for name in names:
                    (downloads / name).write_bytes((candidate / name).read_bytes())
                release = {
                    "tagName": f"v{version}",
                    "name": promotion["release_title"],
                    "body": (candidate / "RELEASE-NOTES.md").read_text(encoding="utf-8"),
                    "isDraft": False,
                    "isPrerelease": expected,
                    "url": f"https://github.com/{REPOSITORY}/releases/tag/v{version}",
                    "assets": [{"name": name} for name in sorted(names)],
                }
                ref = {"object": {"sha": COMMIT}}
                receipt = MODULE.create_receipt(
                    promotion, release, ref, downloads, "https://example.invalid/run/1"
                )
                self.assertIs(receipt["release_prerelease"], expected)
                for wrong in (not expected, None, str(expected).lower(), int(expected)):
                    release["isPrerelease"] = wrong
                    with self.assertRaisesRegex(ValueError, "prerelease state"):
                        MODULE.create_receipt(
                            promotion, release, ref, downloads, "https://example.invalid/run/1"
                        )
                release["isPrerelease"] = expected
                promotion["release_prerelease"] = not expected
                with self.assertRaisesRegex(ValueError, "promotion prerelease state"):
                    MODULE.create_receipt(
                        promotion, release, ref, downloads, "https://example.invalid/run/1"
                    )

    def test_candidate_rejects_a_stale_predecessor_even_when_well_formed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-promotion-") as value:
            fixture = CandidateFixture(Path(value), "v0.1.0-alpha3.3")
            with self.assertRaisesRegex(ValueError, "current public release state"):
                MODULE.verify_candidate(
                    fixture.root,
                    REPOSITORY,
                    RUN_ID,
                    COMMIT,
                    BRANCH,
                    fixture.manifest_sha256,
                    require_current_predecessor=True,
                )
            # Distribution and recovery reverify after the public state advances.
            promotion = MODULE.verify_candidate(
                fixture.root,
                REPOSITORY,
                RUN_ID,
                COMMIT,
                BRANCH,
                fixture.manifest_sha256,
            )
            self.assertEqual(promotion["commit"], COMMIT)

    def test_candidate_rejects_tampering_extra_files_and_unsafe_paths(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-promotion-") as value:
            fixture = CandidateFixture(Path(value))
            (fixture.root / f"splinterm-{VERSION}.tar.gz").write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "changed"):
                MODULE.verify_candidate(
                    fixture.root,
                    REPOSITORY,
                    RUN_ID,
                    COMMIT,
                    BRANCH,
                    fixture.manifest_sha256,
                )
        with tempfile.TemporaryDirectory(prefix="splinterm-promotion-") as value:
            fixture = CandidateFixture(Path(value))
            (fixture.root / "extra").write_text("unexpected", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unexpected files"):
                MODULE.verify_candidate(
                    fixture.root,
                    REPOSITORY,
                    RUN_ID,
                    COMMIT,
                    BRANCH,
                    fixture.manifest_sha256,
                )
        for unsafe in ("../asset", "/asset", "a/../../asset"):
            with self.assertRaisesRegex(ValueError, "unsafe"):
                MODULE.safe_relative(unsafe)

    def test_ci_provenance_must_match_promotion_authority_branch(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-promotion-") as value:
            fixture = CandidateFixture(Path(value))
            with self.assertRaisesRegex(ValueError, "branch"):
                MODULE.verify_candidate(
                    fixture.root,
                    REPOSITORY,
                    RUN_ID,
                    COMMIT,
                    "maint/0.1",
                    fixture.manifest_sha256,
                )

    def test_manifest_hash_and_source_commit_are_explicit_approval_identity(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-promotion-") as value:
            fixture = CandidateFixture(Path(value))
            with self.assertRaisesRegex(ValueError, "approval input"):
                MODULE.verify_candidate(
                    fixture.root, REPOSITORY, RUN_ID, COMMIT, BRANCH, "b" * 64
                )
            with self.assertRaisesRegex(ValueError, "source run"):
                MODULE.verify_candidate(
                    fixture.root,
                    REPOSITORY,
                    RUN_ID,
                    "b" * 40,
                    BRANCH,
                    fixture.manifest_sha256,
                )

    def test_receipt_requires_exact_tag_commit_and_public_assets(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-receipt-") as value:
            candidate = Path(value) / "candidate"
            candidate.mkdir()
            fixture = CandidateFixture(candidate)
            promotion = MODULE.verify_candidate(
                candidate,
                REPOSITORY,
                RUN_ID,
                COMMIT,
                BRANCH,
                fixture.manifest_sha256,
            )
            downloads = Path(value) / "downloads"
            downloads.mkdir()
            names = set(promotion["public_assets"])
            for name in names:
                source = candidate / name
                (downloads / name).write_bytes(source.read_bytes())
            release = {
                "tagName": f"v{VERSION}",
                "name": promotion["release_title"],
                "body": (candidate / "RELEASE-NOTES.md").read_text(encoding="utf-8"),
                "isDraft": False,
                "isPrerelease": True,
                "url": f"https://github.com/{REPOSITORY}/releases/tag/v{VERSION}",
                "assets": [{"name": name} for name in sorted(names)],
            }
            ref = {"object": {"sha": COMMIT}}
            receipt = MODULE.create_receipt(
                promotion, release, ref, downloads, "https://example.invalid/run/1"
            )
            self.assertEqual(receipt["state"], "published")
            self.assertEqual(receipt["ci"], promotion["ci"])
            self.assertEqual(len(receipt["assets"]), 5)
            (downloads / f"splinterm-{VERSION}.tar.gz").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "differs from approved"):
                MODULE.create_receipt(
                    promotion, release, ref, downloads, "https://example.invalid/run/1"
                )
            (downloads / f"splinterm-{VERSION}.tar.gz").write_bytes(
                (candidate / f"splinterm-{VERSION}.tar.gz").read_bytes()
            )
            release["assets"].append({"name": "unexpected"})
            with self.assertRaisesRegex(ValueError, "not exact"):
                MODULE.create_receipt(
                    promotion, release, ref, downloads, "https://example.invalid/run/1"
                )

    def test_release_metadata_and_environment_are_interpreted_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-state-") as value:
            fixture = CandidateFixture(Path(value))
            promotion = MODULE.verify_candidate(
                fixture.root, REPOSITORY, RUN_ID, COMMIT, BRANCH,
                fixture.manifest_sha256,
            )
            release = {
                "tag_name": promotion["tag"],
                "name": promotion["release_title"],
                "body": (fixture.root / "RELEASE-NOTES.md").read_text(),
                "draft": False,
                "prerelease": True,
                "assets": [],
            }
            MODULE.validate_release_metadata(promotion, release)
            release["name"] = "altered"
            with self.assertRaisesRegex(ValueError, "title"):
                MODULE.validate_release_metadata(promotion, release)
        fixture = json.loads(
            (ROOT / "tools/release/fixtures/environment-policy.json").read_text()
        )
        environment = fixture["environment"]
        policies = fixture["policies"]
        MODULE.validate_environment(environment, policies, "aur-release")
        policies[0]["branch_policies"].append({"name": "feature"})
        with self.assertRaisesRegex(ValueError, "exactly"):
            MODULE.validate_environment(environment, policies, "aur-release")

    def test_recovery_returns_only_missing_operations_and_rejects_altered_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="splinterm-recovery-") as value:
            candidate = Path(value) / "candidate"
            candidate.mkdir()
            fixture = CandidateFixture(candidate)
            promotion = MODULE.verify_candidate(
                candidate, REPOSITORY, RUN_ID, COMMIT, BRANCH,
                fixture.manifest_sha256,
            )
            downloads = Path(value) / "downloads"
            downloads.mkdir()
            states = json.loads(
                (ROOT / "tools/release/fixtures/recovery-states.json").read_text()
            )
            refs = {
                None: None,
                "exact": {"object": {"sha": COMMIT}},
                "altered": {"object": {"sha": "9" * 40}},
            }
            for state in states:
                with self.subTest(state=state["name"]):
                    if "error" in state:
                        with self.assertRaisesRegex(ValueError, state["error"]):
                            MODULE.inspect_recovery(
                                promotion, refs[state["ref"]], None, downloads
                            )
                    else:
                        plan = MODULE.inspect_recovery(
                            promotion, refs[state["ref"]], None, downloads
                        )
                        self.assertEqual(
                            [item["operation"] for item in plan["operations"]],
                            state["expected_operations"],
                        )
            ref = refs["exact"]
            existing = sorted(promotion["public_assets"])[0]
            (downloads / existing).write_bytes((candidate / existing).read_bytes())
            release = {
                "tag_name": promotion["tag"],
                "name": promotion["release_title"],
                "body": (candidate / "RELEASE-NOTES.md").read_text(),
                "draft": False,
                "prerelease": True,
                "assets": [{"name": existing}],
            }
            plan = MODULE.inspect_recovery(promotion, ref, release, downloads)
            self.assertEqual(plan["operations"][0]["operation"], "upload-assets")
            self.assertNotIn(existing, plan["operations"][0]["assets"])

            for name in sorted(set(promotion["public_assets"]) - {existing}):
                (downloads / name).write_bytes((candidate / name).read_bytes())
                release["assets"].append({"name": name})
            receipt_only = MODULE.inspect_recovery(
                promotion, ref, release, downloads
            )
            self.assertEqual(receipt_only["state"], "receipt-only")
            self.assertEqual(receipt_only["operations"], [])

            release["assets"].append({"name": "extra"})
            with self.assertRaisesRegex(ValueError, "extra"):
                MODULE.inspect_recovery(promotion, ref, release, downloads)

    def test_recovery_accepts_only_unsuccessful_retained_promotion_conclusions(self) -> None:
        run = {
            "id": 56,
            "event": "workflow_dispatch",
            "status": "completed",
            "head_branch": "main",
            "head_sha": COMMIT,
            "path": MODULE.PROMOTION_WORKFLOW,
            "repository": {"full_name": REPOSITORY},
        }
        artifacts = {"artifacts": [{
            "id": 99,
            "name": "verified-release-candidate-12",
            "expired": False,
            "workflow_run": {"id": 56, "head_sha": COMMIT},
        }]}
        self.assertEqual(
            MODULE.RECOVERY_PROMOTION_CONCLUSIONS,
            {"failure", "cancelled", "timed_out"},
        )
        for conclusion in MODULE.RECOVERY_PROMOTION_CONCLUSIONS:
            with self.subTest(conclusion=conclusion):
                run["conclusion"] = conclusion
                self.assertEqual(
                    MODULE.validate_retained_artifact(
                        run, artifacts, REPOSITORY, 56, "main",
                        {MODULE.PROMOTION_WORKFLOW},
                        r"verified-release-candidate-[1-9][0-9]*",
                        MODULE.RECOVERY_PROMOTION_CONCLUSIONS,
                    ),
                    (99, COMMIT),
                )
        for conclusion in ("success", "neutral", "skipped", "action_required"):
            with self.subTest(refused_conclusion=conclusion):
                run["conclusion"] = conclusion
                with self.assertRaisesRegex(ValueError, "conclusion"):
                    MODULE.validate_retained_artifact(
                        run, artifacts, REPOSITORY, 56, "main",
                        {MODULE.PROMOTION_WORKFLOW},
                        r"verified-release-candidate-[1-9][0-9]*",
                        MODULE.RECOVERY_PROMOTION_CONCLUSIONS,
                    )

    def test_github_api_interpretation_allows_only_explicit_404(self) -> None:
        not_found = HTTPError("https://api.github.invalid", 404, "missing", {}, None)
        forbidden = HTTPError("https://api.github.invalid", 403, "forbidden", {}, None)
        with mock.patch.dict(os.environ, {"GH_TOKEN": "fixture-token"}):
            with mock.patch.object(MODULE, "urlopen", side_effect=not_found):
                self.assertIsNone(
                    MODULE.github_get(REPOSITORY, "releases/tags/v1", allow_not_found=True)
                )
            with mock.patch.object(MODULE, "urlopen", side_effect=forbidden):
                with self.assertRaisesRegex(ValueError, "HTTP 403"):
                    MODULE.github_get(
                        REPOSITORY, "releases/tags/v1", allow_not_found=True
                    )


if __name__ == "__main__":
    unittest.main()
