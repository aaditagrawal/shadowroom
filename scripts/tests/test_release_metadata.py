"""Keep every lockfile in sync with the released path dependency."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "release_metadata", Path(__file__).resolve().parents[1] / "release-metadata.py"
)
metadata = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metadata)


class ReleaseMetadataTests(unittest.TestCase):
    def check_versions(self, root_version, prototype_version):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative, version in (
                ("Cargo.lock", root_version),
                ("tools/compat-desktop/Cargo.lock", prototype_version),
            ):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(
                    f'[[package]]\nname = "shadowroom"\nversion = "{version}"\n'
                )
            metadata.validate_lockfiles(root, "0.1.6")

    def test_matching_lockfiles(self):
        self.check_versions("0.1.6", "0.1.6")

    def test_stale_prototype_names_the_lockfile(self):
        with self.assertRaisesRegex(SystemExit, "tools/compat-desktop/Cargo.lock"):
            self.check_versions("0.1.6", "0.1.4")

    def test_stale_root_names_the_lockfile(self):
        with self.assertRaisesRegex(SystemExit, "^Cargo.lock"):
            self.check_versions("0.1.5", "0.1.6")


if __name__ == "__main__":
    unittest.main()
