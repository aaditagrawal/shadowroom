#!/usr/bin/env python3
"""Pool native-responsiveness.py reports by label into portable JSON and tables.

    uv run --no-project python scripts/summarize-responsiveness.py \
        --output docs/benchmarks/hi-responsiveness.json /tmp/run-*/report.json

Reports with the same `--label` are pooled: every input sample from every run
contributes to that configuration's percentiles (nearest rank). The JSON keeps
each run's environment, binary hash, scenario tables and idle measurements, and
per-input latency columns, without private session paths. Python stdlib only.
"""
import argparse
import json
import math
import statistics
from pathlib import Path

# Per-input columns kept in the portable JSON (null when not applicable).
COLUMNS = ("deliver_ms", "input_to_present_ms", "input_to_damage_ms", "cpu_ms", "input_to_sharp_ms")
SAMPLE_KEYS = ("deliver_ms", "wait_ms", "cpu_ms", "frame_ms", "input_to_present_ms", "input_to_damage_ms",
               "input_to_last_frame_ms", "input_to_sharp_ms")


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, math.ceil(len(ordered) * fraction) - 1))]


def summary(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return {"n": len(values), "p50": round(statistics.median(values), 2),
            "p95": round(percentile(values, .95), 2), "max": round(max(values), 2)}


def pool(reports):
    scenarios = {}
    for report in reports:
        for sample in report["samples"]:
            scenarios.setdefault(sample["scenario"], []).append(sample)
    out = {}
    for name, samples in scenarios.items():
        good = [s for s in samples if "input_to_present_ms" in s]
        tables = [r["scenarios"][name] for r in reports if name in r["scenarios"]]
        out[name] = {
            "inputs": len(samples),
            "inputs_without_new_frame": sum(1 for s in samples if s.get("no_frame")),
            "inputs_unmatched": sum(1 for s in samples if s.get("missing")),
            **{key: summary([s.get(key) for s in good]) for key in SAMPLE_KEYS},
            "first_frame_sharp": sum(1 for s in good if s.get("first_frame_photo") in ("fit", "full")),
            "frames": sum(t["all_frames"] for t in tables),
            "frames_over_one_interval": sum(t["frames_over_one_interval"] for t in tables),
            "dispatch_draws": sum(t.get("dispatch_draws", 0) for t in tables),
            "frame_cpu_ms_p50_by_run": [t["all_frames_cpu_ms"]["p50"] for t in tables if t.get("all_frames_cpu_ms")],
            "region_cpu_ms_mean_by_run": [t.get("region_cpu_ms_mean", {}) for t in tables],
        }
    idle = {}
    for report in reports:
        for row in report["idle"]:
            idle.setdefault(row["scenario"], []).append(row)
    idle_out = {name: {
        "runs": len(rows),
        "cpu_percent_mean": round(statistics.fmean(r["cpu_percent"] for r in rows), 2),
        "main_thread_wakeups_per_s_mean": round(statistics.fmean(r["main_thread_wakeups_per_s"] for r in rows), 1),
        "frames_per_s_mean": round(statistics.fmean(r["frames_per_s"] for r in rows), 2),
    } for name, rows in idle.items()}
    return out, idle_out


def portable_run(report):
    """One run without private paths; samples as per-scenario columns (0.1 ms)."""
    keep = ("label", "binary_sha256", "started", "startup_to_indexed_s", "originals_intact", "environment",
            "refresh_hz", "interval_ms", "frame_count", "dispatch_draw_count", "damage_events")
    run = {key: report.get(key) for key in keep}
    run["idle"] = [{k: v for k, v in row.items() if k not in ("start", "end")} for row in report["idle"]]
    columns = {}
    for sample in report["samples"]:
        scenario = columns.setdefault(sample["scenario"], {key: [] for key in COLUMNS})
        for key in COLUMNS:
            value = sample.get(key)
            scenario[key].append(round(value, 1) if isinstance(value, float) else value)
    run["samples"] = columns
    run["frames"] = {name: {k: table.get(k) for k in ("all_frames", "frames_over_one_interval", "dispatch_draws",
                                                       "all_frames_cpu_ms", "region_cpu_ms_mean")}
                     for name, table in report["scenarios"].items()}
    return run


def fmt(s):
    return f"{s['p50']:.1f} / {s['p95']:.1f} / {s['max']:.1f}" if s else "–"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("reports", nargs="+")
    parser.add_argument("--output", help="portable JSON to write")
    args = parser.parse_args()
    reports = [json.loads(Path(p).read_text()) for p in args.reports]
    labels = []
    for report in reports:
        if report.get("label") not in labels:
            labels.append(report.get("label"))
    configurations = {}
    for label in labels:
        group = [r for r in reports if r.get("label") == label]
        scenarios, idle = pool(group)
        configurations[label] = {"runs": len(group), "scenarios": scenarios, "idle": idle,
                                 "binary_sha256": sorted({r["binary_sha256"] for r in group})}
        print(f"\n### {label} ({len(group)} runs)\n")
        print("| Interaction | Inputs | Input → present p50 / p95 / max ms | Input → X damage p50 ms "
              "| Frame CPU p50 / p95 ms | Frames > 1 interval |")
        print("| --- | ---: | ---: | ---: | ---: | ---: |")
        for name, row in scenarios.items():
            damage = row["input_to_damage_ms"]
            cpu = row["cpu_ms"]
            print(f"| {name} | {row['inputs']} | {fmt(row['input_to_present_ms'])} | "
                  f"{damage['p50']:.1f} | {cpu['p50']:.1f} / {cpu['p95']:.1f} | "
                  f"{row['frames_over_one_interval']} of {row['frames']} |" if damage and cpu else
                  f"| {name} | {row['inputs']} | {fmt(row['input_to_present_ms'])} | – | – | – |")
        print("\n| Idle state | CPU % | Main-thread wakeups/s | Frames/s |")
        print("| --- | ---: | ---: | ---: |")
        for name, row in idle.items():
            print(f"| {name} | {row['cpu_percent_mean']} | {row['main_thread_wakeups_per_s_mean']} | "
                  f"{row['frames_per_s_mean']} |")
    if args.output:
        out = {"schema_version": 1, "workload": "native-responsiveness", "configurations": configurations,
               "runs": [portable_run(r) for r in reports]}
        Path(args.output).write_text(json.dumps(out, separators=(",", ":")) + "\n")
        print(f"\nWrote {args.output}")


if __name__ == "__main__":
    main()
