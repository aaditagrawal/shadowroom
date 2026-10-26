#!/usr/bin/env python3
"""Check RPM provenance, license aggregation, and unsafe metadata rejection."""

import importlib.util
import json
import shutil
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "rpm_prepare", Path(__file__).with_name("rpm-prepare.py")
)
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)
PROJECT = Path(__file__).resolve().parent.parent


class PrepareTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        (self.source / "packaging/rpm").mkdir(parents=True)
        (self.source / "Cargo.toml").write_text(
            '[package]\nname = "shadowroom"\nversion = "0.1.0"\n'
        )
        (self.source / "Cargo.lock").write_text("version = 4\n")
        shutil.copyfile(
            PROJECT / "packaging/rpm/shadowroom.spec.in",
            self.source / "packaging/rpm/shadowroom.spec.in",
        )
        self.metadata = {
            "resolve": {"nodes": [{"id": "included"}]},
            "packages": [
                {
                    "id": "included",
                    "name": "example",
                    "version": "1.2.3",
                    "license": "MIT/Apache-2.0",
                    "rust_version": "1.93",
                },
                {
                    "id": "excluded",
                    "name": "unresolved",
                    "version": "9.0.0",
                    "license": "IGNORED",
                },
            ],
        }

    def prepare(self, **changes):
        arguments = {
            "source": self.source,
            "metadata": self.metadata,
            "output": self.root / "out",
            "release": "2",
            "source_sha": "a" * 40,
            "source_epoch": 1793059200,
        }
        return module.prepare(**(arguments | changes))

    def test_exact_provenance_and_resolved_licenses(self):
        self.assertEqual(self.prepare(), "0.1.0")
        output = self.root / "out"
        spec = (output / "shadowroom.spec").read_text()
        self.assertIn("Release:        2%{?dist}", spec)
        self.assertIn("rust >= 1.93.0", spec)
        self.assertIn("(MIT OR Apache-2.0)", spec)
        self.assertIn("(CC-BY-4.0)", spec)
        self.assertIn("(OFL-1.1)", spec)
        self.assertNotIn("IGNORED", spec)
        self.assertNotIn("@SOURCE_SHA@", spec)
        provenance = json.loads((output / "SOURCE-PROVENANCE.json").read_text())
        self.assertEqual(provenance["source_commit"], "a" * 40)
        self.assertEqual(len(provenance["cargo_lock_sha256"]), 64)
        self.assertEqual(
            [item["name"] for item in provenance["dependency_licenses"]], ["example"]
        )

    def test_rejects_metadata_that_can_change_rpm_macros(self):
        for expression in ["MIT\n%build", "MIT %{lua:1}", "", "MIT @VERSION@"]:
            with self.subTest(expression=expression):
                self.metadata["packages"][0]["license"] = expression
                with self.assertRaises(ValueError):
                    self.prepare()

    def test_rejects_ambiguous_source_or_release(self):
        for change in [
            {"release": "0"},
            {"release": "1%{?dist}"},
            {"source_sha": "HEAD"},
        ]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.prepare(**change)


if __name__ == "__main__":
    unittest.main()
