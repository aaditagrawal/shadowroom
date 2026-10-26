#!/usr/bin/env python3
"""Native format rejection and recoverable RAW-memory admission checks."""
import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import time
import traceback

spec = importlib.util.spec_from_file_location("extended", pathlib.Path(__file__).with_name("native-extended.py"))
ext = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ext)
s = ext.smoke


def exercise(args, root, report):
    binary, fixtures = pathlib.Path(args.binary).resolve(), pathlib.Path(args.fixtures).resolve()
    source = root / "source"
    source.mkdir()
    cases = [
        (fixtures / "jxl-fixtures/jxl/pq_gradient.jxl", "hdr.jxl", "HDR"),
        (fixtures / "jxl-fixtures/generated/premultiplied.jxl", "associated.jxl", "associated-alpha"),
        (fixtures / "heif-fixtures/upstream-with-alpha-prem-marked.heic", "associated.heic", "Premultiplied-alpha"),
        (fixtures / "RAW_CANON_5DMARK2_PREPROD.CR2", "canon.CR2", "memory"),
    ]
    for p, name, _ in cases:
        shutil.copyfile(p, source / name)
    originals = {p.name: (s.digest(p), p.stat().st_mtime_ns) for p in source.iterdir()}
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update({f"XDG_{k.upper()}_HOME": str(root / k) for k in ("config", "data", "cache", "state")})
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(json.dumps({"auto_discover_drives": False,
        "auto_advance": False, "memory_budget_mb": 128, "grouping_mode": "off", "view_mode": "loupe"}))
    report.update(binary=str(binary), binary_sha256=s.digest(binary))
    run = None
    try:
        run = ext.Run(binary, env, root, 1)
        c = run.client
        c.call("import", {"path": str(source)})
        s.wait_for(lambda: run.state()["total"] == 4 and not run.state()["indexing"], 20, "error fixtures indexed")
        photos = {p["filename"]: p for p in c.call("library_list", {"scope": "all", "limit": 20})["photos"]}
        c.call("view", {"mode": "loupe"})
        c.call("zoom", {"mode": "full"})
        c.call("command", {"name": "toggle_inspector"})
        for _, name, message in cases:
            c.call("select", {"id": photos[name]["id"]})
            def failed():
                p = c.call("photo_get", {"id": photos[name]["id"]})
                return p if p["full_error"] else None
            photo = s.wait_for(failed, 30, "explicit Full error " + name)
            s.require(message in photo["full_error"], "Unexpected Full error: " + repr(photo))
            s.require(photo["preview"] is None or photo["preview"]["kind"] != "Full", "Failed Full falsely reports Full quality")
            report["checks"].append(dict(name="explicit rejection " + name, result="PASS", full_error=photo["full_error"]))
        c.call("settings_set", {"settings": {"memory_budget_mb": 2048}})
        s.wait_for(lambda: c.call("cache_stats")["memory_budget_bytes"] == 2048*1024*1024, 10, "new decode memory budget")
        c.call("command", {"name": "retry_preview"})
        def recovered():
            p = c.call("photo_get", {"id": photos["canon.CR2"]["id"]})
            v = p["preview"]
            return v if v and v["kind"] == "Full" and v["rendered_raw"] and not p["full_error"] else None
        preview = s.wait_for(recovered, 60, "recoverable Full sensor render")
        s.require([preview["width"], preview["height"]] == [5634, 3752], "Recovered sensor geometry incorrect")
        report["checks"].append(dict(name="128 MiB refusal recovers after raising budget to 2 GiB", result="PASS", preview=preview))
        c.call("quit")
        run.finish()
        for name, old in originals.items():
            p = source / name
            s.require((s.digest(p), p.stat().st_mtime_ns) == old, "Source changed")
        report["result"] = "PASS"
    finally:
        if run:
            run.abort()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--fixtures", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    root = pathlib.Path(args.output).resolve()
    root.mkdir(parents=True, exist_ok=False)
    report = dict(result="RUNNING", checks=[])
    start = time.monotonic()
    try:
        exercise(args, root, report)
    except Exception as e:
        report.update(result="FAIL", failure=str(e))
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(e), flush=True)
    report["elapsed_seconds"] = time.monotonic() - start
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(dict(result=report["result"], report=str(root / "report.json"))), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
