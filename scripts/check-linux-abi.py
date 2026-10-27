#!/usr/bin/env python3
"""Check required GLIBC versions in every ELF object of a portable package.

Uses ELF version-needs records, not strings or exported symbols. A weak imported
symbol can still have a mandatory version-needs entry, so symbol binding alone
is insufficient to establish the minimum runtime ABI.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
from pathlib import Path


def version(value):
    if not re.fullmatch(r"\d+(?:\.\d+)+", value):
        raise ValueError("Invalid GLIBC version: " + value)
    return tuple(int(part) for part in value.split("."))


def required_versions(output):
    needs = False
    library = None
    records = []
    for line in output.splitlines():
        if line.startswith("Version ") and "section" in line:
            needs = line.startswith("Version needs section")
        if not needs:
            continue
        match = re.search(r"\bFile:\s*(\S+)", line)
        if match:
            library = match[1]
        match = re.search(r"\bName:\s*(GLIBC_[\w.]+)\s+Flags:\s*(\S+)", line)
        if match:
            records.append(
                {
                    "library": library,
                    "name": match[1],
                    "weak": match[2].upper() == "WEAK",
                }
            )
    return records


def inspect(path, readelf):
    result = subprocess.run(
        [readelf, "--wide", "--version-info", "--dynamic", str(path)],
        check=True,
        text=True,
        capture_output=True,
        env={**os.environ, "LC_ALL": "C"},
    )
    return {
        "requires": required_versions(result.stdout),
        "needed_libraries": re.findall(
            r"\(NEEDED\).*?Shared library: \[([^]]+)\]", result.stdout
        ),
        "search_paths": re.findall(
            r"\((?:RUNPATH|RPATH)\).*?\[([^]]*)\]", result.stdout
        ),
        "soname": next(
            iter(re.findall(r"\(SONAME\).*?\[([^]]+)\]", result.stdout)), None
        ),
    }


def audit(root, baseline, readelf, required_bundles=()):
    root = root.resolve(strict=True)
    base = root if root.is_dir() else root.parent
    candidates = sorted(root.rglob("*")) if root.is_dir() else [root]
    objects = []
    errors = []
    aliases = set()
    seen = set()
    for candidate in candidates:
        if not candidate.is_file():
            continue
        path = candidate.resolve(strict=True)
        if not path.is_relative_to(base):
            errors.append(f"Package file escapes root: {candidate.relative_to(base)}")
            continue
        with path.open("rb") as stream:
            if stream.read(4) != b"\x7fELF":
                continue
        aliases.add(candidate.name)
        if path in seen:
            continue
        seen.add(path)
        record = inspect(path, readelf)
        record["path"] = str(path.relative_to(base))
        if record["soname"]:
            aliases.add(record["soname"])
        hard = [r for r in record["requires"] if not r["weak"]]
        numeric = [
            r["name"][6:]
            for r in hard
            if re.fullmatch(r"GLIBC_\d+(?:\.\d+)+", r["name"])
        ]
        record["minimum_glibc"] = max(numeric, key=version, default=None)
        for requirement in hard:
            name = requirement["name"]
            if name == "GLIBC_PRIVATE":
                errors.append(f"{record['path']}: imports private glibc ABI")
            elif re.fullmatch(r"GLIBC_\d+(?:\.\d+)+", name) and version(
                name[6:]
            ) > version(baseline):
                errors.append(
                    f"{record['path']}: requires {name} from {requirement['library']} (maximum {baseline})"
                )
            elif not re.fullmatch(r"GLIBC_\d+(?:\.\d+)+", name):
                errors.append(f"{record['path']}: unrecognized required ABI {name}")
        objects.append(record)
    if not objects:
        errors.append("No ELF objects found in package")
    for library in required_bundles:
        if library not in aliases:
            errors.append("Required bundled library missing: " + library)
    for record in objects:
        record["external_libraries"] = sorted(set(record["needed_libraries"]) - aliases)
    return {
        "schema": 1,
        "maximum_glibc": baseline,
        "result": "FAIL" if errors else "PASS",
        "objects": objects,
        "errors": errors,
        "limitations": [
            "Static ABI inspection does not prove kernel, CPU, display, or GPU compatibility.",
            "Libraries loaded by name at runtime must also be inventoried and exercised.",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--max-glibc", default="2.28")
    parser.add_argument("--readelf", default="readelf")
    parser.add_argument("--require-bundled", action="append", default=[])
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    version(args.max_glibc)
    readelf = shutil.which(args.readelf)
    if not readelf:
        parser.error("readelf is required")
    try:
        report = audit(args.root, args.max_glibc, readelf, args.require_bundled)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(2, f"ABI inspection failed: {error}\n")
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    for error in report["errors"]:
        print(error)
    print(
        f"{report['result']}: inspected {len(report['objects'])} ELF objects; GLIBC <= {args.max_glibc}"
    )
    return bool(report["errors"])


if __name__ == "__main__":
    raise SystemExit(main())
