#!/usr/bin/env python3
"""Validate release tag/version agreement without running Cargo or a build."""

import json
import os
from pathlib import Path
import subprocess
import tomllib


def main():
    root = Path(__file__).resolve().parents[1]
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
    tag = os.environ.get("GITHUB_REF_NAME", "")
    if os.environ.get("GITHUB_REF_TYPE") == "tag" and tag != f"v{version}":
        raise SystemExit(f"Release tag {tag!r} must match Cargo.toml v{version}")
    values = {
        "version": version,
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "source_date_epoch": subprocess.check_output(
            ["git", "show", "-s", "--format=%ct", "HEAD"], cwd=root, text=True
        ).strip(),
    }
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with Path(output).open("a") as destination:
            for name, value in values.items():
                destination.write(f"{name}={value}\n")
    print(json.dumps(values))


if __name__ == "__main__":
    main()
