"""Regression coverage for ELF requirements parsing and package boundaries."""

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    "linux_abi", Path(__file__).parents[1] / "check-linux-abi.py"
)
abi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(abi)


class LinuxAbiTests(unittest.TestCase):
    def test_readelf_locale_is_stable_even_when_the_callers_locale_is_not(self):
        with (
            patch.dict("os.environ", {"LC_ALL": "fr_FR.UTF-8"}),
            patch.object(abi.subprocess, "run", return_value=Mock(stdout="")) as run,
        ):
            abi.inspect(Path("example"), "readelf")
        self.assertEqual(run.call_args.kwargs["env"]["LC_ALL"], "C")

    def test_definitions_and_symbol_binding_are_not_runtime_requirements(self):
        output = """Version definition section '.gnu.version_d' contains 1 entry:
  0x001c: Name: GLIBC_2.44 Flags: none Version: 3
Version needs section '.gnu.version_r' contains 1 entry:
  0x0000: Version: 1 File: libc.so.6 Cnt: 2
  0x0010: Name: GLIBC_2.17 Flags: none Version: 2
  0x0020: Name: GLIBC_2.39 Flags: WEAK Version: 3
"""
        self.assertEqual(
            abi.required_versions(output),
            [
                {"library": "libc.so.6", "name": "GLIBC_2.17", "weak": False},
                {"library": "libc.so.6", "name": "GLIBC_2.39", "weak": True},
            ],
        )

    def test_numeric_versions_are_compared_numerically(self):
        self.assertGreater(abi.version("2.28"), abi.version("2.9"))
        with self.assertRaises(ValueError):
            abi.version("2.28junk")

    def test_scans_libraries_and_preserves_mandatory_version_records(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "libcodec.so.1").write_bytes(b"\x7fELFtest")
            (root / "libcodec.so").symlink_to("libcodec.so.1")
            (root / "launcher").write_text("#!/bin/sh\n")
            record = {
                "requires": [
                    {"library": "libc.so.6", "name": "GLIBC_2.34", "weak": False}
                ],
                "needed_libraries": ["libc.so.6"],
                "search_paths": ["$ORIGIN"],
                "soname": "libcodec.so.1",
            }
            with patch.object(abi, "inspect", return_value=record) as inspect:
                result = abi.audit(root, "2.28", "readelf", ["libcodec.so.1"])
            self.assertEqual(inspect.call_count, 1)
            self.assertEqual(result["result"], "FAIL")
            self.assertIn("GLIBC_2.34", result["errors"][0])
            self.assertEqual(result["objects"][0]["external_libraries"], ["libc.so.6"])

    def test_missing_payload_and_external_symlinks_fail(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root = parent / "package"
            root.mkdir()
            (parent / "outside").write_bytes(b"\x7fELFtest")
            (root / "liboutside.so").symlink_to(parent / "outside")
            result = abi.audit(root, "2.28", "readelf", ["libheif.so.1"])
            self.assertEqual(result["result"], "FAIL")
            self.assertTrue(any("escapes root" in error for error in result["errors"]))
            self.assertTrue(any("libheif.so.1" in error for error in result["errors"]))


if __name__ == "__main__":
    unittest.main()
