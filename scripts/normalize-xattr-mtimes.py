#!/usr/bin/env python3
"""Give tracked patched-xattr inputs stable mtimes without changing their bytes.

The dedicated artifact cache must use an exact key containing all vendor bytes,
Cargo manifests, toolchain and build configuration; never use restore fallbacks.
Ordinary application inputs retain their checkout timestamps and rebuild normally.
"""

import argparse
import os
import stat
import subprocess
from pathlib import Path

SOURCE_MTIME_NS = 315532800 * 1_000_000_000  # 1980-01-01 UTC.


def normalize(root: Path) -> int:
    tracked = subprocess.check_output(
        ["git", "ls-files", "-z", "--", "vendor/xattr"], cwd=root
    )
    paths = [root / os.fsdecode(name) for name in tracked.split(b"\0") if name]
    if not paths:
        raise RuntimeError("No tracked vendor/xattr inputs found")
    for path in paths:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode):
            raise RuntimeError(f"Expected a regular tracked xattr input: {path}")
        os.utime(
            path,
            ns=(metadata.st_atime_ns, SOURCE_MTIME_NS),
            follow_symlinks=False,
        )
    return len(paths)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parent.parent
    )
    args = parser.parse_args()
    print(f"Normalized {normalize(args.root.resolve())} tracked xattr input mtimes")


if __name__ == "__main__":
    main()
