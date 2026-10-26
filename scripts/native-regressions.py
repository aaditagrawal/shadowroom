#!/usr/bin/env python3
"""Focused isolated Linux native/MCP regression checks; never contacts a shared session."""
import argparse
import base64
import importlib.util
import json
import os
import pathlib
import re
import shutil
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
import traceback

SPEC = importlib.util.spec_from_file_location("shadowroom_native_smoke", pathlib.Path(__file__).with_name("native-smoke.py"))
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)
require, wait_for = smoke.require, smoke.wait_for
JPEG = base64.b64decode("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")


class RegressionRun(smoke.NativeRun):
    """Reuse smoke process/MCP lifecycle, without its fixed six-photo readiness wait."""
    def __init__(self, binary, environment, root, fixtures, number, initial=False):
        self.stdout = (root / ("native-%d.stdout.log" % number)).open("wb")
        self.stderr = (root / ("native-%d.stderr.log" % number)).open("wb")
        self.process = subprocess.Popen([str(binary)] + (["--import", str(fixtures)] if initial else []),
                                        env=environment, stdout=self.stdout, stderr=self.stderr)
        manifest = root / "config/shadowroom/automation.json"
        def ready():
            require(self.process.poll() is None, "Own native process exited before readiness")
            try:
                descriptor = json.loads(manifest.read_text())
                return descriptor.get("pid") == self.process.pid
            except (FileNotFoundError, json.JSONDecodeError):
                return False
        wait_for(ready, 20, "own PID's private native endpoint")
        require(stat.S_IMODE(manifest.stat().st_mode) == 0o600, "Private descriptor is not 0600")
        self.client = smoke.MCP(binary, environment, root, number)
        self.client.initialize()


def synced_json(path, value):
    with path.open("w", encoding="utf-8") as output:
        json.dump(value, output)
        output.flush()
        os.fsync(output.fileno())
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def blake3(path, executable):
    if executable is None:
        return None
    result = subprocess.run([str(executable), str(path)], capture_output=True, text=True,
                            check=True, timeout=120)
    value = result.stdout.split()[0]
    require(re.fullmatch(r"[0-9a-f]{64}", value), "BLAKE3 tool returned an invalid digest")
    return value


def all_photos(run):
    return run.client.call("library_list", {"scope": "all", "limit": 100})["photos"]


def settled(run, count):
    return wait_for(lambda: (lambda state: state if state["total"] == count and not state["indexing"] else None)(run.state()),
                    25, "%d indexed native photos" % count)


def exercise(args, root, report):
    binary = pathlib.Path(args.binary).expanduser().resolve()
    require(binary.is_file() and os.access(binary, os.X_OK), "A built --binary executable is required")
    require(sys.platform.startswith("linux") and os.environ.get("DISPLAY"), "An actual Linux DISPLAY is required")
    originals = root / "originals"
    originals.mkdir()
    jpeg = originals / "promotion.jpg"
    jpeg.write_bytes(JPEG)
    for index in range(5):
        smoke.bmp(originals / ("fixture-%02d.bmp" % index), 256 + index * 16, 160 + index * 8, index)
    for index, path in enumerate(sorted(originals.iterdir())):
        os.utime(path, ns=(1700000000000000000 + index * 1000000000,) * 2)
    before = {str(path): {"sha256": smoke.digest(path), "mtime_ns": path.stat().st_mtime_ns} for path in originals.iterdir()}
    environment = os.environ.copy()
    environment.pop("WAYLAND_DISPLAY", None)
    environment.update(RUST_BACKTRACE="1", XDG_CONFIG_HOME=str(root / "config"),
                       XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
                       XDG_STATE_HOME=str(root / "state"))
    configuration = root / "config/shadowroom"
    configuration.mkdir(parents=True)
    synced_json(configuration / "settings.json", {"auto_discover_drives": False, "auto_advance": False,
        "memory_budget_mb": 128, "cache_budget_mb": 128, "grouping_mode": "off"})
    report.update(binary=str(binary), binary_mtime_ns=binary.stat().st_mtime_ns,
                  backend="native X11", display=environment["DISPLAY"], fixture_hashes=before)
    active = []
    extra_processes = []
    def launch(number, initial=False):
        run = RegressionRun.__new__(RegressionRun)
        active.append(run)
        RegressionRun.__init__(run, binary, environment, root, originals, number, initial)
        return run
    def record(name, **evidence):
        report["checks"].append({"name": name, "result": "PASS", **evidence})
        print("PASS " + name, flush=True)
    try:
        run = launch(1, True)
        settled(run, 6)
        photo = next(photo for photo in all_photos(run) if photo["path"] == str(jpeg))
        photo_id = photo["id"]
        database = next((root / "data").rglob("catalog.sqlite3"))
        run.client.call("select", {"id": photo_id})
        run.client.call("rate", {"ids": [photo_id], "rating": 4})
        run.durable()
        require(smoke.database_rating(database, photo_id) == 4, "Initial JPEG rating was not durable")
        # A bounded TIFF-like RAW container carrying the same valid 320x200 JPEG.
        raw = originals / "promotion.nef"
        tiff = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x01\0\0\0\0\0\0\0"
        raw.write_bytes(tiff + b"\0" * 1024 + JPEG)
        before[str(raw)] = {"sha256": smoke.digest(raw), "mtime_ns": raw.stat().st_mtime_ns}
        run.client.call("import", {"path": str(originals)})
        def promoted():
            values = all_photos(run)
            candidate = next((item for item in values if item["path"] == str(raw)), None)
            return candidate if candidate and not run.state()["indexing"] else None
        promoted = wait_for(promoted, 20, "JPEG identity promoted to RAW")
        require(promoted["id"] == photo_id and run.state()["total"] == 6, "RAW promotion changed identity or duplicated JPEG")
        require(promoted["companion_path"] == str(jpeg) and promoted["rating"] == 4, "RAW promotion lost companion/decision")
        run.client.call("undo")
        run.durable()
        require(smoke.database_rating(database, photo_id) == 0, "Undo addressed a retired JPEG ID")
        run.client.call("redo")
        run.durable()
        require(smoke.database_rating(database, photo_id) == 4, "Redo after RAW promotion was not durable")
        record("F1 JPEG to RAW promotion preserves ID and durable Undo/Redo", photo_id=photo_id, raw=str(raw), companion=str(jpeg))

        preferences = run.client.call("settings_get")
        run.client.call("quit")
        run.finish()
        journal = database.parent / "pending-decisions.json"
        require(not journal.exists(), "Clean quit left a recovery journal")
        preferences.update(interface_font="Geist Mono", auto_advance=False, auto_discover_drives=False,
                           filter="all", min_rating=0, view_mode="loupe", grouping_mode="off", last_photo_id=photo_id)
        synced_json(journal, {"version": 1, "decisions": [{"id": photo_id, "rating": 5,
                    "flag": "Pick", "label": "Blue"}], "settings": preferences})
        connection = smoke.writer_lock(database)
        try:
            run = launch(2)
            state = run.state()
            require(state["recovery"]["pending"] and journal.exists(), "Recovery disappeared before a durable decision ACK")
            require(smoke.database_rating(database, photo_id) == 4, "Held writer did not hold journal replay")
            revision = state["persistence"]["revision"]
            run.client.call("rate", {"ids": [photo_id], "rating": 1}, error=True)
            require(run.state()["persistence"]["revision"] == revision, "Mutation was allowed during recovery")
        finally:
            connection.rollback()
            connection.close()
        # A slow native startup can cross SQLite's busy timeout; explicitly exercise retry then.
        retry = run.state()["persistence"]["state"] == "failed"
        if retry:
            run.client.call("command", {"name": "retry_saves"})
        recovered = wait_for(lambda: (lambda state: state if not state["recovery"]["pending"]
                            and state["persistence"]["state"] == "durable"
                            and state["settings_persistence"]["state"] == "durable" else None)(run.state()),
                            18, "decision and preference recovery ACKs")
        settled(run, 6)
        require(not journal.exists(), "Recovery journal was not removed after both ACKs")
        recovered_photo = run.client.call("photo_get", {"id": photo_id})["photo"]
        require((recovered_photo["rating"], recovered_photo["flag"], recovered_photo["label"]) == (5, "Pick", "Blue"),
                "Recovered decision differs from journal")
        require(run.client.call("settings_get")["interface_font"] == "Geist Mono", "Recovered preferences inactive")
        require(json.loads((configuration / "settings.json").read_text())["interface_font"] == "Geist Mono", "Preference ACK preceded disk save")
        run.client.call("rate", {"ids": [photo_id], "rating": 3})
        run.durable()
        require(smoke.database_rating(database, photo_id) == 3, "Mutation remained blocked after recovery")
        record("F2 journal replay waits for ACKs then removes journal and restores mutation", photo_id=photo_id,
               journal=str(journal), retry_after_busy_timeout=retry, settings_state=recovered["settings_persistence"])

        # Preferences-only recovery must survive a malformed ordinary settings file.
        preferences = run.client.call("settings_get")
        run.client.call("quit")
        run.finish()
        preferences.update(interface_font="DM Mono", auto_advance=False, auto_discover_drives=False)
        settings_path = configuration / "settings.json"
        settings_path.write_bytes(b"{malformed ordinary preferences")
        synced_json(journal, {"version": 1, "decisions": [], "settings": preferences})
        journal_before = journal.read_bytes()
        run = launch(3)
        settled(run, 6)
        pending = run.state()
        require(pending["recovery"]["pending"] and not pending["settings_writable"],
                "Preferences-only recovery ignored malformed ordinary settings")
        require(journal.read_bytes() == journal_before, "Preferences-only journal was cleared without a preference ACK")
        run.client.call("quit")
        require(run.process.poll() is None and not run.state()["closing"], "Native quit discarded unreplayed preferences")

        # A second native binary sharing this *private* XDG environment must fail
        # before journal/catalog access, leaving this owner's endpoint untouched.
        endpoint = configuration / "automation.json"
        endpoint_before = endpoint.read_bytes()
        malformed_before = settings_path.read_bytes()
        connection = smoke.writer_lock(database)
        try:
            rows_before = connection.execute("SELECT id,path,rating,flag,label,data FROM photos ORDER BY id").fetchall()
            with (root / "second-native.stdout.log").open("wb") as output, (root / "second-native.stderr.log").open("wb") as errors:
                second = subprocess.Popen([str(binary)], env=environment, stdout=output, stderr=errors)
                extra_processes.append(second)
                second.wait(timeout=10)
            require(second.returncode == 1, "Second owner did not fail with exit status 1")
            message = (root / "second-native.stderr.log").read_text()
            require("already owns this catalog" in message, "Second owner failed for an unrelated reason: " + message)
            require(journal.read_bytes() == journal_before, "Second owner changed the pending journal")
            require(settings_path.read_bytes() == malformed_before, "Second owner rewrote malformed settings")
            require(endpoint.read_bytes() == endpoint_before, "Second owner replaced the live endpoint")
            require(connection.execute("SELECT id,path,rating,flag,label,data FROM photos ORDER BY id").fetchall() == rows_before,
                    "Second owner changed catalog rows")
        finally:
            connection.rollback()
            connection.close()
        require(run.process.poll() is None and json.loads(endpoint.read_text())["pid"] == run.process.pid,
                "Original private owner did not survive second launch")
        record("F5 second same-XDG native owner exits1 without journal/catalog/endpoint mutation",
               owner_pid=run.process.pid, rejected_pid=second.pid, exit_status=second.returncode)

        repaired = dict(preferences)
        repaired["interface_font"] = "DM Sans"
        synced_json(settings_path, repaired)
        run.client.call("command", {"name": "retry_saves"})
        replayed = wait_for(lambda: (lambda state: state if not state["recovery"]["pending"]
                            and state["settings_persistence"]["state"] == "durable" else None)(run.state()),
                            15, "repaired preferences-only recovery")
        require(not journal.exists(), "Preferences-only journal survived its exact durable ACK")
        require(json.loads(settings_path.read_text())["interface_font"] == "DM Mono", "Retry saved repaired defaults instead of the journal payload")
        require(run.client.call("settings_get")["interface_font"] == "DM Mono", "Retained recovery preferences were not active")
        require(smoke.database_rating(database, photo_id) == 3, "Preferences-only replay changed a decision")
        run.client.call("rate", {"ids": [photo_id], "rating": 2})
        run.durable()
        require(smoke.database_rating(database, photo_id) == 2, "Recovery gate did not reopen after preference ACK")
        record("F4 preferences-only journal survives malformed settings and blocked quit, then repairs/retries",
               journal=str(journal), saved_font="DM Mono", settings_state=replayed["settings_persistence"])

        size = args.export_mib * 1024 * 1024
        require(shutil.disk_usage(root).free > size + 256 * 1024 * 1024, "Insufficient space for isolated export fixture")
        large = originals / "slow-export.bmp"
        smoke.bmp(large, 16, 16, 7)
        # Padding is sparse; the BMP header remains a safe, genuinely decodable 16x16 image.
        with large.open("r+b") as output:
            output.truncate(size)
            output.flush()
            os.fsync(output.fileno())
        source_mtime = large.stat().st_mtime_ns
        source_sha = smoke.digest(large)
        source_blake3 = blake3(large, args.blake3_tool)
        run.client.call("import", {"path": str(originals)})
        wait_for(lambda: next((photo for photo in all_photos(run) if photo["path"] == str(large)), None), 20, "sparse BMP catalog entry")
        settled(run, 7)
        large_id = next(photo["id"] for photo in all_photos(run) if photo["path"] == str(large))
        run.client.call("flag", {"ids": [photo["id"] for photo in all_photos(run)], "flag": "none"})
        run.client.call("flag", {"ids": [large_id], "flag": "pick"})
        run.durable()
        destination = root / "large-export"
        began = time.monotonic()
        job = run.client.call("export_picks", {"destination": str(destination)})
        closing = run.client.call("quit")["workspace"]
        require(run.process.poll() is None and closing["closing"], "Quit exited before the queued export could finish")
        initial_job = next(item for item in closing["export_jobs"] if item["id"] == job["job_id"])
        require(initial_job["state"] in ("queued", "running"), "Fixture export was too fast to exercise pending quit")
        terminal = None
        manifest_path = destination / "shadowroom-picks-manifest.json"
        deadline = time.monotonic() + 120
        while run.process.poll() is None:
            require(time.monotonic() < deadline, "Native quit/export exceeded 120 seconds")
            try:
                state = run.state()
            except AssertionError as error:
                require(any(message in str(error) for message in ("not running", "endpoint is unavailable", "Desktop closed the automation connection")),
                        "Unexpected MCP failure during quit: " + str(error))
                # Endpoint retirement can race the last read after a normal close.
                try:
                    run.process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    raise
                require(run.process.returncode == 0, "Native process failed during export quit")
                break
            outcome = next((item for item in state["export_jobs"] if item["id"] == job["job_id"]), None)
            if outcome and outcome["state"] in ("completed", "failed"):
                terminal = outcome
                require(outcome["state"] == "completed", "Export failed before quit: %r" % outcome)
            time.sleep(0.01)
        run.finish()
        elapsed = time.monotonic() - began
        manifest = json.loads(manifest_path.read_text())
        require(manifest["state"] == "completed" and manifest["completed_files"] == 1, "Quit left an incomplete export manifest")
        entry = next(entry for group in manifest["groups"] for entry in group["files"] if entry["source"] == str(large))
        copied = pathlib.Path(entry["destination"])
        require(entry["status"] == "verified" and entry["bytes"] == size and entry["output_retained"], "Export file was not verified")
        require(smoke.digest(copied) == source_sha, "Export SHA256 differs from sparse original")
        require(re.fullmatch(r"[0-9a-f]{64}", entry["blake3"]), "Native BLAKE3 manifest digest missing")
        if source_blake3 is not None:
            require(blake3(copied, args.blake3_tool) == source_blake3 == entry["blake3"], "Independent BLAKE3 verification failed")
        require(large.stat().st_mtime_ns == source_mtime and smoke.digest(large) == source_sha, "Export changed original")
        record("F3 normal quit waits for large verified export and worker completion", job_id=job["job_id"],
               source_bytes=size, seconds=round(elapsed, 3), native_pid=run.process.pid,
               observed_terminal=terminal, manifest_state=manifest["state"], sha256=source_sha,
               blake3=entry["blake3"], independently_verified_blake3=source_blake3 is not None)
        for path, expected in before.items():
            require(smoke.digest(pathlib.Path(path)) == expected["sha256"], "Fixture original changed: " + path)
            require(pathlib.Path(path).stat().st_mtime_ns == expected["mtime_ns"], "Fixture mtime changed: " + path)
        report["result"] = "PASS"
    finally:
        for process in extra_processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
        for run in reversed(active):
            try:
                run.abort()
            except Exception as error:
                report.setdefault("cleanup_errors", []).append(str(error))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--export-mib", type=int, default=512, help="Sparse source size (32..512 MiB)")
    parser.add_argument("--blake3-tool", type=pathlib.Path, help="Optional b3sum-compatible executable for independent BLAKE3 checks")
    parser.add_argument("--output", help="New isolated evidence directory")
    args = parser.parse_args()
    require(32 <= args.export_mib <= 512, "--export-mib must be between 32 and 512")
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-native-regressions-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = {"result": "RUNNING", "session": str(root), "checks": []}
    started = time.monotonic()
    print("Regression evidence: " + str(root), flush=True)
    try:
        exercise(args, root, report)
    except Exception as error:
        report["result"] = "FAIL"
        report["failure"] = str(error)
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), file=sys.stderr)
    report["elapsed_seconds"] = round(time.monotonic() - started, 3)
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"result": report["result"], "report": str(root / "report.json"), "seconds": report["elapsed_seconds"]}), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
