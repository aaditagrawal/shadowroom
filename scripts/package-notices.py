#!/usr/bin/env python3
"""Collect local license notices; never download dependencies or execute builds."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess


def collect(project: Path, destination: Path, cargo: str, features: str) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    legal = destination / "licenses"
    legal.mkdir()
    for name in ("LICENSE", "THIRD_PARTY_NOTICES.md", "Cargo.lock"):
        shutil.copy2(project / name, destination / name)
    shutil.copy2(project / "assets/icons/LICENSE", legal / "Codicons-CC-BY-4.0.txt")
    icon_legal = legal / "icons"
    icon_legal.mkdir()
    shutil.copy2(project / "assets/icons/README.md", icon_legal / "README.md")
    shutil.copy2(project / "assets/icons/shadowroom/trash-restore.svg", icon_legal / "trash-restore.svg")
    for family in ("dmsans", "dmmono", "geistmono"):
        target = legal / "fonts" / family
        target.mkdir(parents=True)
        shutil.copy2(project / "assets/fonts" / family / "OFL.txt", target / "OFL.txt")
    target = legal / "xattr"
    target.mkdir()
    for name in ("LICENSE-MIT", "LICENSE-APACHE", "SHADOWROOM-PATCH.md"):
        shutil.copy2(project / "vendor/xattr" / name, target / name)

    environment = dict(os.environ, RUSTUP_AUTO_INSTALL="0")
    host = subprocess.check_output([cargo, "-vV"], text=True, env=environment)
    host = next(line.split(": ", 1)[1] for line in host.splitlines() if line.startswith("host: "))
    metadata = json.loads(subprocess.check_output(
        [cargo, "metadata", "--offline", "--locked", "--format-version", "1",
         "--filter-platform", host, "--features", features],
        cwd=project, text=True, env=environment,
    ))
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    inventory = []
    prefixes = ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE")
    skipped = {".git", "target", "node_modules"}
    for package in sorted(metadata["packages"], key=lambda item: (item["name"], item["version"])):
        if package["id"] not in resolved or package["name"] == "shadowroom":
            continue
        source = Path(package["manifest_path"]).parent.resolve()
        paths = set()
        for directory, subdirectories, files in os.walk(source, followlinks=False):
            subdirectories[:] = sorted(name for name in subdirectories if name not in skipped)
            for name in files:
                inside_license_directory = any(
                    part.upper() in ("LICENSES", "LICENCES")
                    for part in Path(directory).relative_to(source).parts
                )
                if name.upper().startswith(prefixes) or inside_license_directory:
                    path = Path(directory) / name
                    if path.is_file() and not path.is_symlink():
                        paths.add(path)
        if package.get("license_file"):
            declared = source / package["license_file"]
            if declared.is_file() and declared.resolve().is_relative_to(source):
                paths.add(declared)
        files = []
        package_id = f"{package['name']}-{package['version']}"
        directory = legal / "dependencies" / package_id
        if directory.exists():
            raise SystemExit(f"Duplicate package identity requires manual notices: {package_id}")
        for path in sorted(paths):
            relative = path.relative_to(source)
            target = directory / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
            files.append(str(target.relative_to(legal)))
        supplemental = project / "assets/licenses/dependencies" / package_id
        if supplemental.is_dir():
            for path in sorted(supplemental.rglob("*")):
                if path.is_file() and not path.is_symlink():
                    target = directory / "upstream" / path.relative_to(supplemental)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(path, target)
                    files.append(str(target.relative_to(legal)))
        inventory.append({
            "name": package["name"], "version": package["version"],
            "license": package.get("license"), "authors": package.get("authors", []),
            "repository": package.get("repository"), "source": package.get("source"),
            "notice_files": files,
            "notice_status": (
                "supplemental material; see upstream/SOURCES.json" if supplemental.is_dir()
                else "collected from crate archive" if files
                else "no separate notice file in crate archive"
            ),
        })
    (legal / "dependencies.json").write_text(json.dumps(inventory, indent=2) + "\n")
    missing = sum(not item["notice_files"] for item in inventory)
    print(f"Collected notices for {len(inventory)} resolved dependency packages; "
          f"{missing} without collected notice material (see inventory/provenance).")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--cargo", required=True)
    parser.add_argument("--features", default="desktop")
    args = parser.parse_args()
    collect(Path(__file__).resolve().parent.parent, args.destination, args.cargo, args.features)
