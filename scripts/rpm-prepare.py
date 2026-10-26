#!/usr/bin/env python3
"""Generate RPM metadata from an exact source archive and locked Cargo metadata."""

import argparse
import hashlib
import json
import re
import tomllib
from datetime import UTC, datetime
from pathlib import Path


def prepare(source, metadata, output, release, source_sha, source_epoch):
    package = tomllib.loads((source / "Cargo.toml").read_text())["package"]
    version = package["version"]
    if package["name"] != "shadowroom" or not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError(
            "RPM packaging requires Shadowroom with a stable numeric version"
        )
    if not re.fullmatch(r"[1-9]\d*", release):
        raise ValueError("RPM release must be a positive integer")
    if not re.fullmatch(r"[0-9a-f]{40,64}", source_sha):
        raise ValueError("RPM provenance requires the full source commit SHA")
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    licenses = {"Apache-2.0", "CC-BY-4.0", "OFL-1.1"}
    rust_versions = [(1, 89, 0)]
    inventory = []
    for dependency in metadata["packages"]:
        if dependency["id"] not in resolved:
            continue
        expression = dependency.get("license", "").replace("/", " OR ")
        expression = " ".join(expression.split())
        if not expression or not re.fullmatch(r"[A-Za-z0-9().+ -]+", expression):
            raise ValueError(
                f"Missing or unsafe license expression for {dependency['name']}"
            )
        licenses.add(expression)
        if dependency.get("rust_version"):
            numbers = tuple(map(int, dependency["rust_version"].split(".")))
            rust_versions.append((*numbers, *([0] * (3 - len(numbers)))))
        inventory.append(
            {
                "name": dependency["name"],
                "version": dependency["version"],
                "license": expression,
            }
        )
    output.mkdir(parents=True, exist_ok=True)
    provenance = {
        "source_commit": source_sha,
        "source_date_epoch": source_epoch,
        "package_version": version,
        "rpm_release": release,
        "cargo_lock_sha256": hashlib.sha256(
            (source / "Cargo.lock").read_bytes()
        ).hexdigest(),
        "minimum_rust": ".".join(map(str, max(rust_versions))),
        "dependency_inventory_scope": "Resolved Linux desktop build/development superset; not every entry is linked into the binaries",
        "dependency_licenses": sorted(
            inventory, key=lambda item: (item["name"], item["version"])
        ),
        "optional_codecs": "System-provided; no bundled libheif/libde265/LibRaw runtime",
    }
    replacements = {
        "VERSION": version,
        "RELEASE": release,
        "LICENSES": " AND ".join(f"({item})" for item in sorted(licenses)),
        "RUST_VERSION": provenance["minimum_rust"],
        "SOURCE_SHA": source_sha,
        "CHANGELOG_DATE": datetime.fromtimestamp(source_epoch, tz=UTC).strftime(
            "%a %b %d %Y"
        ),
    }
    spec = (source / "packaging/rpm/shadowroom.spec.in").read_text()
    for name, value in replacements.items():
        spec = spec.replace(f"@{name}@", value)
    if re.search(r"@[A-Z_]+@", spec):
        raise ValueError("Unexpanded RPM template placeholders")
    (output / "shadowroom.spec").write_text(spec)
    (output / "SOURCE-PROVENANCE.json").write_text(
        json.dumps(provenance, indent=2) + "\n"
    )
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--release", default="1")
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--source-epoch", required=True, type=int)
    args = parser.parse_args()
    print(
        prepare(
            args.source,
            json.loads(args.metadata.read_text()),
            args.output,
            args.release,
            args.source_sha,
            args.source_epoch,
        )
    )


if __name__ == "__main__":
    main()
