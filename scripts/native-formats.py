#!/usr/bin/env python3
"""Exercise mixed image formats through the running native app and its MCP."""
import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import time
import traceback

spec = importlib.util.spec_from_file_location("extended", pathlib.Path(__file__).with_name("native-extended.py"))
ext = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ext)
s = ext.smoke


def screenshot(run, env, target):
    tool = s.xdotool_path(None)
    if not tool or not shutil.which("import"):
        return None
    tool_env = env.copy()
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tool_env["LD_LIBRARY_PATH"] = str(libraries)
    windows = subprocess.run([str(tool), "search", "--all", "--pid", str(run.process.pid), "--name", "^Shadowroom$"],
                             env=tool_env, capture_output=True, text=True, timeout=3, check=True).stdout.splitlines()
    window = next(w for w in windows if w.isdigit())
    owner = subprocess.run([str(tool), "getwindowpid", window], env=tool_env, capture_output=True,
                           text=True, timeout=3, check=True).stdout.strip()
    s.require(owner == str(run.process.pid), "Refusing to capture another process")
    subprocess.run(["import", "-window", window, str(target)], env=env, capture_output=True, timeout=5, check=True)
    return str(target)


def exercise(args, root, report):
    binary, fixtures = pathlib.Path(args.binary).resolve(), pathlib.Path(args.fixtures).resolve()
    source = root / "source"
    source.mkdir()
    wanted = json.loads((fixtures / "expected.json").read_text())
    for name in wanted:
        shutil.copyfile(fixtures / name, source / name)
    originals = {p.name: (s.digest(p), p.stat().st_mtime_ns) for p in source.iterdir()}
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update({f"XDG_{key.upper()}_HOME": str(root / key) for key in ("config", "data", "cache", "state")})
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(json.dumps({"auto_discover_drives": False,
        "auto_advance": False, "memory_budget_mb": 2048, "grouping_mode": "off", "view_mode": "loupe"}))
    report.update(binary=str(binary), binary_sha256=s.digest(binary), backend="native X11", host=os.uname().nodename)
    run = None
    try:
        run = ext.Run(binary, env, root, 1)
        c = run.client
        c.call("import", {"path": str(source)})
        s.wait_for(lambda: run.state()["total"] == len(wanted) and not run.state()["indexing"], 30, "mixed format indexing")
        photos = c.call("library_list", {"scope": "all", "limit": 200})["photos"]
        s.require(len(photos) == len(wanted), "Unexpected format catalog size")
        c.call("view", {"mode": "loupe"})
        c.call("zoom", {"mode": "full"})
        for p in photos:
            name, expected = p["filename"], wanted[p["filename"]]
            if "catalog" in expected:
                s.require([p["width"], p["height"], p["orientation"]] == expected["catalog"], "Indexed geometry wrong: " + name)
            start = time.monotonic()
            c.call("select", {"id": p["id"]})

            def decoded():
                value = c.call("photo_get", {"id": p["id"]})
                s.require(value["full_error"] is None, "Full decode failed: " + name + " " + str(value["full_error"]))
                preview = value["preview"]
                return preview if preview and preview["kind"] == "Full" else None

            preview = s.wait_for(decoded, 60, "native Full " + name)
            elapsed = (time.monotonic() - start) * 1000
            s.require([preview["width"], preview["height"]] == expected["display"], "Full geometry wrong: " + name)
            s.require([preview["source_width"], preview["source_height"]] == expected["display"], "Source geometry wrong: " + name)
            s.require(preview["rendered_raw"] == expected.get("raw", False), "Sensor provenance wrong: " + name)
            s.require(not preview["embedded_raw"], "Full was an embedded preview: " + name)
            if expected.get("note"):
                s.require(expected["note"] in (preview["rendering_note"] or ""), "Source limitation not exposed: " + name)
            stats = c.call("cache_stats")
            s.require(stats["memory_bytes"] <= stats["memory_budget_bytes"], "Media budget exceeded")
            report["checks"].append(dict(name="native Full " + name, result="PASS", full_ms=elapsed, preview=preview))
            print(f"PASS {name} Full {elapsed:.1f}ms", flush=True)
            if expected.get("screenshot"):
                c.call("zoom", {"mode": "fit"})
                time.sleep(.3)
                report.setdefault("screenshots", []).append(screenshot(run, env, root / (name + ".png")))
                c.call("zoom", {"mode": "full"})
        s.require(c.call("cache_stats")["errors"] == 0, "Unexpected decode errors")
        ids = [p["id"] for p in photos]
        c.call("rate", {"rating": 5, "ids": ids})
        s.wait_for(lambda: run.state()["persistence"]["state"] == "durable", 15, "durable mixed-format ratings")
        c.call("quit")
        run.finish()
        run = ext.Run(binary, env, root, 2)
        s.wait_for(lambda: run.state()["total"] == len(wanted), 20, "mixed format restart")
        resumed = run.client.call("library_list", {"scope": "all", "limit": 200})["photos"]
        s.require(all(p["rating"] == 5 for p in resumed), "Mixed-format decisions lost on restart")
        run.client.call("quit")
        run.finish()
        for name, old in originals.items():
            p = source / name
            s.require((s.digest(p), p.stat().st_mtime_ns) == old, "Original changed: " + name)
        report["checks"].append(dict(name="ratings survive restart; original bytes/mtimes intact", result="PASS"))
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
