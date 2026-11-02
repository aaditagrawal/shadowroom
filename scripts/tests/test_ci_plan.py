"""Artifact promotion must preserve exact-source release validation."""

import copy
import importlib.util
import io
import json
import os
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "ci_plan", Path(__file__).parents[1] / "ci-plan.py"
)
planner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(planner)
publisher = planner.publisher
SHA = "a" * 40


class PromotionSafety(unittest.TestCase):
    def setUp(self):
        self.run = {
            "id": 123,
            "event": "push",
            "head_sha": SHA,
            "head_branch": "main",
            "path": ".github/workflows/ci.yml",
            "head_repository": {"full_name": publisher.REPOSITORY},
            "status": "completed",
            "conclusion": "success",
        }
        self.jobs = [
            {"name": n, "conclusion": "success"} for n in sorted(publisher.NATIVE_JOBS)
        ]
        self.artifacts = [
            {
                "name": n,
                "id": i,
                "expired": False,
                "size_in_bytes": 100,
                "digest": "sha256:" + "b" * 64,
            }
            for i, n in enumerate(sorted(publisher.NATIVE_ARTIFACTS), 1)
        ]

    def api(self, path):
        if path.startswith("actions/workflows/"):
            return {"workflow_runs": [self.run]}
        if path.endswith("/jobs?per_page=100"):
            return {"jobs": self.jobs}
        if path.endswith("/artifacts?per_page=100"):
            return {"artifacts": self.artifacts}
        if path == "actions/runs/123":
            return self.run
        raise AssertionError(path)

    def test_exact_successful_main_build_is_reused(self):
        self.assertEqual(planner.reusable_run(SHA, self.api), 123)
        receipt = planner.receipt(SHA, "v0.1.6", 123, 456, self.api)
        self.assertEqual(receipt["source_commit"], SHA)
        self.assertEqual(receipt["build_run_id"], 123)
        self.assertEqual(receipt["publish_run_id"], 456)
        self.assertEqual(len(receipt["artifacts"]), 4)

    def test_untrusted_incomplete_or_other_commit_builds_are_never_reused(self):
        original = dict(self.run)
        for changes in (
            {"event": "pull_request"},
            {"head_branch": "feature"},
            {"head_sha": "c" * 40},
            {"head_repository": {"full_name": "fork/shadowroom"}},
            {"path": ".github/workflows/other.yml"},
            {"status": "in_progress"},
            {"conclusion": "failure"},
        ):
            with self.subTest(changes=changes):
                self.run = {**original, **changes}
                self.assertEqual(planner.reusable_run(SHA, self.api), "")
        self.run = original
        self.jobs[0]["conclusion"] = "skipped"
        self.assertEqual(planner.reusable_run(SHA, self.api), "")

    def test_missing_expired_duplicate_or_unsigned_artifacts_force_fresh_builds(self):
        original = copy.deepcopy(self.artifacts)
        for change in ("missing", "expired", "duplicate", "digest", "empty"):
            with self.subTest(change=change):
                self.artifacts = copy.deepcopy(original)
                if change == "missing":
                    self.artifacts.pop()
                elif change == "duplicate":
                    self.artifacts.append(self.artifacts[0])
                elif change == "expired":
                    self.artifacts[0]["expired"] = True
                elif change == "digest":
                    self.artifacts[0]["digest"] = ""
                else:
                    self.artifacts[0]["size_in_bytes"] = 0
                self.assertEqual(planner.reusable_run(SHA, self.api), "")

    def test_receipt_revalidates_the_source_before_publication(self):
        self.run["conclusion"] = "failure"
        with self.assertRaisesRegex(ValueError, "successfully"):
            planner.receipt(SHA, "v0.1.6", 123, 456, self.api)

    def test_fresh_tag_receipt_also_requires_complete_native_checks(self):
        self.run["head_branch"] = "v0.1.6"
        self.assertEqual(
            planner.receipt(SHA, "v0.1.6", 123, 123, self.api)["build_run_id"], 123
        )
        self.jobs[0]["conclusion"] = "failure"
        with self.assertRaisesRegex(ValueError, "incomplete"):
            planner.receipt(SHA, "v0.1.6", 123, 123, self.api)

    def test_only_explicit_documentation_changes_skip_native_builds(self):
        self.assertFalse(planner.needs_native(["README.md", "docs/CONTROLS.md"]))
        self.assertFalse(planner.needs_native([]))
        for path in (
            "Cargo.toml",
            "Cargo.lock",
            "src/desktop.rs",
            "assets/icon.png",
            ".github/workflows/ci.yml",
            "scripts/check-docs.py",
            "new-file",
        ):
            with self.subTest(path=path):
                self.assertTrue(planner.needs_native(["docs/README.md", path]))

    def test_draft_pr_defers_compilation_until_ready(self):
        with patch.dict(
            os.environ, {"GITHUB_EVENT_NAME": "pull_request", "IS_DRAFT": "true"}
        ):
            self.assertEqual(planner.plan(SHA), {"reuse_run_id": "", "native": "false"})
        with patch.dict(
            os.environ,
            {"GITHUB_EVENT_NAME": "pull_request", "IS_DRAFT": "false", "BASE_SHA": ""},
        ):
            self.assertEqual(planner.plan(SHA), {"reuse_run_id": "", "native": "true"})

    def test_promoted_tag_requires_both_publisher_and_original_native_checks(self):
        tag_run = {
            **self.run,
            "id": 456,
            "head_branch": "v0.1.6",
            "status": "in_progress",
        }
        tag_jobs = [
            {"name": n, "conclusion": "success"}
            for n in ("Release preflight", "Publish verified GitHub release")
        ]
        publisher.validate_source_run(
            tag_run,
            tag_jobs,
            "v0.1.6",
            SHA,
            current=True,
            promoted=(self.run, self.jobs),
        )
        with self.assertRaises(ValueError):
            publisher.validate_source_run(
                tag_run,
                tag_jobs[:-1],
                "v0.1.6",
                SHA,
                current=True,
                promoted=(self.run, self.jobs),
            )
        with self.assertRaises(ValueError):
            publisher.validate_source_run(
                tag_run,
                tag_jobs,
                "v0.1.6",
                SHA,
                current=True,
                promoted=(self.run, self.jobs[:-1]),
            )

    def test_downloaded_provenance_must_match_the_publishing_run(self):
        receipt = planner.receipt(SHA, "v0.1.6", 123, 456, self.api)
        asset = {
            "name": "BUILD-PROVENANCE.json",
            "size": 100,
            "digest": "sha256:" + "a" * 64,
            "browser_download_url": "https://github.com/provenance",
        }

        def downloaded(url, path, expected):
            path.write_text(json.dumps(receipt))

        with patch.object(publisher, "download", side_effect=downloaded):
            actual = publisher.promoted_build(
                {"assets": [asset]}, "v0.1.6", SHA, 456, self.api
            )
            self.assertEqual(actual, (self.run, self.jobs))
            for changed in (
                {"source_commit": "c" * 40},
                {"tag": "v0.1.7"},
                {"publish_run_id": 789},
                {"build_run_id": "123"},
            ):
                with self.subTest(changed=changed):
                    original = dict(receipt)
                    receipt.update(changed)
                    with self.assertRaises(ValueError):
                        publisher.promoted_build(
                            {"assets": [asset]}, "v0.1.6", SHA, 456, self.api
                        )
                    receipt = original

    def test_provenance_download_checks_github_digest(self):
        asset = {
            "name": "BUILD-PROVENANCE.json",
            "size": 100,
            "digest": "sha256:" + "a" * 64,
            "browser_download_url": "https://github.com/provenance",
        }
        with (
            patch.object(publisher, "urlopen", return_value=io.BytesIO(b"tampered")),
            self.assertRaisesRegex(ValueError, "checksum"),
        ):
            publisher.promoted_build({"assets": [asset]}, "v0.1.6", SHA, 456, self.api)


if __name__ == "__main__":
    unittest.main()
