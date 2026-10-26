#!/usr/bin/env python3
"""Isolated native Linux desktop + MCP acceptance checks; Python stdlib only."""
import argparse
import hashlib
import json
import os
import pathlib
import queue
import shutil
import sqlite3
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import traceback


TOOLS = {
    "ingest_scan", "ingest_plan", "ingest_start", "ingest_status", "ingest_cancel",
    "ingest_pause", "ingest_resume", "ingest_history", "ingest_mark_imported", "eject",
    "workspace_state", "library_list", "photo_get", "select", "rate", "flag",
    "label", "undo", "redo", "view", "zoom", "pin", "filter", "group",
    "settings_get", "settings_set", "import", "export_xmp", "export_picks",
    "cancel_import", "theme_import", "cache_stats", "command", "quit",
    "trash", "duplicates",
    "remote_host_add", "remote_host_remove", "remote_hosts", "remote_status",
    "remote_connect", "remote_disconnect", "remote_cancel", "remote_roots",
    "remote_index", "remote_sync",
}

# Registry parity is checked for every session. The extended harness exercises
# ingest/network workflows separately, without pretending a registry entry is
# runtime coverage of that feature.
CORE_TOOLS = TOOLS - {name for name in TOOLS if name.startswith(("ingest_", "remote_")) or name == "eject"}


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def wait_for(function, timeout=10, description="condition"):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = function()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError("Timed out waiting for " + description)


def bmp(path, width, height, phase):
    stride = (width * 3 + 3) & ~3
    size = stride * height
    header = struct.pack("<2sIHHI", b"BM", 54 + size, 0, 0, 54)
    dib = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0, size, 2835, 2835, 0, 0)
    pixels = bytearray(size)
    for y in range(height):
        for x in range(width):
            offset = y * stride + x * 3
            pixels[offset:offset + 3] = bytes(((x + phase * 17) % 256,
                                               (y * 2 + phase * 31) % 256,
                                               (x + y + phase * 43) % 256))
    path.write_bytes(header + dib + pixels)


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65536), b""):
            hasher.update(block)
    return hasher.hexdigest()


class MCP:
    def __init__(self, binary, environment, root, number):
        self.serial = 0
        self.messages = queue.Queue()
        self.transcript = (root / ("mcp-%d.jsonl" % number)).open("w", encoding="utf-8")
        self.stderr_path = root / ("mcp-%d.stderr.log" % number)
        self.stderr = self.stderr_path.open("wb")
        self.process = subprocess.Popen(
            [str(binary), "--mcp"], env=environment, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.stderr, text=True, bufsize=1,
        )
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.used = set()

    def _read(self):
        try:
            for line in self.process.stdout:
                try:
                    value = json.loads(line)
                    require(isinstance(value, dict) and value.get("jsonrpc") == "2.0",
                            "MCP stdout contained a non-JSON-RPC packet")
                    self.messages.put(("packet", value))
                except Exception as error:
                    self.messages.put(("invalid", str(error) + ": " + repr(line)))
        finally:
            self.messages.put(("eof", None))

    def write(self, message):
        self.transcript.write(json.dumps({"direction": "request", "packet": message}) + "\n")
        self.transcript.flush()
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params=None, timeout=12):
        self.serial += 1
        serial = self.serial
        self.write({"jsonrpc": "2.0", "id": serial, "method": method,
                    "params": {} if params is None else params})
        try:
            kind, packet = self.messages.get(timeout=timeout)
        except queue.Empty:
            raise AssertionError("MCP response timeout: " + method) from None
        require(kind == "packet", "MCP stdout failure: %s %r" % (kind, packet))
        self.transcript.write(json.dumps({"direction": "response", "packet": packet}) + "\n")
        self.transcript.flush()
        require(packet.get("id") == serial,
                "Unexpected response/notification output: expected ID %d, got %r" % (serial, packet))
        return packet

    def initialize(self):
        response = self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                           "clientInfo": {"name": "shadowroom-native-smoke", "version": "1"}})
        require(response["result"]["protocolVersion"] == "2025-11-25", "Protocol negotiation failed")
        self.write({"jsonrpc": "2.0", "method": "notifications/initialized"})
        # An erroneous response to this notification fails the next ID check.
        tools = self.rpc("tools/list")["result"]["tools"]
        require(len(tools) == len(TOOLS) and {tool["name"] for tool in tools} == TOOLS,
                f"Expected the complete {len(TOOLS)}-tool registry")

    def call(self, name, arguments=None, error=False):
        self.used.add(name)
        response = self.rpc("tools/call", {"name": name, "arguments": arguments or {}})
        require("result" in response, "Unexpected JSON-RPC tool failure: %r" % response)
        result = response["result"]
        require(bool(result.get("isError")) == error,
                "Unexpected %s outcome: %r" % (name, result))
        return result if error else result["structuredContent"]

    def finish(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        self.process.wait(timeout=5)
        self.reader.join(timeout=2)
        self.stderr.close()
        self.transcript.close()
        require(self.process.returncode == 0, "MCP process failed: " + str(self.stderr_path))
        while not self.messages.empty():
            kind, packet = self.messages.get_nowait()
            require(kind == "eof", "Unsolicited MCP stdout at shutdown: %r" % ((kind, packet),))
        require(not self.stderr_path.read_bytes(), "Unexpected MCP stderr: " + str(self.stderr_path))

    def abort(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)
        for stream in (self.process.stdin, self.stderr, self.transcript):
            try:
                stream.close()
            except Exception:
                pass


class NativeRun:
    def __init__(self, binary, environment, root, fixtures, number, initial=False):
        self.stdout = (root / ("native-%d.stdout.log" % number)).open("wb")
        self.stderr = (root / ("native-%d.stderr.log" % number)).open("wb")
        arguments = [str(binary)] + (["--import", str(fixtures)] if initial else [])
        self.process = subprocess.Popen(arguments, env=environment, stdout=self.stdout, stderr=self.stderr)
        manifest = root / "config/shadowroom/automation.json"

        def ready():
            require(self.process.poll() is None, "Native app exited before readiness; inspect logs in " + str(root))
            try:
                descriptor = json.loads(manifest.read_text())
                # Never contact or close an endpoint belonging to another process.
                return descriptor.get("pid") == self.process.pid
            except (FileNotFoundError, json.JSONDecodeError):
                return False

        wait_for(ready, 20, "this app's private automation endpoint")
        require(stat.S_IMODE(manifest.stat().st_mode) == 0o600, "Descriptor must be owner-only (0600)")
        self.client = MCP(binary, environment, root, number)
        self.client.initialize()
        wait_for(lambda: self.state()["total"] == 6 and not self.state()["indexing"], 20, "six BMP photographs")

    def state(self):
        return self.client.call("workspace_state")

    def durable(self):
        return wait_for(lambda: (lambda state: state if state["persistence"]["state"] == "durable" else None)(self.state()),
                        12, "durable decisions")

    def finish(self):
        self.process.wait(timeout=12)
        require(self.process.returncode == 0, "Native process exited unsuccessfully")
        self.client.finish()
        self.stdout.close()
        self.stderr.close()

    def abort(self):
        if hasattr(self, "client"):
            self.client.abort()
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)
        self.stdout.close()
        self.stderr.close()


def writer_lock(database):
    connection = sqlite3.connect(database, timeout=1)
    connection.execute("BEGIN IMMEDIATE")
    return connection


def database_rating(database, photo_id):
    with sqlite3.connect(database) as connection:
        return connection.execute("SELECT rating FROM photos WHERE id=?", (photo_id,)).fetchone()[0]


def xdotool_path(explicit):
    if explicit:
        return pathlib.Path(explicit).resolve()
    located = shutil.which("xdotool")
    if located:
        return pathlib.Path(located)
    local = pathlib.Path.home() / ".local/share/shadowroom-build/usr/bin/xdotool"
    return local if local.is_file() else None


def close_owned_window(tool, process, environment):
    local = environment.copy()
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        local["LD_LIBRARY_PATH"] = str(libraries) + (":" + local["LD_LIBRARY_PATH"] if local.get("LD_LIBRARY_PATH") else "")

    def windows():
        require(process.poll() is None, "Own native process exited before WM close")
        result = subprocess.run([str(tool), "search", "--all", "--pid", str(process.pid), "--name", "^Shadowroom$"],
                                env=local, capture_output=True, text=True, timeout=3)
        return [line for line in result.stdout.splitlines() if line.isdigit()]

    candidates = wait_for(windows, 8, "this process's native X11 window")
    window = candidates[0]
    owner = subprocess.run([str(tool), "getwindowpid", window], env=local, capture_output=True,
                           text=True, timeout=3, check=True)
    require(owner.stdout.strip() == str(process.pid), "Refusing to close another process's window")
    # windowquit sends WM_DELETE_WINDOW. xdotool's similarly named windowclose
    # destroys the X window directly and bypasses application close confirmation.
    subprocess.run([str(tool), "windowquit", window], env=local, capture_output=True, timeout=3, check=True)
    return {"method": "xdotool windowquit (WM_DELETE_WINDOW)", "pid": process.pid, "window_id": window}


def exercise(args, root, report):
    binary = pathlib.Path(args.binary).expanduser().resolve()
    require(binary.is_file() and os.access(binary, os.X_OK), "--binary must name a built executable")
    require(sys.platform.startswith("linux"), "This acceptance harness requires native Linux")
    require(os.environ.get("DISPLAY"), "An actual Linux desktop DISPLAY is required")
    fixtures = root / "originals"
    fixtures.mkdir()
    expected = {}
    base_ns = 1700000000000000000
    for index in range(6):
        path = fixtures / ("fixture-%02d.bmp" % index)
        width, height = 256 + index * 32, 160 + index * 16
        bmp(path, width, height, index)
        os.utime(path, ns=(base_ns + index * 200000000, base_ns + index * 200000000))
        expected[str(path)] = {"width": width, "height": height, "sha256": digest(path),
                               "mtime_ns": path.stat().st_mtime_ns}
    environment = os.environ.copy()
    environment.pop("WAYLAND_DISPLAY", None)
    environment.update(RUST_BACKTRACE="1", XDG_CONFIG_HOME=str(root / "config"),
                       XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
                       XDG_STATE_HOME=str(root / "state"))
    configuration = root / "config/shadowroom"
    configuration.mkdir(parents=True)
    # Disable removable-drive imports before the first native frame/worker starts.
    (configuration / "settings.json").write_text(json.dumps({"auto_discover_drives": False,
        "auto_advance": False, "memory_budget_mb": 128, "cache_budget_mb": 128}))
    report.update(binary=str(binary), binary_size=binary.stat().st_size,
                  binary_mtime_ns=binary.stat().st_mtime_ns, display=environment["DISPLAY"],
                  backend="native X11 (WAYLAND_DISPLAY removed)", fixtures=expected)
    active = []

    def record(name, **details):
        report["checks"].append({"name": name, "result": "PASS", **details})
        print("PASS " + name, flush=True)

    def launch(number, initial=False):
        run = NativeRun.__new__(NativeRun)
        active.append(run)
        NativeRun.__init__(run, binary, environment, root, fixtures, number, initial)
        return run

    try:
        run = launch(1, True)
        client = run.client
        record("native startup, MCP initialize/notification/stdout purity, %d tools and 0600 descriptor" % len(TOOLS) + "")
        photos = client.call("library_list", {"scope": "all", "limit": 6})["photos"]
        ids = [photo["id"] for photo in photos]
        require(len(set(ids)) == 6, "Expected six unique catalog IDs")
        a, b = ids[:2]
        database = next((root / "data").rglob("catalog.sqlite3"))
        for photo in photos:
            wanted = expected[photo["path"]]
            require((photo["width"], photo["height"]) == (wanted["width"], wanted["height"]), "BMP dimensions wrong")
            require(photo.get("metadata_version", 0) >= 1, "Rebuildable metadata revision missing")
            require(photo["captured_at"] is None and photo.get("captured_offset") is None,
                    "BMP should not acquire an invented camera timestamp/offset")
            require(photo.get("capture_time_kind") == "" and photo["camera"] == "" and photo["orientation"] == 1,
                    "BMP camera/provenance defaults wrong")
            require(client.call("photo_get", {"id": photo["id"]})["photo"]["path"] == photo["path"], "photo_get mismatch")
        record("BMP metadata dimensions, revision and unknown-camera/time provenance")
        page = client.call("library_list", {"scope": "all", "offset": 2, "limit": 2})
        require(len(page["photos"]) == 2 and page["next_offset"] == 4, "Pagination wrong")
        before = run.state()["persistence"]["revision"]
        for name, arguments in [
            ("rate", {"rating": 4, "ids": [a, 99999999]}), ("rate", {"rating": "bad"}),
            ("rate", {"rating": True}), ("select", {"ids": [a, "bad"]}),
            ("photo_get", {"id": 99999999}), ("label", {"label": "unknown"}),
            ("settings_set", {"settings": {"memory_budget_mb": "bad"}}),
            ("filter", {"min_rating": 6}), ("command", {"name": "arbitrary_shell"}),
        ]:
            client.call(name, arguments, error=True)
        require(run.state()["persistence"]["revision"] == before, "Invalid request mutated persistence revision")
        require(client.call("photo_get", {"id": a})["photo"]["rating"] == 0, "Mixed invalid IDs partially changed a photo")
        unknown = client.rpc("tools/call", {"name": "shell", "arguments": {}})
        require(unknown.get("error", {}).get("code") == -32602, "Unknown tool must fail before dispatch")
        record("pagination and invalid IDs/types/commands rejected before mutation")

        connection = writer_lock(database)
        timings = []
        try:
            for rating in [5, 2, 5]:
                began = time.monotonic()
                result = client.call("rate", {"rating": rating, "ids": [a]})
                timings.append(time.monotonic() - began)
                require(result["persistence"]["state"] == "pending", "Locked writer must report pending")
            require(max(timings) < 1.0, "UI blocked behind SQLite writer")
            state = run.state()
            require(a in state["persistence"]["unsaved_ids"] and state["persistence"]["state"] == "pending",
                    "Pending optimistic decision missing")
        finally:
            connection.rollback()
            connection.close()
        run.durable()
        require(database_rating(database, a) == 5, "5→2→5 did not persist final value")
        client.call("undo")
        run.durable()
        require(database_rating(database, a) == 2, "Undo did not persist 2")
        client.call("redo")
        run.durable()
        require(database_rating(database, a) == 5, "Redo did not persist 5")
        record("real SQLite writer lock: responsive 5→2→5, durable latest value, undo/redo", call_seconds=timings)

        connection = writer_lock(database)
        try:
            client.call("rate", {"rating": 3, "ids": [b]})
            failed = wait_for(lambda: (lambda state: state if state["persistence"]["state"] == "failed" else None)(run.state()),
                              10, "real SQLite busy-timeout failure")
            require(b in failed["persistence"]["unsaved_ids"], "Failed decision was lost")
            client.call("ingest_plan", {"options": {}}, error=True)
            client.call("ingest_start", error=True)
            client.call("quit")
            require(run.process.poll() is None and not run.state()["closing"], "Failed unsaved decision must prevent quit")
        finally:
            connection.rollback()
            connection.close()
        client.call("command", {"name": "retry_saves"})
        run.durable()
        require(database_rating(database, b) == 3, "Retry did not persist failed decision")
        record("actual >5s busy timeout, failed/unsaved IDs, blocked quit and retry_saves recovery")

        client.call("select", {"ids": [a, b]})
        client.call("flag", {"flag": "pick", "ids": [a, b]})
        client.call("label", {"label": "blue", "ids": [a, b]})
        client.call("label", {"label": "purple", "ids": [a]})
        run.durable()
        require(client.call("photo_get", {"id": a})["photo"]["label"] == "Purple", "Purple label missing")
        require(client.call("photo_get", {"id": b})["photo"]["label"] == "Blue", "Batch blue label missing")
        # Trash follows the same durable decision pipeline and preserves review data.
        client.call("trash", {"ids": [b], "trashed": True})
        run.durable()
        trashed = client.call("photo_get", {"id": b})["photo"]
        require(trashed["trashed"] and trashed["label"] == "Blue" and trashed["rating"] == 3,
                "In-app Trash lost prior review data")
        client.call("trash", {"ids": [b], "trashed": False})
        run.durable()
        require(not client.call("photo_get", {"id": b})["photo"]["trashed"], "Restore failed")
        client.call("duplicates", {"action": "status"})
        client.call("filter", {"filter": "picks", "min_rating": 2})
        require(run.state()["filtered_count"] == 2, "Pick/star filter wrong")
        client.call("filter", {"filter": "all", "min_rating": 0, "clear_folder": True})
        client.call("select", {"id": a})
        client.call("command", {"name": "next"})
        require(run.state()["selected_id"] == b, "Next navigation wrong")
        client.call("command", {"name": "previous"})
        require(run.state()["selected_id"] == a, "Previous navigation wrong")
        client.call("pin")
        client.call("select", {"id": b})
        client.call("view", {"mode": "compare"})
        client.call("zoom", {"mode": "full"})
        state = run.state()
        require(state["view"] == "compare" and state["zoom"] == "full" and state["reference_id"] == a,
                "Pin/compare/full state wrong")
        sync = state.get("compare_sync")
        require(isinstance(sync, bool), "Compare synchronization state must be inspectable")
        client.call("command", {"name": "toggle_compare_sync"})
        require(run.state()["compare_sync"] != sync, "Compare synchronization toggle failed")
        prior_log = client.call("settings_get").get("histogram_log", False)
        client.call("command", {"name": "toggle_histogram_log"})
        require(client.call("settings_get").get("histogram_log") != prior_log, "Histogram log toggle failed")
        client.call("group", {"mode": "time", "gap_ms": 2000})
        grouped = wait_for(lambda: (lambda state: state if state["groups"] else None)(run.state()),
                           6, "background time grouping")
        require(any(set(group) == set(ids) for group in grouped["groups"]), "Expected one temporal BMP group")
        client.call("command", {"name": "next_group"})
        client.call("group", {"mode": "off"})
        client.call("view", {"mode": "grid"})
        client.call("import", {"path": str(fixtures)})
        client.call("cancel_import")
        wait_for(lambda: not run.state()["indexing"], 6, "import cancellation/finish")
        require(run.state()["total"] == 6, "Reimport created duplicate photos")
        record("multi-ID flags/labels, filters, navigation, pin/compare/full, sync/log toggles, grouping, import/cancel")

        cache = wait_for(lambda: (lambda value: value if value["decoded"] > 0 else None)(client.call("cache_stats")),
                         8, "native previews to decode")
        require(cache["memory_bytes"] <= cache["memory_budget_bytes"], "Retained preview cache exceeded budget")
        for tool, arguments in [("export_picks", {}), ("export_xmp", {"ids": [a, b]})]:
            destination = root / tool
            job = client.call(tool, {"destination": str(destination), **arguments})
            require(job["state"] == "queued", "Export must return queued acceptance")
            job_id = job["job_id"]
            final = wait_for(lambda: next((item for item in run.state()["export_jobs"]
                                           if item["id"] == job_id and item["state"] in ("completed", "failed")), None),
                             10, tool + " tracked completion")
            require(final["state"] == "completed" and final["completed"] == 2, "Tracked export failed: %r" % final)
        for photo_id in [a, b]:
            source = pathlib.Path(client.call("photo_get", {"id": photo_id})["photo"]["path"])
            require(digest(root / "export_picks" / source.name) == expected[str(source)]["sha256"], "Export bytes differ")
        require((root / "export_picks/shadowroom-picks-manifest.json").is_file(), "Picks manifest missing")
        require((root / "export_xmp/shadowroom-xmp-manifest.json").is_file(), "XMP manifest missing")
        for source, wanted in expected.items():
            require(digest(pathlib.Path(source)) == wanted["sha256"], "An original changed")
            require(pathlib.Path(source).stat().st_mtime_ns == wanted["mtime_ns"], "An original modification time changed")
        record("native preview/cache stats, tracked picks/XMP completion, manifests and original/output hashes", cache=cache)

        family = root / "smoke-theme.json"
        family.write_text(json.dumps({"name": "Native Smoke", "author": "Shadowroom native validation",
            "themes": [{"name": "Native Smoke Dark", "appearance": "dark", "style": {
                "background": "#0f0f0f", "text": "#e6e6e6", "text.accent": "#5b9bd580",
                "panel.background": "#141414", "syntax": {"comment": {"color": "#8a8a8a", "font_style": "italic"}},
                "future": {"kept": [1, None]}}}]}))
        require(client.call("theme_import", {"path": str(family)})["persisted_family"], "Theme family not persisted")
        client.call("settings_set", {"settings": {"interface_font": "DM Mono", "theme_mode": "manual",
            "filter": "picks", "min_rating": 2, "view_mode": "loupe", "auto_advance": False,
            "auto_discover_drives": False, "grouping_mode": "off"}})
        client.call("zoom", {"mode": "fit"})
        client.call("select", {"id": b})
        run.durable()
        require(client.call("settings_get")["interface_font"] == "DM Mono", "Font setting not active")
        require(list((configuration / "themes").glob("*.json")), "No persisted imported family")
        connection = writer_lock(database)
        try:
            client.call("rate", {"rating": 4, "ids": [a]})
            client.call("quit")
            require(run.state()["closing"] and run.process.poll() is None, "Quit did not await pending writer")
        finally:
            connection.rollback()
            connection.close()
        used = set(client.used)
        run.finish()
        require(database_rating(database, a) == 4, "MCP quit lost pending decision")
        family.unlink()  # A restart must use the saved copy, not this import path.
        record("MCP pending-write quit barrier and normal native process exit")

        run = launch(2)
        client = run.client
        wait_for(lambda: run.state()["theme"] == "Native Smoke Dark", 6, "imported theme after restart")
        state = run.state()
        require(state["interface_font"] == "DM Mono" and state["filter"] == "picks" and state["min_rating"] == 2,
                "Font/filter preferences did not survive restart")
        require(state["filtered_count"] == 2 and state["view"] == "loupe", "Persisted view/filter wrong")
        require(database_rating(database, a) == 4 and database_rating(database, b) == 3, "Decisions did not survive restart")
        require(client.call("photo_get", {"id": a})["photo"]["label"] == "Purple", "Labels did not survive restart")
        record("restart preserves decisions, labels, imported family, font, filter and view")
        tool = xdotool_path(args.xdotool)
        if tool is not None:
            connection = writer_lock(database)
            try:
                client.call("rate", {"rating": 2, "ids": [b]})
                close = close_owned_window(tool, run.process, environment)
                wait_for(lambda: run.state()["closing"], 3, "WM close entering pending-write gate")
                require(run.process.poll() is None, "WM close discarded a blocked write")
            finally:
                connection.rollback()
                connection.close()
            run.finish()
            require(database_rating(database, b) == 2, "WM close lost pending decision")
            record("native WM close on own PID waits for pending decision durability", **close)
            run = launch(3)
            client = run.client
            require(client.call("photo_get", {"id": b})["photo"]["rating"] == 2, "WM-close decision lost on relaunch")
            client.call("quit")
            run.finish()
            record("relaunch confirms WM-close decision")
        else:
            report["checks"].append({"name": "native WM close", "result": "SKIP", "reason": "xdotool unavailable"})
            client.call("quit")
            run.finish()
        require(CORE_TOOLS <= used, "Core MCP coverage missing: " + repr(sorted(CORE_TOOLS - used)))
        report["tools_registered"] = sorted(TOOLS)
        report["tools_exercised"] = sorted(used)
        for source, wanted in expected.items():
            require(digest(pathlib.Path(source)) == wanted["sha256"], "An original changed across restart/close")
            require(pathlib.Path(source).stat().st_mtime_ns == wanted["mtime_ns"], "An original modification time changed across restart/close")
        require(len(list(fixtures.iterdir())) == 6, "Export wrote unexpected files beside originals")
        report["result"] = "PASS"
    finally:
        for run in reversed(active):
            try:
                run.abort()
            except Exception as error:
                report.setdefault("cleanup_errors", []).append(str(error))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="Built native executable; never builds or downloads dependencies")
    parser.add_argument("--xdotool", help="Optional xdotool executable for native own-PID WM close")
    parser.add_argument("--output", help="New directory for isolated profile and evidence")
    args = parser.parse_args()
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-native-validation-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = {"result": "RUNNING", "session": str(root), "checks": []}
    started = time.monotonic()
    print("Validation evidence: " + str(root), flush=True)
    try:
        exercise(args, root, report)
    except Exception as error:
        report["result"] = "FAIL"
        report["failure"] = str(error)
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), file=sys.stderr)
    report["elapsed_seconds"] = time.monotonic() - started
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"result": report["result"], "report": str(root / "report.json"),
                      "checks": len(report["checks"]), "seconds": round(report["elapsed_seconds"], 2)}), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
