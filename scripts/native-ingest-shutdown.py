#!/usr/bin/env python3
"""Native close during running/paused imports, on isolated sparse-video fixtures."""
import argparse
import importlib.util
import json
import os
import pathlib
import tempfile
import time
import traceback

spec = importlib.util.spec_from_file_location("extended", pathlib.Path(__file__).with_name("native-extended.py"))
ext = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ext)
s = ext.smoke


def exercise(binary, root, report):
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update({f"XDG_{key.upper()}_HOME": str(root / key) for key in ("config", "data", "cache", "state")})
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(json.dumps({"auto_discover_drives": False, "auto_advance": False}))
    source = root / "source"
    source.mkdir()
    video = source / "clip.mov"
    with video.open("wb") as f:
        f.truncate(256 * 1024 * 1024)
    original = (s.digest(video), video.stat().st_mtime_ns)
    for number, paused in enumerate((False, True), 1):
        run = None
        try:
            run = ext.Run(binary, env, root, number)
            c = run.client

            def status():
                return c.call("ingest_status")

            c.call("ingest_scan", {"path": str(source)})
            s.wait_for(lambda: not status()["scanning"] and status()["total"] == 1, 15, "video scan")
            library, backup = root / f"library-{number}", root / f"backup-{number}"
            c.call("ingest_plan", {"options": {"library_root": str(library), "backup_roots": [str(backup)],
                "folder_template": "", "file_template": "{original}", "include_videos": True, "selected_ids": [0]}})
            s.wait_for(lambda: status()["plan"] and status()["plan"]["ready"], 15, "video plan")
            c.call("ingest_start")
            c.call("ingest_pause")
            s.wait_for(lambda: status()["progress"]["phase"] == "paused", 8, "paused job")
            if not paused:
                c.call("ingest_resume")
                s.wait_for(lambda: status()["progress"]["phase"] in ("copying", "verifying"), 8, "resumed job")
            started = time.monotonic()
            if paused:
                c.call("quit")
                close_method = "MCP quit"
            else:
                tool = s.xdotool_path(None)
                s.require(tool is not None, "xdotool required for owned native window-close check")
                close_method = s.close_owned_window(tool, run.process, env)
            run.finish()
            manifests = list(library.glob("shadowroom-ingest-*.json"))
            s.require(len(manifests) == 1, "Terminal import manifest missing")
            manifest = json.loads(manifests[0].read_text())
            phase = manifest["progress"]["phase"]
            if paused:
                s.require(phase == "cancelled", "Paused quit did not cancel: " + phase)
                s.require(not (library / "clip.mov").exists() and not (backup / "clip.mov").exists(), "Cancelled partial group published")
            else:
                s.require(phase == "completed" and manifest["progress"]["completed"] == 1, "Native close interrupted copy")
                for dest in (library, backup):
                    s.require(s.digest(dest / "clip.mov") == original[0], "Close left truncated/corrupt destination")
            s.require(not [p for p in root.rglob("*") if ".shadowroom-part-" in p.name], "Staging file left after close")
            item = dict(name="paused import quit cancels" if paused else "WM close waits for running verified import",
                        result="PASS", seconds=time.monotonic()-started, phase=phase, close=close_method)
            report["checks"].append(item)
            print("PASS " + item["name"], flush=True)
        finally:
            if run:
                run.abort()
    s.require((s.digest(video), video.stat().st_mtime_ns) == original, "Source changed")
    report["result"] = "PASS"


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True)
    p.add_argument("--output")
    args = p.parse_args()
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-ingest-shutdown-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    binary = pathlib.Path(args.binary).resolve()
    report = dict(result="RUNNING", binary=str(binary), binary_sha256=s.digest(binary), checks=[])
    start = time.monotonic()
    try:
        exercise(binary, root, report)
    except Exception as error:
        report.update(result="FAIL", failure=str(error))
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), flush=True)
    report["elapsed_seconds"] = time.monotonic() - start
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(dict(result=report["result"], report=str(root / "report.json"), seconds=report["elapsed_seconds"])), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
