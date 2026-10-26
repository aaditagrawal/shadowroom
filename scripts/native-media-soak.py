#!/usr/bin/env python3
"""Repeated native Full HEIF previews and orderly exits, with a private profile."""
import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import tempfile
import time
import traceback

spec = importlib.util.spec_from_file_location("extended", pathlib.Path(__file__).with_name("native-extended.py"))
ext = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ext)
s = ext.smoke


def exercise(args, root, report):
    binary = pathlib.Path(args.binary).resolve()
    fixtures = pathlib.Path(args.fixtures).resolve()
    source = root / "source"
    source.mkdir()
    for p in fixtures.iterdir():
        if p.is_file() and p.suffix.lower() in (".heic", ".heif", ".jpg", ".bmp"):
            shutil.copyfile(p, source / p.name)
    expected = {p.name: (s.digest(p), p.stat().st_mtime_ns) for p in source.iterdir()}
    s.require(expected, "Existing positive SDR fixtures required")
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update({f"XDG_{key.upper()}_HOME": str(root / key) for key in ("config", "data", "cache", "state")})
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(json.dumps({"auto_discover_drives": False,
        "auto_advance": False, "memory_budget_mb": 128, "grouping_mode": "off", "view_mode": "loupe"}))
    report.update(binary=str(binary), binary_sha256=s.digest(binary), fixtures=list(expected), rounds=args.rounds)
    for n in range(1, args.rounds+1):
        run = None
        try:
            run = ext.Run(binary, env, root, n)
            c = run.client
            if n == 1:
                c.call("import", {"path": str(source)})
            s.wait_for(lambda: run.state()["total"] == len(expected) and not run.state()["indexing"], 20, "complete media catalog")
            photos = c.call("library_list", {"scope": "all", "limit": 100})["photos"]
            c.call("view", {"mode": "loupe"})
            c.call("zoom", {"mode": "full"})
            for p in photos:
                c.call("select", {"id": p["id"]})
                # Ensure the render loop had an opportunity to enqueue the new
                # selection before interpreting an idle worker snapshot.
                time.sleep(.12)
                stats = s.wait_for(lambda: (lambda v: v if v["active"] == 0 and v["queued"] == 0 else None)(c.call("cache_stats")),
                                   15, "Full decode completion")
                s.require(stats["errors"] == 0, "Media decode failed: " + repr(stats))
                s.require(stats["memory_bytes"] <= stats["memory_budget_bytes"], "Cache exceeded byte budget")
            s.require(stats["decoded"] >= len(photos), "Full previews were not decoded")
            c.call("quit")
            run.finish()
            report["checks"].append(dict(name=f"native Full decode and clean exit {n}", result="PASS", cache=stats))
            print(f"PASS native media round {n}", flush=True)
        finally:
            if run:
                run.abort()
    for name, value in expected.items():
        p = source / name
        s.require((s.digest(p), p.stat().st_mtime_ns) == value, "Source changed")
    report["result"] = "PASS"


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True)
    p.add_argument("--fixtures", required=True)
    p.add_argument("--rounds", type=int, default=10)
    p.add_argument("--output")
    args = p.parse_args()
    s.require(1 <= args.rounds <= 50, "Rounds must be 1..50")
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-media-soak-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = dict(result="RUNNING", checks=[])
    start = time.monotonic()
    try:
        exercise(args, root, report)
    except Exception as error:
        report.update(result="FAIL", failure=str(error))
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), flush=True)
    report["elapsed_seconds"] = time.monotonic()-start
    (root / "report.json").write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps(dict(result=report["result"], report=str(root / "report.json"), seconds=report["elapsed_seconds"])), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
