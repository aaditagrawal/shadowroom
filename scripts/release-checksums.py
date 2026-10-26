#!/usr/bin/env python3
"""Write portable SHA-256 sidecars and a combined manifest for release archives."""

import argparse
import hashlib
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    artifacts = sorted(
        path
        for path in args.directory.iterdir()
        if path.is_file()
        and path.name.endswith((".tar.gz", ".zip", ".AppImage", ".rpm"))
    )
    if not artifacts:
        raise SystemExit("No release archives found")
    checksums = []
    for path in artifacts:
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        entry = f"{digest}  {path.name}\n"
        path.with_name(path.name + ".sha256").write_text(entry)
        checksums.append(entry)
    (args.directory / "SHA256SUMS").write_text("".join(checksums))
    print(f"Checksummed {len(artifacts)} release artifacts")


if __name__ == "__main__":
    main()
