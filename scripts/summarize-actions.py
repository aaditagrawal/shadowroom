#!/usr/bin/env python3
"""Summarize exported GitHub Actions run timelines without exposing runner logs.

Export each input with gh run view --json attempt,databaseId,url,headSha,status,
conclusion,createdAt,jobs, then pass LABEL=FILE arguments in measurement order.
"""

import argparse
import json
from datetime import datetime
from itertools import pairwise
from pathlib import Path


def timestamp(value):
    if not value or value.startswith("0001-"):
        return None
    return datetime.fromisoformat(value)


def elapsed(start, end):
    first, last = timestamp(start), timestamp(end)
    return max(0.0, (last - first).total_seconds()) if first and last else None


def summarize(label, run):
    jobs = []
    for job in run["jobs"]:
        steps = [
            {
                "name": step["name"],
                "conclusion": step["conclusion"],
                "seconds": elapsed(step["startedAt"], step["completedAt"]),
            }
            for step in job.get("steps", [])
            if timestamp(step.get("startedAt"))
        ]
        jobs.append(
            {
                "name": job["name"],
                "conclusion": job["conclusion"],
                "seconds": elapsed(job["startedAt"], job["completedAt"]),
                "steps": steps,
            }
        )
    started = [timestamp(job["startedAt"]) for job in run["jobs"]]
    completed = [timestamp(job["completedAt"]) for job in run["jobs"]]
    started, completed = list(filter(None, started)), list(filter(None, completed))
    return {
        "label": label,
        "run_id": run["databaseId"],
        "attempt": run.get("attempt", 1),
        "url": run.get("url"),
        "head_sha": run["headSha"],
        "status": run["status"],
        "conclusion": run["conclusion"],
        "runner_elapsed_seconds": (max(completed) - min(started)).total_seconds()
        if started and completed
        else None,
        "summed_job_seconds": sum(job["seconds"] or 0 for job in jobs),
        "jobs": jobs,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", nargs="+", metavar="LABEL=FILE")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    runs = []
    for entry in args.runs:
        label, separator, filename = entry.partition("=")
        if not label or not separator or not filename:
            parser.error("Each input must be LABEL=FILE")
        run = json.loads(Path(filename).read_text())
        if run["status"] != "completed":
            parser.error(f"Run {label} is still active")
        runs.append(summarize(label, run))
    comparisons = []
    successful = [run for run in runs if run["conclusion"] == "success"]
    for before, after in pairwise(successful):
        if before["head_sha"] != after["head_sha"]:
            continue
        first, last = before["runner_elapsed_seconds"], after["runner_elapsed_seconds"]
        if first and last is not None:
            comparisons.append(
                {
                    "before": before["label"],
                    "after": after["label"],
                    "elapsed_reduction_percent": round(100 * (first - last) / first, 2),
                    "seconds_saved": first - last,
                }
            )
    report = {
        "schema": 1,
        "method": "GitHub Actions completed job and step timestamps exported by gh run view",
        "limitations": [
            "Runner elapsed time spans the first job start to the last job completion, including dependency waits. Summed job seconds counts parallel jobs separately.",
            "These are wall-clock workflow measurements, not CPU execution times, application startup or photo-review benchmarks.",
            "Cache transfer, network, hosted runner variation and scheduling can affect results; a rerun is not an isolated cache microbenchmark.",
            "Failed historical runs document the original failures and are excluded from speed comparisons. Same-commit successful runs are compared; cache warmth must be established separately from their logs.",
        ],
        "runs": runs,
        "same_commit_successful_comparisons": comparisons,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(args.output)


if __name__ == "__main__":
    main()
