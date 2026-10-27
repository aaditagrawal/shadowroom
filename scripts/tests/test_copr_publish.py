"""Safety boundaries for the privileged publisher; no credentials/network needed."""

import hashlib
import importlib.util
import io
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "copr_publish", Path(__file__).parents[1] / "copr-publish.py"
)
publisher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publisher)


class PublisherSafety(unittest.TestCase):
    def build(self, state="succeeded", version="0.1.2-1"):
        return {
            "id": 42,
            "state": state,
            "source_package": {"name": "shadowroom", "version": version},
        }

    def run_and_jobs(self):
        return {
            "event": "push",
            "head_sha": "a" * 40,
            "head_branch": "v0.1.2",
            "path": ".github/workflows/ci.yml",
            "conclusion": "success",
            "status": "completed",
            "head_repository": {"full_name": publisher.REPOSITORY},
        }, [{"name": name, "conclusion": "success"} for name in publisher.REQUIRED_JOBS]

    def test_same_evr_reuses_successful_and_running_build(self):
        for state in ("succeeded", "pending", "running"):
            build = self.build(state)
            self.assertIs(publisher.existing_build([build], "0.1.2-1"), build)

    def test_failed_or_duplicate_evr_requires_operator_review(self):
        for builds in (
            [self.build("failed")],
            [self.build("canceled")],
            [self.build(), self.build()],
        ):
            with self.assertRaises(ValueError):
                publisher.existing_build(builds, "0.1.2-1")

    def test_unknown_inflight_upload_blocks_a_second_post(self):
        with self.assertRaisesRegex(ValueError, "unidentified"):
            publisher.existing_build(
                [{"id": 43, "state": "importing", "source_package": None}], "0.1.2-1"
            )
        with self.assertRaisesRegex(ValueError, "unidentified"):
            publisher.existing_build(
                [
                    {
                        "id": 43,
                        "state": "importing",
                        "source_package": {"name": "shadowroom"},
                    }
                ],
                "0.1.2-1",
            )
        self.assertIsNone(
            publisher.existing_build([self.build(version="0.1.1-1")], "0.1.2-1")
        )

    def test_source_run_requires_every_native_job_and_exact_repo_tag(self):
        run, jobs = self.run_and_jobs()
        publisher.validate_source_run(run, jobs, "v0.1.2", "a" * 40)
        for changed in (
            {"head_sha": "b" * 40},
            {"event": "pull_request"},
            {"path": ".github/workflows/other.yml"},
            {"head_repository": {"full_name": "fork/shadowroom"}},
        ):
            with self.assertRaises(ValueError):
                publisher.validate_source_run(
                    {**run, **changed}, jobs, "v0.1.2", "a" * 40
                )
        with self.assertRaises(ValueError):
            publisher.validate_source_run(run, jobs[:-1], "v0.1.2", "a" * 40)

    def test_current_reusable_run_does_not_wait_for_its_own_conclusion(self):
        run, jobs = self.run_and_jobs()
        run["conclusion"] = None
        run["status"] = "in_progress"
        publisher.validate_source_run(run, jobs, "v0.1.2", "a" * 40, current=True)
        with self.assertRaises(ValueError):
            publisher.validate_source_run(run, jobs, "v0.1.2", "a" * 40)

    def test_failed_copr_monitor_does_not_block_resuming_verified_release(self):
        run, jobs = self.run_and_jobs()
        run["conclusion"] = "failure"
        jobs.append(
            {
                "name": "Publish Fedora COPR / Verify and publish Fedora source RPM",
                "conclusion": "failure",
            }
        )
        publisher.validate_source_run(run, jobs, "v0.1.2", "a" * 40)
        jobs[0]["conclusion"] = "failure"
        with self.assertRaisesRegex(ValueError, "Required native"):
            publisher.validate_source_run(run, jobs, "v0.1.2", "a" * 40)

    def test_manifest_rejects_unsafe_names_duplicates_and_bad_digests(self):
        valid = "a" * 64 + "  shadowroom-0.1.2-1.src.rpm"
        self.assertEqual(
            publisher.checksum_manifest(valid)["shadowroom-0.1.2-1.src.rpm"], "a" * 64
        )
        for data in (
            "",
            valid + "\n" + valid,
            "a" * 64 + "  ../secret",
            "invalid  package.rpm",
        ):
            with self.assertRaises(ValueError):
                publisher.checksum_manifest(data)

    def test_download_bad_checksum_refuses_verified_bytes(self):
        with tempfile.TemporaryDirectory() as folder:
            with (
                patch.object(
                    publisher, "urlopen", return_value=io.BytesIO(b"changed bytes")
                ),
                self.assertRaisesRegex(ValueError, "checksum"),
            ):
                publisher.download(
                    "https://github.com/example",
                    Path(folder) / "source.rpm",
                    hashlib.sha256(b"release bytes").hexdigest(),
                )
            with self.assertRaises(ValueError):
                publisher.download(
                    "http://untrusted/source.rpm", Path(folder) / "other"
                )

    def test_provenance_archive_never_follows_symlink(self):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            entry = tarfile.TarInfo("rpm-notices/SOURCE-PROVENANCE.json")
            entry.type = tarfile.SYMTYPE
            entry.linkname = "/private/credentials"
            archive.addfile(entry)
        data.seek(0)
        with self.assertRaisesRegex(ValueError, "Unsafe"):
            publisher.tar_member(data, "rpm-notices/SOURCE-PROVENANCE.json")

    def test_config_rejects_other_endpoints_and_redacts_parse_errors(self):
        secret = "DO_NOT_PRINT_TOKEN"
        good = f"[copr-cli]\nusername=aaditagrawal\nlogin=api-login\ntoken={secret}\ncopr_url={publisher.COPR_URL}\n"
        publisher.credentials(good)
        for text in (
            good.replace(publisher.COPR_URL, "https://other.example"),
            good.replace("username=aaditagrawal", "username=other"),
            f"broken [{secret}",
        ):
            with self.assertRaises(ValueError) as failure:
                publisher.credentials(text)
            self.assertNotIn(secret, str(failure.exception))


if __name__ == "__main__":
    unittest.main()
