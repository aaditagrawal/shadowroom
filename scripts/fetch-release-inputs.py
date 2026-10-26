#!/usr/bin/env python3
"""Fetch only the public release inputs pinned in packaging/release-inputs.json."""

import argparse
import hashlib
import json
import os
import shutil
import tempfile
import time
import urllib.request
from pathlib import Path


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=("codecs", "appimage", "all"), default="all")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    manifest = json.loads((root / "packaging/release-inputs.json").read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    for entry in manifest["inputs"]:
        if args.kind != "all" and entry["kind"] != args.kind:
            continue
        target = args.output / entry["name"]
        if target.is_file() and digest(target) == entry["sha256"]:
            print(f"Verified cached {entry['name']}")
            continue
        for attempt in range(3):
            temporary = None
            try:
                request = urllib.request.Request(
                    entry["url"], headers={"User-Agent": "Shadowroom-release"}
                )
                with urllib.request.urlopen(request, timeout=60) as response:
                    if not response.url.startswith("https://"):
                        raise ValueError("Refusing a non-HTTPS download redirect")
                    with tempfile.NamedTemporaryFile(
                        dir=args.output, delete=False
                    ) as output:
                        temporary = Path(output.name)
                        shutil.copyfileobj(response, output)
                if digest(temporary) != entry["sha256"]:
                    raise ValueError(f"SHA-256 mismatch for {entry['name']}")
                os.replace(temporary, target)
                print(f"Downloaded and verified {entry['name']}")
                break
            except (OSError, ValueError):
                if attempt == 2:
                    raise
                time.sleep(attempt + 1)
            finally:
                if temporary is not None:
                    temporary.unlink(missing_ok=True)
    shutil.copyfile(
        root / "packaging/release-inputs.json", args.output / "release-inputs.json"
    )


if __name__ == "__main__":
    main()
