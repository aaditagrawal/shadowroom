#!/usr/bin/env python3
"""Measure a build command, optionally with small source edits, without cleaning caches."""

import argparse
import hashlib
import json
import os
import statistics
import subprocess
import time
import uuid
from pathlib import Path

PROBES = {
    "desktop-edit": ("src/main.rs", b"Shadowroom: {error:#}"),
    "core-edit": ("src/catalog.rs", b"Open local photo catalog"),
}
RECORDED_ENV = {
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "LCMS2_STATIC",
    "RUSTFLAGS",
    "RUSTC_BOOTSTRAP",
    "MBX_CACHE_DIR",
    "MBX_INCREMENTAL",
    "MBX_EAGER_INCREMENTAL",
    "MBX_LEARNED_INCREMENTAL",
    "MBX_SEED_INCREMENTAL",
    "MBX_LINKER",
    "MBX_SCHEDULER_CPUS",
    "MBX_SCHEDULER_MEMORY",
    "MBX_SCHEDULER_TESTS",
    "BLAZE_BACKEND",
    "BLAZE_MEMORY_MAX",
    "BLAZE_MEMORY_BUDGET",
    "BLAZE_HOME",
}


def other_builds(root):
    """Record other compiler processes rather than assuming the machine is idle."""
    found = []
    for proc in Path("/proc").glob("[0-9]*"):
        try:
            process = (proc / "comm").read_text().strip()
            if process not in {"cargo", "rustc", "blaze-engine"}:
                continue
            directory = (proc / "cwd").resolve()
            found.append(
                {
                    "pid": int(proc.name),
                    "project": directory.name,
                    "same_project": directory == root,
                    "process": process,
                }
            )
        except (OSError, ValueError):
            pass
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--label", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--project-root", type=Path)
    parser.add_argument(
        "--scenario", choices=["unchanged", *PROBES], default="unchanged"
    )
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--no-warmup", action="store_true")
    parser.add_argument(
        "--warmup-edit",
        action="store_true",
        help="Warm the first source edit before measuring steady edits",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or args.repeat < 1:
        parser.error("provide a command after -- and a positive --repeat")

    root = (args.project_root or Path(__file__).resolve().parents[1]).resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    run_id = uuid.uuid4().hex[:10]
    probe = PROBES.get(args.scenario)
    path = root / probe[0] if probe else None
    original = path.read_bytes() if path else b""
    expected = original
    if probe and original.count(probe[1]) != 1:
        parser.error(f"probe must occur exactly once in {probe[0]}")

    rows = []
    report = {
        "label": args.label,
        "scenario": args.scenario,
        "warmup_edit": args.warmup_edit,
        "command": command,
        "run_id": run_id,
        "utc_started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "environment": {
            key: value
            for key, value in os.environ.items()
            if key.startswith("CARGO_PROFILE_") or key in RECORDED_ENV
        },
        "source_sha256": hashlib.sha256(original).hexdigest() if original else None,
        "samples": rows,
    }
    report_path = output / f"{args.label}-{args.scenario}-{run_id}.json"
    try:
        for iteration in range(-int(not args.no_warmup), args.repeat):
            if path and (iteration >= 0 or args.warmup_edit):
                assert probe is not None
                if path.read_bytes() != expected:
                    raise RuntimeError(
                        f"{path} changed externally; stopping without overwriting it"
                    )
                needle = probe[1]
                suffix = f" build probe {run_id}-{iteration}".encode()
                replacement = (
                    b"Shadowroom" + suffix + b": {error:#}"
                    if args.scenario == "desktop-edit"
                    else needle + suffix
                )
                expected = original.replace(needle, replacement)
                path.write_bytes(expected)

            stem = output / f"{args.label}-{args.scenario}-{run_id}-{iteration}"
            timing_path = stem.with_suffix(".time")
            before = other_builds(root)
            started = time.perf_counter()
            with stem.with_suffix(".log").open("wb") as log:
                result = subprocess.run(
                    [
                        "/usr/bin/time",
                        "-f",
                        "%e %U %S %M",
                        "-o",
                        str(timing_path),
                        *command,
                    ],
                    cwd=root,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    check=False,
                )
            wall = time.perf_counter() - started
            timing = timing_path.read_text().splitlines()[-1].split()
            row = {
                "iteration": iteration,
                "warmup": iteration < 0,
                "wall_seconds": round(wall, 4),
                "user_seconds": float(timing[1]),
                "system_seconds": float(timing[2]),
                "max_rss_kib": int(timing[3]),
                "exit_code": result.returncode,
                "other_builds_before": before,
                "other_builds_after": other_builds(root),
                "log": stem.with_suffix(".log").name,
            }
            rows.append(row)
            report_path.write_text(json.dumps(report, indent=2) + "\n")
            print(
                json.dumps({"label": args.label, "scenario": args.scenario, **row}),
                flush=True,
            )
            if result.returncode:
                print(
                    "\n".join(
                        line[:1000]
                        for line in stem.with_suffix(".log")
                        .read_text()
                        .splitlines()[-20:]
                    ),
                    flush=True,
                )
                return result.returncode
    finally:
        if path and path.read_bytes() == expected and expected != original:
            path.write_bytes(original)
        elif path and path.read_bytes() != original:
            print(
                f"Source changed externally: original SHA256 {hashlib.sha256(original).hexdigest()}; inspect {path}",
                flush=True,
            )

    samples = [row["wall_seconds"] for row in rows if not row["warmup"]]
    report["median_wall_seconds"] = statistics.median(samples)
    report["min_wall_seconds"] = min(samples)
    report["max_wall_seconds"] = max(samples)
    report["timing_notes"] = (
        "Wall time covers the command, including waits. CPU/RSS figures cover /usr/bin/time descendants, not retained daemons; maximum RSS is not aggregate system memory. Other compiler processes are recorded at sample boundaries; retained idle daemons may also be listed."
    )
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "report": str(report_path),
                "median_wall_seconds": report["median_wall_seconds"],
            }
        ),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
