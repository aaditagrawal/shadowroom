#!/usr/bin/env python3
"""Native macOS acceptance with test-owned app paths, fixtures and process IDs."""
import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import time
import traceback


spec = importlib.util.spec_from_file_location("shadowroom_native_smoke", Path(__file__).with_name("native-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


def native_quit(pid):
    """NSRunningApplication's normal terminate request; no UI scripting/keystrokes."""
    ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
    objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
    objc.objc_getClass.argtypes = [ctypes.c_char_p]
    objc.objc_getClass.restype = ctypes.c_void_p
    objc.sel_registerName.argtypes = [ctypes.c_char_p]
    objc.sel_registerName.restype = ctypes.c_void_p
    address = ctypes.cast(objc.objc_msgSend, ctypes.c_void_p).value
    find = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int)(address)
    terminate = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)(address)
    application = find(objc.objc_getClass(b"NSRunningApplication"),
                       objc.sel_registerName(b"runningApplicationWithProcessIdentifier:"), pid)
    smoke.require(application, "No native NSRunningApplication for own app PID")
    smoke.require(terminate(application, objc.sel_registerName(b"terminate")), "Native quit request rejected")


def own_windows(pid):
    """Read CoreGraphics window metadata for this PID; never capture other windows."""
    core = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    core.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    core.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
    foundation.CFArrayGetCount.argtypes = [ctypes.c_void_p]
    foundation.CFArrayGetCount.restype = ctypes.c_long
    foundation.CFArrayGetValueAtIndex.argtypes = [ctypes.c_void_p, ctypes.c_long]
    foundation.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
    foundation.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    foundation.CFDictionaryGetValue.restype = ctypes.c_void_p
    foundation.CFNumberGetValue.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p]
    foundation.CFNumberGetValue.restype = ctypes.c_bool
    foundation.CFRelease.argtypes = [ctypes.c_void_p]
    key_pid = ctypes.c_void_p.in_dll(core, "kCGWindowOwnerPID").value
    key_id = ctypes.c_void_p.in_dll(core, "kCGWindowNumber").value
    windows = core.CGWindowListCopyWindowInfo(1 | 16, 0)
    result = []
    try:
        for index in range(foundation.CFArrayGetCount(windows)):
            value = foundation.CFArrayGetValueAtIndex(windows, index)
            owner = ctypes.c_int()
            number = foundation.CFDictionaryGetValue(value, key_pid)
            if number and foundation.CFNumberGetValue(number, 9, ctypes.byref(owner)) and owner.value == pid:
                number = foundation.CFDictionaryGetValue(value, key_id)
                identifier = ctypes.c_int()
                if number and foundation.CFNumberGetValue(number, 9, ctypes.byref(identifier)):
                    result.append(identifier.value)
    finally:
        if windows:
            foundation.CFRelease(windows)
    return result


def key_to_owned_process(pid, keycode, command=False):
    core = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    core.CGPreflightPostEventAccess.restype = ctypes.c_bool
    smoke.require(core.CGPreflightPostEventAccess(), "Event posting permission is absent; no permission prompt requested")
    core.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
    core.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
    core.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
    core.CGEventPostToPid.argtypes = [ctypes.c_int, ctypes.c_void_p]
    foundation.CFRelease.argtypes = [ctypes.c_void_p]
    for pressed in (True, False):
        event = core.CGEventCreateKeyboardEvent(None, keycode, pressed)
        smoke.require(event, "Could not create native keyboard event")
        try:
            core.CGEventSetFlags(event, 1 << 20 if command else 0)
            core.CGEventPostToPid(pid, event)
        finally:
            foundation.CFRelease(event)


def command_palette_evidence(pid, window, root):
    core = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    core.CGPreflightScreenCaptureAccess.restype = ctypes.c_bool
    allowed = core.CGPreflightScreenCaptureAccess()
    paths = []

    def capture(name):
        if allowed:
            target = root / name
            subprocess.run(["/usr/sbin/screencapture", "-x", "-l", str(window), str(target)], check=True, timeout=8)
            paths.append(str(target))

    capture("cmd-k-before.png")
    key_to_owned_process(pid, 40, command=True)  # ANSI K
    time.sleep(0.4)
    capture("cmd-k-open.png")
    key_to_owned_process(pid, 53)  # Escape
    time.sleep(0.3)
    capture("cmd-k-escaped.png")
    return {"method": "CGEventPostToPid Cmd+K/Escape, own app PID only",
            "screen_capture_preflight": allowed, "screenshots": paths,
            "verification": "visual inspection required; event delivery alone is not proof"}


def exercise(binary, root, report, heif_fixture=None):
    smoke.require(sys.platform == "darwin", "Run on native macOS")
    smoke.require(binary.is_file() and os.access(binary, os.X_OK), "Executable binary required")
    home = Path.home()
    links = {
        home / "Library/Application Support/shadowroom": root / "config/shadowroom",
        home / "Library/Application Support/app.Shadowroom.shadowroom": root / "data/catalog",
        home / "Library/Caches/app.Shadowroom.shadowroom": root / "cache",
    }
    # Reject all existing app state, including dangling symlinks. Never move it.
    for target in links:
        smoke.require(not os.path.lexists(target), "Existing Shadowroom state: " + str(target))
    created = []
    active = []
    environment = dict(os.environ, RUST_BACKTRACE="1")
    fixtures = root / "originals"
    fixtures.mkdir()
    originals = {}
    for index in range(6):
        photo = fixtures / ("fixture-%02d.bmp" % index)
        smoke.bmp(photo, 256 + index * 32, 160 + index * 16, index)
        originals[str(photo)] = smoke.digest(photo)
    report.update(binary=str(binary), backend="native macOS AppKit/Metal", fixtures=originals)

    def record(name, **details):
        report["checks"].append(dict(name=name, result="PASS", **details))
        print("PASS " + name, flush=True)

    def launch(number, initial=False):
        run = smoke.NativeRun.__new__(smoke.NativeRun)
        active.append(run)
        smoke.NativeRun.__init__(run, binary, environment, root, fixtures, number, initial)
        return run

    try:
        for target, private in links.items():
            private.mkdir(parents=True)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(private, target_is_directory=True)
            created.append(target)
        configuration = root / "config/shadowroom"
        (configuration / "settings.json").write_text(json.dumps({
            "auto_discover_drives": False, "auto_advance": False,
            "memory_budget_mb": 128, "cache_budget_mb": 128,
        }))
        run = launch(1, True)
        client = run.client
        windows = smoke.wait_for(lambda: own_windows(run.process.pid), 6, "own native on-screen window")
        record("native AppKit/Metal window, six BMPs, MCP %d tools/notification/stdout purity" % len(smoke.TOOLS), pid=run.process.pid, windows=windows)
        report["command_palette_evidence"] = command_palette_evidence(run.process.pid, windows[0], root)
        photos = client.call("library_list", {"scope": "all", "limit": 6})["photos"]
        a, b = [photo["id"] for photo in photos[:2]]
        database = next((root / "data").rglob("catalog.sqlite3"))
        for photo in photos:
            smoke.require(client.call("photo_get", {"id": photo["id"]})["photo"]["path"] == photo["path"], "Metadata mismatch")
            smoke.require(photo["camera"] == "" and photo["captured_at"] is None, "Invented BMP EXIF")
        lock = smoke.writer_lock(database)
        timings = []
        try:
            for rating in (5, 2, 5):
                started = time.monotonic()
                state = client.call("rate", {"rating": rating, "ids": [a]})
                timings.append(time.monotonic() - started)
                smoke.require(state["persistence"]["state"] == "pending", "Writer lock not pending")
            smoke.require(max(timings) < 1, "Native UI blocked on writer lock")
        finally:
            lock.rollback()
            lock.close()
        run.durable()
        smoke.require(smoke.database_rating(database, a) == 5, "Latest value did not persist")
        client.call("undo")
        run.durable()
        smoke.require(smoke.database_rating(database, a) == 2, "Undo failed")
        client.call("redo")
        run.durable()
        smoke.require(smoke.database_rating(database, a) == 5, "Redo failed")
        record("native writer-lock responsiveness, coalesced 5→2→5, durable undo/redo", seconds=timings)

        lock = smoke.writer_lock(database)
        try:
            client.call("rate", {"rating": 3, "ids": [b]})
            failed = smoke.wait_for(lambda: (lambda s: s if s["persistence"]["state"] == "failed" else None)(run.state()), 10, "busy timeout")
            smoke.require(b in failed["persistence"]["unsaved_ids"], "Failed decision lost")
            client.call("quit")
            smoke.require(run.process.poll() is None and not run.state()["closing"], "Failed save incorrectly allowed quit")
        finally:
            lock.rollback()
            lock.close()
        client.call("command", {"name": "retry_saves"})
        run.durable()
        smoke.require(smoke.database_rating(database, b) == 3, "Failed save retry not durable")
        record("actual SQLite busy timeout, blocked quit, explicit retry durable")

        client.call("flag", {"flag": "pick", "ids": [a, b]})
        client.call("label", {"label": "purple", "ids": [a]})
        client.call("trash", {"ids": [b], "trashed": True})
        run.durable()
        trashed = client.call("photo_get", {"id": b})["photo"]
        smoke.require(trashed["trashed"] and trashed["rating"] == 3 and trashed["flag"] == "Pick",
                      "In-app Trash lost prior decisions")
        client.call("filter", {"filter": "trash"})
        smoke.require(run.state()["filtered_count"] == 1, "Trash filter must show the trashed fixture")
        client.call("trash", {"ids": [b], "trashed": False})
        run.durable()
        smoke.require(not client.call("photo_get", {"id": b})["photo"]["trashed"], "Restore failed")
        client.call("filter", {"filter": "all"})
        client.call("duplicates", {"action": "status"})
        record("durable in-app Trash and restore retain review decisions; duplicate status available")
        client.call("select", {"id": a})
        client.call("pin")
        client.call("select", {"id": b})
        client.call("view", {"mode": "compare"})
        client.call("zoom", {"mode": "full"})
        state = run.state()
        smoke.require(state["view"] == "compare" and state["reference_id"] == a and state["zoom"] == "full", "Compare/full state failed")
        client.call("view", {"mode": "grid"})
        client.call("group", {"mode": "time", "gap_ms": 2000})
        smoke.wait_for(lambda: run.state()["groups"], 6, "background grouping")
        client.call("group", {"mode": "off"})
        cache = smoke.wait_for(lambda: (lambda s: s if s["decoded"] > 0 else None)(client.call("cache_stats")), 10, "native Metal previews")
        smoke.require(cache["memory_bytes"] <= cache["memory_budget_bytes"], "Cache exceeded budget")
        run.durable()
        job = client.call("export_picks", {"destination": str(root / "export")})
        completed = smoke.wait_for(lambda: next((j for j in run.state()["export_jobs"] if j["id"] == job["job_id"] and j["state"] in ("completed", "failed")), None), 10, "tracked export")
        smoke.require(completed["state"] == "completed" and completed["completed"] == 2, "Export failed")
        for photo in photos[:2]:
            source = Path(photo["path"])
            smoke.require(smoke.digest(root / "export" / source.name) == originals[str(source)], "Export checksum mismatch")
        record("Metal preview/cache, compare/full, grouping, tracked verified export", cache=cache)

        family = root / "mac-theme.json"
        family.write_text(json.dumps({"name": "Mac Validation", "themes": [{"name": "Mac Validation Dark", "appearance": "dark", "style": {"background": "#0f0f0f", "text": "#e6e6e6"}}]}))
        smoke.require(client.call("theme_import", {"path": str(family)})["persisted_family"], "Theme import not persisted")
        client.call("settings_set", {"settings": {"interface_font": "DM Mono", "theme_mode": "manual", "filter": "all", "view_mode": "loupe", "auto_discover_drives": False}})
        run.durable()
        client.call("quit")
        run.finish()
        family.unlink()
        run = launch(2)
        client = run.client
        smoke.wait_for(lambda: run.state()["theme"] == "Mac Validation Dark", 6, "saved theme")
        smoke.require(run.state()["interface_font"] == "DM Mono", "Font did not survive restart")
        smoke.require(client.call("photo_get", {"id": a})["photo"]["label"] == "Purple", "Label did not survive restart")
        record("normal MCP quit/relaunch preserves decisions, label, theme and font")

        # Keep probes read-only: a failed mutation acknowledgement must never be
        # blindly retried. This also exercises fresh loopback connections repeatedly.
        for _ in range(150):
            smoke.require(run.state()["total"] == 6, "Read-only state probe lost library")
        record("150 consecutive fresh-connection read-only MCP state probes")

        lock = smoke.writer_lock(database)
        try:
            client.call("rate", {"rating": 4, "ids": [a]})
            smoke.require(run.state()["persistence"]["state"] == "pending", "OS quit case must have a real pending write")
            quit_started = time.monotonic()
            native_quit(run.process.pid)
            # OS application termination may tear down automation before the
            # process exits. Observe the process/journal, not a closing endpoint.
            deadline = time.monotonic() + 7
            while run.process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.05)
            exited_early = run.process.poll() is not None
            if exited_early:
                journal = root / "data/catalog/pending-decisions.json"
                smoke.require(journal.is_file(), "OS quit lost pending write without recovery journal")
                report["os_quit_journal"] = json.loads(journal.read_text())
        finally:
            lock.rollback()
            lock.close()
        run.finish()
        run = launch(3)
        client = run.client
        run.durable()
        smoke.require(smoke.database_rating(database, a) == 4, "OS quit pending decision not recovered")
        smoke.require(client.call("photo_get", {"id": a})["photo"]["rating"] == 4, "Recovered decision missing from UI")
        record("own-PID native NSRunningApplication quit under writer lock and durable restart recovery",
               exited_before_release=exited_early, quit_seconds=time.monotonic() - quit_started)
        if heif_fixture:
            # Use only an explicitly supplied generated fixture. Independent core
            # reference comparison checks its decoded orientation and color.
            smoke.require(heif_fixture.is_file(), "HEIF fixture missing")
            baseline = client.call("cache_stats")
            for name in ("native-fixture.heic", "native-copy.heic"):
                target = fixtures / name
                shutil.copy2(heif_fixture, target)
                originals[str(target)] = smoke.digest(target)
            client.call("import", {"path": str(fixtures)})
            smoke.wait_for(lambda: (lambda state: state if state["total"] == 8 and not state["indexing"] else None)(run.state()),
                           15, "two native HEIF fixtures indexed")
            heifs = [photo for photo in client.call("library_list", {"scope": "all", "limit": 8})["photos"]
                     if Path(photo["path"]).suffix.lower() == ".heic"]
            smoke.require(len(heifs) == 2 and all(p["width"] > 0 and p["height"] > 0 for p in heifs),
                          "Native HEIF metadata missing")
            client.call("view", {"mode": "loupe"})
            client.call("zoom", {"mode": "full"})
            client.call("select", {"id": heifs[0]["id"]})
            cache = smoke.wait_for(lambda: (lambda c: c if c["decoded"] > baseline["decoded"] and not c["queued"] and not c["active"] else None)(client.call("cache_stats")),
                                   15, "native ImageIO HEIF preview")
            smoke.require(cache["errors"] == baseline["errors"], "Native HEIF preview failed")
            job = client.call("duplicates", {"action": "scan", "ids": [p["id"] for p in heifs]})
            duplicate = smoke.wait_for(lambda: (lambda d: d if d and d["id"] == job["job_id"] and d["phase"] in ("completed", "failed") else None)(client.call("duplicates", {"action": "status"})["scan"]),
                                       10, "actual HEIF exact duplicate scan")
            smoke.require(duplicate["phase"] == "completed" and not duplicate["errors"], "HEIF duplicate scan failed")
            smoke.require(any(set(group) == {p["id"] for p in heifs} for group in duplicate["groups"]),
                          "Identical HEIF bytes not grouped")
            record("native ImageIO HEIF metadata/full preview and exact duplicate grouping", photos=heifs, duplicate_scan=duplicate)
        client.call("quit")
        run.finish()
        for source, expected in originals.items():
            smoke.require(smoke.digest(Path(source)) == expected, "Original bytes changed")
        record("all generated originals remain unchanged", count=len(originals))
        report["result"] = "PASS"
    except Exception:
        report["processes_at_failure"] = [
            {"pid": run.process.pid, "returncode": run.process.poll()}
            for run in active if hasattr(run, "process")
        ]
        raise
    finally:
        for run in reversed(active):
            try:
                run.abort()
            except Exception as error:
                report.setdefault("cleanup_errors", []).append(str(error))
        for target in reversed(created):
            if target.is_symlink() and target.readlink() == links[target]:
                target.unlink()
            else:
                report.setdefault("cleanup_errors", []).append("Refused to remove changed path: " + str(target))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path, help="New directory for retained test evidence")
    parser.add_argument("--heif-fixture", type=Path, help="Optional generated SDR HEIC for native preview and duplicate acceptance")
    args = parser.parse_args()
    args.evidence.mkdir(parents=True, exist_ok=False)
    report = dict(result="RUNNING", session=str(args.evidence), checks=[])
    began = time.monotonic()
    try:
        exercise(args.binary.resolve(), args.evidence.resolve(), report,
                 args.heif_fixture.resolve() if args.heif_fixture else None)
    except Exception as error:
        report.update(result="FAIL", failure=str(error))
        (args.evidence / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), file=sys.stderr)
    report["elapsed_seconds"] = time.monotonic() - began
    (args.evidence / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(dict(result=report["result"], checks=len(report["checks"]), report=str(args.evidence / "report.json"))), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
