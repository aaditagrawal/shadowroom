#!/usr/bin/env python3
"""Inject ingest failures into a private catalog and verify native recovery/quit."""
import argparse
import importlib.util
import json
import os
import pathlib
import sqlite3
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
    card, library = root / "card", root / "library"
    card.mkdir()
    s.bmp(card / "frame.bmp", 256, 160, 1)
    original = s.digest(card / "frame.bmp")
    options = dict(library_root=str(library), backup_roots=[], folder_template="", file_template="{original}")
    run = None

    def record(name, **detail):
        report["checks"].append(dict(name=name, result="PASS", **detail))
        print("PASS " + name, flush=True)

    try:
        run = ext.Run(binary, env, root, 1)
        c = run.client

        def status():
            return c.call("ingest_status")

        def scan():
            c.call("ingest_scan", {"path": str(card)})
            s.wait_for(lambda: not status()["scanning"] and status()["total"] == 1, 12, "source scan")

        def plan():
            c.call("ingest_plan", {"options": options})
            return s.wait_for(lambda: status()["plan"], 12, "ready plan")

        scan()
        s.require(plan()["ready"], "Initial plan invalid")
        c.call("ingest_scan", {"path": str(root / "nonexistent-source")})
        s.wait_for(lambda: not status()["scanning"] and status()["error"], 10, "failed replacement scan")
        c.call("ingest_plan", {"options": options})
        invalid = s.wait_for(lambda: status() if status()["error"] else None, 10, "no current scan error")
        s.require(invalid["plan"] is None, "Failed scan retained previous ready plan")
        c.call("ingest_start", error=True)
        s.require(not library.exists(), "Failed scan imported old source")
        record("failed source replacement invalidates old scan and plan")

        scan()
        s.require(plan()["ready"], "Replacement source plan invalid")
        c.call("ingest_plan", {"options": dict(options, file_template="{unknown_token}")})
        s.wait_for(lambda: status()["error"], 10, "invalid replacement template")
        s.require(status()["plan"] is None, "Invalid template retained previous plan")
        c.call("ingest_start", error=True)
        s.require(not library.exists(), "Invalid template imported old plan")
        record("invalid replacement template cannot start previous plan")

        s.require(plan()["ready"], "Valid recovery plan refused")
        database = next((root / "data").rglob("catalog.sqlite3"))
        with sqlite3.connect(database) as db:
            db.execute("CREATE TRIGGER test_fail_ingest_finish BEFORE UPDATE ON ingest_jobs BEGIN SELECT RAISE(ABORT, 'injected final history failure'); END")
        try:
            c.call("ingest_start")
            failed = s.wait_for(lambda: status() if "injected final history failure" in str(status()["error"]) else None,
                                15, "injected final history error")
            s.require(failed["progress"]["phase"] == "failed", "Ended import remained active: " + repr(failed["progress"]))
            s.require(failed["progress"]["completed"] == 1 and failed["progress"]["job_id"], "Failure lost committed count/job identity")
            manifest = json.loads(pathlib.Path(failed["progress"]["manifest"]).read_text())
            for key in ("job_id", "phase", "completed", "skipped", "failed", "files", "photos", "videos", "bytes", "errors"):
                s.require(manifest["progress"][key] == failed["progress"][key], "Durable manifest disagrees on " + key)
            s.require(s.digest(library / "frame.bmp") == original, "Late history error damaged committed file")
            s.wait_for(lambda: run.state()["total"] == 1, 10, "committed catalog visible after late failure")
        finally:
            with sqlite3.connect(database) as db:
                db.execute("DROP TRIGGER test_fail_ingest_finish")
        record("late history-write failure reports terminal job identity/counts and publishes committed photos", progress=failed["progress"])
        c.call("quit")
        run.finish()
        record("normal quit completes after failed import")
        run = ext.Run(binary, env, root, 2)
        c = run.client
        s.wait_for(lambda: run.state()["total"] == 1, 10, "committed photo after restart")
        photo = c.call("library_list", {"scope": "all"})["photos"][0]
        s.require(photo["path"] == str(library / "frame.bmp"), "Committed photo lost on restart")
        c.call("ingest_history", {"limit": 20})
        history = s.wait_for(lambda: c.call("ingest_status")["history"] if c.call("ingest_status")["history"].get("jobs") else None,
                             10, "reconciled job history")
        job = next(j for j in history["jobs"] if j["id"] == failed["progress"]["job_id"])
        s.require(job["verification"] == "failed" and job["counts"]["completed"] == 1, "Abandoned job history still claims running")
        c.call("quit")
        run.finish()
        record("restart retains committed group and source bytes")
        s.require(s.digest(card / "frame.bmp") == original, "Source changed")
        report["result"] = "PASS"
    finally:
        if run:
            run.abort()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True)
    p.add_argument("--output")
    args = p.parse_args()
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-ingest-failures-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    binary = pathlib.Path(args.binary).resolve()
    report = dict(result="RUNNING", session=str(root), binary=str(binary), binary_sha256=s.digest(binary), checks=[])
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
