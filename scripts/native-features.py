#!/usr/bin/env python3
"""Isolated native app acceptance for app-only Trash and opt-in exact duplicates.

Uses the actual GUI binary through its own authenticated MCP endpoint, never the
user's running workspace. All source writes are private fixture setup only.
"""
import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
import traceback

spec = importlib.util.spec_from_file_location("native_smoke", pathlib.Path(__file__).with_name("native-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
require, wait_for, digest = smoke.require, smoke.wait_for, smoke.digest


def exercise(args, root, report):
    binary = pathlib.Path(args.binary).resolve()
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update(XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
               XDG_CACHE_HOME=str(root / "cache"), XDG_STATE_HOME=str(root / "state"))
    configuration = root / "config/shadowroom"
    configuration.mkdir(parents=True)
    (configuration / "settings.json").write_text(json.dumps({"auto_discover_drives": False,
        "auto_advance": False, "grouping_mode": "off", "view_mode": "grid",
        "memory_budget_mb": 128, "cache_budget_mb": 128}))
    fixtures = root / "originals"
    fixtures.mkdir()
    smoke.bmp(fixtures / "a.bmp", 640, 400, 0)
    shutil.copyfile(fixtures / "a.bmp", fixtures / "b-renamed.bmp")
    changed = bytearray((fixtures / "a.bmp").read_bytes())
    changed[30_000] ^= 127  # Valid same-size BMP; outside start/middle/end samples.
    (fixtures / "c-same-size.bmp").write_bytes(changed)
    smoke.bmp(fixtures / "d.bmp", 640, 400, 1)
    shutil.copyfile(fixtures / "d.bmp", fixtures / "e-renamed.bmp")
    smoke.bmp(fixtures / "unique.bmp", 642, 400, 2)
    originals = {}
    def remember(path):
        originals[str(path)] = {"sha256": digest(path), "mtime_ns": path.stat().st_mtime_ns}
    for path in fixtures.iterdir():
        remember(path)
    report.update(binary=str(binary), binary_bytes=binary.stat().st_size,
        binary_mtime_ns=binary.stat().st_mtime_ns, display=env.get("DISPLAY"),
        backend="native X11 backend on host desktop; WAYLAND_DISPLAY removed", checks=[],
        synthetic_sources="6 BMPs; cancellation adds 2 valid BMPs with sparse zero trailers")
    active = []
    def record(name, **details):
        report["checks"].append({"name": name, "result": "PASS", **details})
        print("PASS " + name, flush=True)
    def launch(number, initial=False):
        run = smoke.NativeRun.__new__(smoke.NativeRun)
        active.append(run)
        smoke.NativeRun.__init__(run, binary, env, root, fixtures, number, initial)
        report.setdefault("owned_pids", []).append(run.process.pid)
        return run
    def export_done(run, tool, destination, ids=None):
        arguments = {"destination": str(destination)}
        if ids is not None:
            arguments["ids"] = ids
        queued = run.client.call(tool, arguments)
        final = wait_for(lambda: next((job for job in run.state()["export_jobs"]
            if job["id"] == queued["job_id"] and job["state"] in ("completed", "failed")), None),
            15, tool + " completion")
        require(final["state"] == "completed", "Export failed: " + repr(final))
        return final
    def scan_done(run, ids=None):
        arguments = {"action": "scan"}
        if ids is not None:
            arguments["ids"] = ids
        queued = run.client.call("duplicates", arguments)
        def finished():
            scan = run.client.call("duplicates", {"action": "status"})["scan"]
            return scan if scan and scan["id"] == queued["job_id"] and scan["phase"] in ("completed", "failed", "cancelled") else None
        result = wait_for(finished, 20, "exact duplicate scan")
        require(result["phase"] == "completed", "Duplicate scan did not complete: " + repr(result))
        require(not result["errors"], "Duplicate scan errors: " + repr(result["errors"]))
        return result
    try:
        run = launch(1, True)
        client = run.client
        photos = client.call("library_list", {"scope": "all", "limit": 1000})["photos"]
        by_name = {photo["filename"]: photo["id"] for photo in photos}
        a, b, c = [by_name[name] for name in ("a.bmp", "b-renamed.bmp", "c-same-size.bmp")]
        d, e = [by_name[name] for name in ("d.bmp", "e-renamed.bmp")]
        wanted = {frozenset((a, b)), frozenset((d, e))}
        first = scan_done(run)
        require({frozenset(group) for group in first["groups"]} == wanted,
                "Exact groups confused same-size edited bytes: " + repr(first))
        require(first["bytes_read"] > 0 and first["cache_hits"] == 0, "Initial scan did not read sources")
        require(c not in {photo_id for group in first["groups"] for photo_id in group}, "Edited source falsely classified duplicate")
        record("cold opt-in scan verifies bytes, finds renamed copies and rejects same-size sample collision", scan=first)

        client.call("rate", {"ids": [a], "rating": 4})
        client.call("flag", {"ids": [a], "flag": "pick"})
        client.call("label", {"ids": [a], "label": "blue"})
        run.durable()
        before = client.call("photo_get", {"id": a})["photo"]
        result = client.call("trash", {"ids": [a], "trashed": True})
        require(result["originals_untouched"], "Trash did not state non-destructive acceptance")
        run.durable()
        def photo(photo_id):
            return client.call("photo_get", {"id": photo_id})["photo"]
        after = photo(a)
        require(after["trashed"] and all(after[key] == before[key] for key in ("rating", "flag", "label")), "Trash lost decisions")
        for filter_name, count in [("all", 5), ("picks", 0), ("trash", 1)]:
            client.call("filter", {"filter": filter_name, "min_rating": 0, "clear_folder": True})
            visible = client.call("library_list")["photos"]
            require(len(visible) == count, "Wrong " + filter_name + " filter count")
            if filter_name == "trash":
                require(visible[0]["id"] == a, "Trash filter returned wrong photo")
        require(export_done(run, "export_picks", root / "trashed-picks")["completed"] == 0,
                "A trashed pick was exported")
        require(export_done(run, "export_xmp", root / "trashed-xmp", [a])["completed"] == 0,
                "A trashed photo got an XMP export")
        client.call("undo")
        run.durable()
        require(not photo(a)["trashed"], "Undo did not restore from Trash")
        client.call("redo")
        run.durable()
        require(photo(a)["trashed"], "Redo did not put photo back in Trash")
        record("durable Trash preserves full decisions, excludes live filters/exports and supports undo/redo", id=a)
        client.call("quit")
        run.finish()

        run = launch(2)
        client = run.client
        restored = client.call("photo_get", {"id": a})["photo"]
        require(restored["trashed"] and all(restored[key] == before[key] for key in ("rating", "flag", "label")), "Restart lost Trash or decisions")
        require(run.state()["trash_count"] == 1, "Restart lost Trash collection count")
        client.call("trash", {"ids": [a], "trashed": False})
        run.durable()
        require(not client.call("photo_get", {"id": a})["photo"]["trashed"], "Restore command failed")
        record("restart keeps Trash and restore keeps previous rating/flag/label")

        snapshot = {item["id"]: {key: item[key] for key in ("rating", "flag", "label", "trashed")}
                    for item in client.call("library_list", {"scope": "all", "limit": 1000})["photos"]}
        revision = run.state()["persistence"]["revision"]
        for tool, arguments in [
            ("trash", {"ids": [a, 99999999], "trashed": True}),
            ("trash", {"ids": [a], "trashed": "yes"}),
            ("trash", {"ids": [True], "trashed": True}),
            ("duplicates", {"action": "scan", "ids": [a, 99999999]}),
            ("duplicates", {"action": "scan", "ids": ["bad"]}),
            ("duplicates", {"action": "scan", "ids": []}),
            ("duplicates", {"action": "delete"}),
        ]:
            client.call(tool, arguments, error=True)
        current = {item["id"]: {key: item[key] for key in ("rating", "flag", "label", "trashed")}
                   for item in client.call("library_list", {"scope": "all", "limit": 1000})["photos"]}
        require(current == snapshot and run.state()["persistence"]["revision"] == revision, "Invalid request mutated decisions")
        record("invalid IDs/types/actions reject atomically without mutation")

        warm = scan_done(run)
        require({frozenset(group) for group in warm["groups"]} == wanted, "Restarted warm scan groups changed")
        require(warm["bytes_read"] == 0 and warm["cache_hits"] >= 5,
                "Persisted hashes did not avoid source reads: " + repr(warm))
        record("persisted exact hashes survive restart; warm rescan reads zero source bytes", scan=warm)
        subset = scan_done(run, [a, b])
        require(subset["total"] == 2 and subset["groups"] == [[a, b]], "Explicit subset scan escaped requested IDs")
        final = scan_done(run)
        client.call("filter", {"filter": "all", "duplicates_only": True, "min_rating": 0, "clear_folder": True})
        visible = client.call("library_list")["photos"]
        ids = [item["id"] for item in visible]
        expected_ids = [photo_id for group in final["groups"] for photo_id in group]
        require(ids == expected_ids and len(ids) == 4, "Duplicate collection is not grouped contiguously")
        client.call("select", {"id": final["groups"][0][0]})
        client.call("command", {"name": "next_group"})
        require(run.state()["selected_id"] in final["groups"][1], "Next group did not reach next visible exact set")
        client.call("command", {"name": "previous_group"})
        require(run.state()["selected_id"] in final["groups"][0], "Previous group did not return to first exact set")
        record("explicit-ID scan, duplicate collection ordering and visible group navigation")
        client.call("filter", {"filter": "all", "duplicates_only": False})

        if args.heif_fixture:
            heif = pathlib.Path(args.heif_fixture).resolve()
            heif_dir = root / "heif-originals"
            heif_dir.mkdir()
            for name in ("standalone.heic", "paired.heic"):
                shutil.copyfile(heif, heif_dir / name)
            (heif_dir / "paired.nef").write_bytes(b"private synthetic RAW container for companion pairing only")
            for path in heif_dir.iterdir():
                remember(path)
            heif_before_cache = client.call("cache_stats")
            client.call("import", {"path": str(heif_dir)})
            wait_for(lambda: run.state()["total"] == 8 and not run.state()["indexing"], 20, "standalone HEIF and RAW+HEIF pair")
            imported = [item for item in client.call("library_list", {"scope": "all", "limit": 1000})["photos"] if item["folder"] == str(heif_dir)]
            require(len(imported) == 2, "HEIF companion appeared as separate photo")
            paired = next(item for item in imported if item["filename"] == "paired.nef")
            standalone = next(item for item in imported if item["filename"] == "standalone.heic")
            require(pathlib.Path(paired["companion_path"]).name == "paired.heic", "RAW did not use HEIF companion")
            if args.heif_dimensions:
                width, height = map(int, args.heif_dimensions.lower().split("x"))
                require((standalone["width"], standalone["height"]) == (width, height), "HEIF dimensions differ from fixture")
                require((paired["width"], paired["height"]) == (width, height), "RAW missing dimensions did not fall back to HEIF")
            # Isolate this export from the earlier rated/picked BMP.
            client.call("flag", {"ids": [a], "flag": "none"})
            client.call("flag", {"ids": [item["id"] for item in imported], "flag": "pick"})
            run.durable()
            exported = export_done(run, "export_picks", root / "heif-export")
            require(exported["completed"] == 3, "Paired HEIF export did not copy all three media members")
            for path in heif_dir.iterdir():
                require(digest(root / "heif-export" / path.name) == originals[str(path)]["sha256"], "HEIF export bytes differ")
            client.call("select", {"id": standalone["id"]})
            client.call("view", {"mode": "loupe"})
            time.sleep(.5)
            preview_cache = wait_for(lambda: (lambda cache: cache if cache["queued"] == 0 and cache["active"] == 0 else None)(client.call("cache_stats")), 20, "actual HEIF preview decode completion")
            require(preview_cache["errors"] == heif_before_cache["errors"], "HEIF preview failed; inspect recent_errors: " + repr(preview_cache["recent_errors"]))
            require(preview_cache["decoded"] > heif_before_cache["decoded"], "No decoded HEIF preview was produced")
            require(run.state()["error"] is None, "Selected HEIF UI reports an error")
            # Decoder completion precedes asynchronous display texture upload.
            # Allow the real render loop to replace its provisional thumbnail.
            time.sleep(1)
            preview_cache = client.call("cache_stats")
            screenshot = None
            tool = smoke.xdotool_path(None)
            if tool and shutil.which("import"):
                tool_env = env.copy()
                libraries = tool.parent.parent / "lib64"
                if libraries.is_dir():
                    tool_env["LD_LIBRARY_PATH"] = str(libraries) + (":" + tool_env["LD_LIBRARY_PATH"] if tool_env.get("LD_LIBRARY_PATH") else "")
                windows = subprocess.run([str(tool), "search", "--all", "--pid", str(run.process.pid), "--name", "^Shadowroom$"], env=tool_env, capture_output=True, text=True, timeout=3, check=True).stdout.splitlines()
                window = next(window for window in windows if window.isdigit())
                owner = subprocess.run([str(tool), "getwindowpid", window], env=tool_env, capture_output=True, text=True, timeout=3, check=True).stdout.strip()
                require(owner == str(run.process.pid), "Refusing to capture another process's window")
                target = root / "actual-heif-loupe.png"
                subprocess.run(["import", "-window", window, str(target)], env=env, capture_output=True, timeout=5, check=True)
                screenshot = str(target)
            record("actual HEIF decoded preview/import/dimensions and RAW+HEIF pairing/export", fixture=str(heif),
                   photos=imported, preview_cache=preview_cache, screenshot=screenshot)
        else:
            report["checks"].append({"name": "actual HEIF import/pair/export", "result": "SKIP", "reason": "--heif-fixture not supplied"})

        # Long, valid BMP sources make cancellation observable without allocating
        # gigabytes of fixture pixels. The zero trailers are sparse file extents.
        large_dir = root / "cancellation-originals"
        large_dir.mkdir()
        seed = (fixtures / "a.bmp").read_bytes()
        for name in ("long-a.bmp", "long-b.bmp"):
            path = large_dir / name
            with path.open("wb") as destination:
                destination.write(seed)
                destination.truncate(1024 * 1024 * 1024)
            remember(path)
        before_total = run.state()["total"]
        client.call("import", {"path": str(large_dir)})
        wait_for(lambda: run.state()["total"] == before_total + 2 and not run.state()["indexing"], 20, "cancellation sources indexed")
        large_ids = [item["id"] for item in client.call("library_list", {"scope": "all", "limit": 1000})["photos"] if item["folder"] == str(large_dir)]
        # R6-2: start through the agent route while the old completed collection
        # is visible. Long sources keep the new scan genuinely in flight.
        client.call("filter", {"filter": "all", "duplicates_only": True, "min_rating": 0, "clear_folder": True})
        require(run.state()["filtered_count"] == 4, "Expected prior completed duplicate set before rescan")
        queued = client.call("duplicates", {"action": "scan", "ids": large_ids})
        rescanning = run.state()
        require(rescanning["duplicates_only"] and rescanning["filtered_count"] == 0,
                "Starting a new scan left old duplicate IDs in the visible collection")
        require(not rescanning["duplicate_scan"]["groups"] and not client.call("library_list")["photos"],
                "New scan mixed old group membership with new scan state")
        client.call("command", {"name": "next_group"})
        require(run.state()["selected_id"] is None, "Empty in-flight duplicate view navigated an obsolete group")
        wait_for(lambda: (lambda scan: scan and scan["id"] == queued["job_id"] and scan["phase"] == "running" and scan["stage"] == "verifying")(client.call("duplicates", {"action": "status"})["scan"]), 10, "active streamed verification")
        began = time.monotonic()
        client.call("rate", {"ids": [a], "rating": 2})
        run.durable()
        decision_seconds = time.monotonic() - began
        require(decision_seconds < 3, "Decision persistence stalled behind duplicate source reads")
        client.call("duplicates", {"action": "cancel"})
        cancelled = wait_for(lambda: (lambda scan: scan if scan and scan["id"] == queued["job_id"] and scan["phase"] in ("cancelled", "failed", "completed") else None)(client.call("duplicates", {"action": "status"})["scan"]), 10, "cancelled scan")
        require(cancelled["phase"] == "cancelled", "Cancel did not stop active stream: " + repr(cancelled))
        record("active streamed scan keeps decision ACK/durability responsive and cancels", decision_seconds=decision_seconds, scan=cancelled)
        require(run.state()["filtered_count"] == 0 and not client.call("library_list")["photos"]
                and not run.state()["duplicate_scan"]["groups"],
                "Cancelled rescan restored obsolete duplicate IDs or group membership")
        client.call("command", {"name": "previous_group"})
        require(run.state()["selected_id"] is None, "Cancelled empty duplicate collection navigated old groups")
        record("R6-2 rescan admission and cancellation clear old visible IDs and group navigation")

        # R6-3: a stable JPEG ID becomes a different logical RAW+JPEG asset after
        # a completed scan. Reuse the real embedded JPEG regression fixture.
        client.call("filter", {"filter": "all", "duplicates_only": False, "min_rating": 0, "clear_folder": True})
        regression_spec = importlib.util.spec_from_file_location("feature_regression_fixture", pathlib.Path(__file__).with_name("native-regressions.py"))
        regression = importlib.util.module_from_spec(regression_spec)
        regression_spec.loader.exec_module(regression)
        promotion_dir = root / "promotion-originals"
        promotion_dir.mkdir()
        jpeg = promotion_dir / "promote.jpg"
        renamed = promotion_dir / "renamed-copy.jpg"
        jpeg.write_bytes(regression.JPEG)
        renamed.write_bytes(regression.JPEG)
        remember(jpeg)
        remember(renamed)
        promotion_total = run.state()["total"] + 2
        client.call("import", {"path": str(promotion_dir)})
        wait_for(lambda: run.state()["total"] == promotion_total and not run.state()["indexing"], 20, "identical JPEG regression pair")
        jpeg_photos = [item for item in client.call("library_list", {"scope": "all", "limit": 1000})["photos"] if item["folder"] == str(promotion_dir)]
        promoted_id = next(item["id"] for item in jpeg_photos if item["filename"] == jpeg.name)
        jpeg_ids = sorted(item["id"] for item in jpeg_photos)
        jpeg_scan = scan_done(run, jpeg_ids)
        require(jpeg_scan["groups"] == [jpeg_ids], "Identical real JPEGs did not produce an exact set")
        client.call("filter", {"filter": "all", "duplicates_only": True, "min_rating": 0, "clear_folder": True})
        require(run.state()["filtered_count"] == 2, "JPEG duplicate collection did not show both members")
        raw = promotion_dir / "promote.nef"
        tiff = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x01\0\0\0\0\0\0\0"
        raw.write_bytes(tiff + b"\0" * 1024 + regression.JPEG)
        remember(raw)
        client.call("import", {"path": str(promotion_dir)})
        def stale_promotion():
            state = run.state()
            return state if not state["indexing"] and state["duplicate_scan"]["phase"] == "stale" else None
        stale = wait_for(stale_promotion, 20, "completed duplicate results invalidated by RAW promotion")
        promoted = client.call("photo_get", {"id": promoted_id})["photo"]
        require(promoted["path"] == str(raw) and promoted["companion_path"] == str(jpeg),
                "Regression import did not promote the original JPEG identity in place")
        require(stale["total"] == promotion_total and stale["duplicates_only"]
                and stale["filtered_count"] == 0 and not stale["duplicate_scan"]["groups"]
                and not client.call("library_list")["photos"],
                "Changed logical asset remained in completed duplicate results")
        client.call("command", {"name": "next_group"})
        require(run.state()["selected_id"] is None, "Stale empty duplicate collection navigated obsolete groups")
        record("R6-3 completed JPEG duplicate set becomes stale after in-place RAW promotion", id=promoted_id,
               phase=stale["duplicate_scan"]["phase"], visible=stale["filtered_count"])

        # R6-1: this command routes the same CmdScanDuplicates used by the UI;
        # omitted IDs on the separate MCP duplicates tool intentionally mean all.
        client.call("filter", {"filter": "all", "duplicates_only": False, "folder": str(promotion_dir), "min_rating": 0})
        client.call("trash", {"ids": jpeg_ids, "trashed": True})
        run.durable()
        require(run.state()["filtered_count"] == 0, "Scoped all-trashed fixture view is not empty")
        media_before = wait_for(lambda: (lambda cache: cache if cache["active"] == 0 and cache["queued"] == 0 else None)(client.call("cache_stats")), 10, "preview workers idle before empty-view scan")
        prior = client.call("duplicates", {"action": "status"})["scan"]
        client.call("command", {"name": "scan_duplicates_in_view"})
        empty_view = run.state()
        require(empty_view["duplicate_scan"]["id"] == prior["id"]
                and empty_view["duplicate_scan"]["phase"] == prior["phase"]
                and empty_view["filtered_count"] == 0 and empty_view["folder"] == str(promotion_dir),
                "Empty scoped UI scan admitted a new catalog-wide job")
        require(empty_view["status"] == "No photos to scan in this view", "Empty scoped scan lacks no-work status")
        require(client.call("cache_stats")["source_reads"] == media_before["source_reads"],
                "Empty scoped command triggered additional preview source reads")
        record("R6-1 scoped all-trashed UI scan action admits no new scan job", job_id=prior["id"], status=empty_view["status"])
        client.call("trash", {"ids": jpeg_ids, "trashed": False})
        run.durable()
        client.call("filter", {"filter": "all", "duplicates_only": False, "clear_folder": True, "min_rating": 0})

        queued = client.call("duplicates", {"action": "scan", "ids": large_ids})
        wait_for(lambda: (lambda scan: scan and scan["id"] == queued["job_id"] and scan["phase"] == "running")(client.call("duplicates", {"action": "status"})["scan"]), 10, "scan before quit")
        began = time.monotonic()
        client.call("quit")
        run.finish()
        record("normal native quit cancels and joins an active duplicate worker", exit_seconds=time.monotonic()-began)
        require(not (configuration / "automation.json").exists(), "Private automation manifest survived quit")
        for path, expected in originals.items():
            source = pathlib.Path(path)
            require(digest(source) == expected["sha256"], "Source bytes changed: " + path)
            require(source.stat().st_mtime_ns == expected["mtime_ns"], "Source mtime changed: " + path)
        record("all original SHA256 and mtimes unchanged; private endpoint cleaned", originals=len(originals))
        report["originals"] = originals
        report["result"] = "PASS"
    finally:
        for run in active:
            run.abort()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=str(pathlib.Path(__file__).resolve().parents[1] / "target/debug/shadowroom"))
    parser.add_argument("--output", help="New output directory; defaults to private /tmp session")
    parser.add_argument("--heif-fixture", help="Existing real HEIC/HEIF file copied into private fixtures")
    parser.add_argument("--heif-dimensions", help="Expected real fixture dimensions, e.g. 1280x854")
    args = parser.parse_args()
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-native-features-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    os.chmod(root, 0o700)
    report = {"result": "RUNNING", "session": str(root), "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    print("Native feature evidence: " + str(root), flush=True)
    try:
        exercise(args, root, report)
    except Exception as error:
        report["result"] = "FAIL"
        report["failure"] = str(error)
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), flush=True)
    finally:
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"result": report["result"], "report": str(root / "report.json")}), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
