#!/usr/bin/env python3
"""Publish portable native-benchmark measurements without private session paths."""

import argparse
import json
import math
import platform
import statistics
from pathlib import Path


def summarize(values):
    ordered = sorted(values)
    return {
        "samples": len(values),
        "p50_ms": statistics.median(values),
        "p95_ms": ordered[math.ceil(len(ordered) * 0.95) - 1],
        "max_ms": max(values),
        "all_ms": values,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reports", nargs="+", type=Path)
    parser.add_argument(
        "--output", type=Path, default=Path("docs/benchmarks/release.json")
    )
    args = parser.parse_args()
    reports = [json.loads(path.read_text()) for path in args.reports]
    first = reports[0]
    environment_keys = [
        "cpu",
        "logical_cpus",
        "binary_bytes",
        "binary_sha256",
        "build",
        "backend",
        "memory_budget_mib",
        "disk_budget_mib",
        "grouping",
        "concurrent_work",
    ]
    for report in reports:
        if (
            report["result"] != "PASS"
            or not report["originals_unchanged"]
            or not report["normal_exit"]
        ):
            raise ValueError("Only successful, cleanly closed runs can be published")
        for key in environment_keys:
            if report["environment"][key] != first["environment"][key]:
                raise ValueError(f"Runs have different environments: {key}")
        for key in [
            "paths",
            "dimensions",
            "distinct_pixel_buffers",
            "format",
            "storage",
        ]:
            if report["fixture"][key] != first["fixture"][key]:
                raise ValueError(f"Runs have different fixtures: {key}")
    runs = []
    for index, report in enumerate(reports, 1):
        trace = report["memory_timeline"]
        runs.append(
            {
                "run": index,
                "started_utc": report["started_utc"],
                "result": report["result"],
                "startup_seconds": report["startup_seconds"],
                "import_seconds": report["import"]["end_to_end_seconds"],
                "elapsed_seconds": report["elapsed_seconds"],
                "load_average_at_start": report["environment"]["load_average_at_start"],
                "peak_rss_mib": trace["kernel_hwm_kib"] / 1024,
                "warm_reverse_source_reads": report["navigation"][
                    "warm_reverse_source_reads_delta"
                ],
                "warm_reverse_decodes": report["navigation"][
                    "warm_reverse_decodes_delta"
                ],
                "originals_unchanged": report["originals_unchanged"],
                "normal_exit": report["normal_exit"],
                "latency": report["latency"],
                "memory_trace": {
                    "interval_seconds": trace["interval_seconds"],
                    "samples": [
                        {
                            "seconds": sample["seconds"],
                            "VmRSS_kib": sample["VmRSS_kib"],
                            "VmHWM_kib": sample["VmHWM_kib"],
                        }
                        for sample in trace["samples"]
                    ],
                },
            }
        )
    output = {
        "schema_version": 1,
        "workload": "Synthetic 10k-path native desktop workload; OS-page-cache warm; new catalog per run",
        "environment": {
            **{key: first["environment"][key] for key in environment_keys},
            "os": platform.freedesktop_os_release()["PRETTY_NAME"],
            "kernel": platform.release(),
        },
        "fixture": first["fixture"],
        "runs": runs,
        "latency": {
            key: summarize(
                [value for run in runs for value in run["latency"][key]["all_ms"]]
            )
            for key in first["latency"]
        },
        "startup_median_seconds": statistics.median(
            run["startup_seconds"] for run in runs
        ),
        "import_median_seconds": statistics.median(
            run["import_seconds"] for run in runs
        ),
        "peak_rss_mib": max(run["peak_rss_mib"] for run in runs),
        "memory_trace": runs[len(runs) // 2]["memory_trace"],
        "measurement_notes": [
            "Latency is MCP request-to-response, including JSON-RPC/local bridge transport and Python harness overhead.",
            "A rating response acknowledges acceptance; durable FULL-synchronized SQLite commit follows asynchronously.",
            "Startup ends after private endpoint readiness and MCP handshake; it is not first-frame presentation.",
            "The memory trace begins after readiness, samples process RSS every 100 ms, and does not measure GPU memory.",
            "The fixture uses 16 hardlinked 128x80 BMP buffers; this is not camera-card, RAW, network, or uncached storage throughput.",
            "The suite includes deliberate SQLite writer contention and settle waits; wall duration is not CPU time.",
        ],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    print(args.output)


if __name__ == "__main__":
    main()
