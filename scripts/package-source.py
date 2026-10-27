#!/usr/bin/env python3
"""Create a source release from the public tree, excluding local/build artifacts."""

import argparse
import hashlib
import subprocess
import tarfile
import tomllib
from pathlib import Path

ROOTS = [
    ".github",
    ".gitignore",
    "Cargo.toml",
    "Cargo.lock",
    "README.md",
    "CONTRIBUTING.md",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
    "assets",
    "docs",
    "examples",
    "packaging",
    "scripts",
    "src",
    "tests",
    "tools",
    "vendor",
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
    prefix = f"shadowroom-{version}"
    output = args.output or root / "dist" / f"{prefix}-source.tar.gz"
    output.parent.mkdir(parents=True, exist_ok=True)

    def public(info):
        path = Path(info.name)
        if (
            "__pycache__" in path.parts
            or path.suffix in {".pyc", ".pyo"}
            or path.name in {"contact-sheet.png", "contact-sheet.svg", ".DS_Store"}
        ):
            return None
        return info

    # A release comes from the reviewed public tree. Do not sweep up untracked
    # screenshots, manifests or local evidence below docs/ and other public roots.
    tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    files = sorted(
        name for name in tracked
        if name and (name in ROOTS or Path(name).parts[0] in ROOTS)
    )
    with tarfile.open(output, "x:gz") as archive:
        for name in files:
            archive.add(root / name, arcname=f"{prefix}/{name}", recursive=False, filter=public)
    with output.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    output.with_suffix(output.suffix + ".sha256").write_text(
        f"{digest}  {output.name}\n"
    )
    print(output)


if __name__ == "__main__":
    main()
