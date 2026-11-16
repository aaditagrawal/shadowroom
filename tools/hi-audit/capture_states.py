#!/usr/bin/env python3
"""Deterministic human-interface screenshot harness for Shadowroom (Linux, X11).

Usage:
    uv run --no-project python capture_states.py --binary <shadowroom> \
        --photos <demo-photo-dir> --out <dir>

What it does
  * Starts a private rootful Xwayland server (falls back to the current DISPLAY
    when Xwayland/WAYLAND_DISPLAY are unavailable) so synthetic input never
    reaches the user's own windows, then launches the app on it with a private
    HOME and XDG_{CONFIG,DATA,CACHE,STATE}_HOME.  The user's real profile is
    never read or written; the harness verifies that before/after.
  * Copies the demo photos into a fixed session directory (originals are never
    touched) and drives the app through its MCP stdio control server, using
    xdotool keys/clicks only for states MCP cannot reach.
  * Captures every state with XGetImage (stdlib ctypes), waits for the frame to
    settle, and writes PNG + manifest.json with state id, description, theme,
    window size, how it was reached, sha256 (file and pixels), workspace_state
    checks, duplicate detection and transition timings:
       - mcp_rtt_ms:      tools/call send -> JSON-RPC response
       - frame_change_ms: action send -> first window frame differing from the
                          frame captured just before the action
       - settle_ms:       action send -> frame stable (no change for 1.1 s;
                          blinking carets are detected and resolved to the
                          caret-visible frame).
Python stdlib only.
"""
from __future__ import annotations

import argparse
import contextlib
import ctypes
import ctypes.util
import hashlib
import json
import os
import queue
import shutil
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
DARK, LIGHT = "Shadowroom Dark", "Shadowroom Light"
LARGE, SMALL = (1440, 900), (1024, 680)
ROOT_SIZE = (1600, 1000)          # private X screen; window sits at 0,0
PARK = (300, 21)                  # neutral title-bar spot inside the window (tooltips do not
                                  # dismiss when the pointer leaves the window, so never park outside)
STABLE_S = 1.1                    # frame must be unchanged this long to settle
PRE_STABLE_S = 0.35               # quieter settle used before a transition


class Skip(Exception):
    pass


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def wait_for(fn, timeout=10.0, what="condition", interval=0.05):
    deadline = time.monotonic() + timeout
    last_error = None
    while time.monotonic() < deadline:
        try:
            value = fn()
            if value:
                return value
        except Exception as error:  # transient MCP/X errors while waiting
            last_error = error
        time.sleep(interval)
    raise AssertionError(f"Timed out waiting for {what}" + (f" ({last_error})" if last_error else ""))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 16), b""):
            digest.update(block)
    return digest.hexdigest()


# --------------------------------------------------------------------------- X11
class _Funcs(ctypes.Structure):
    _fields_ = [("create_image", ctypes.c_void_p),
                ("destroy_image", ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p)),
                ("get_pixel", ctypes.c_void_p), ("put_pixel", ctypes.c_void_p),
                ("sub_image", ctypes.c_void_p), ("add_pixel", ctypes.c_void_p)]


class _XImage(ctypes.Structure):
    _fields_ = [("width", ctypes.c_int), ("height", ctypes.c_int), ("xoffset", ctypes.c_int),
                ("format", ctypes.c_int), ("data", ctypes.c_void_p), ("byte_order", ctypes.c_int),
                ("bitmap_unit", ctypes.c_int), ("bitmap_bit_order", ctypes.c_int),
                ("bitmap_pad", ctypes.c_int), ("depth", ctypes.c_int),
                ("bytes_per_line", ctypes.c_int), ("bits_per_pixel", ctypes.c_int),
                ("red_mask", ctypes.c_ulong), ("green_mask", ctypes.c_ulong),
                ("blue_mask", ctypes.c_ulong), ("obdata", ctypes.c_void_p), ("f", _Funcs)]


class Frame:
    __slots__ = ("width", "height", "rgb", "key", "at")

    def __init__(self, width, height, rgb, at):
        self.width, self.height, self.rgb, self.at = width, height, rgb, at
        self.key = hashlib.blake2b(rgb, digest_size=16).hexdigest()


class XDisplay:
    """Minimal libX11 binding: window geometry and XGetImage grabs."""

    def __init__(self, display_name: str):
        lib = ctypes.util.find_library("X11") or "libX11.so.6"
        self.x = x = ctypes.CDLL(lib)
        x.XInitThreads()
        x.XOpenDisplay.argtypes = [ctypes.c_char_p]
        x.XOpenDisplay.restype = ctypes.c_void_p
        x.XGetImage.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_int,
                                ctypes.c_uint, ctypes.c_uint, ctypes.c_ulong, ctypes.c_int]
        x.XGetImage.restype = ctypes.POINTER(_XImage)
        x.XGetGeometry.argtypes = [ctypes.c_void_p, ctypes.c_ulong] + [ctypes.c_void_p] * 7
        x.XGetGeometry.restype = ctypes.c_int
        x.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
        x.XCloseDisplay.argtypes = [ctypes.c_void_p]
        handler_type = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p)
        self.errors = 0

        def on_error(_display, _event):
            self.errors += 1
            return 0

        self._handler = handler_type(on_error)  # keep a reference alive
        x.XSetErrorHandler(self._handler)
        self.display = x.XOpenDisplay(display_name.encode())
        require(self.display, f"Cannot open X display {display_name}")
        self.lock = threading.Lock()

    def geometry(self, window: int):
        root, xx, yy = ctypes.c_ulong(), ctypes.c_int(), ctypes.c_int()
        w, h, border, depth = ctypes.c_uint(), ctypes.c_uint(), ctypes.c_uint(), ctypes.c_uint()
        with self.lock:
            ok = self.x.XGetGeometry(self.display, window, ctypes.byref(root), ctypes.byref(xx),
                                     ctypes.byref(yy), ctypes.byref(w), ctypes.byref(h),
                                     ctypes.byref(border), ctypes.byref(depth))
        require(ok, "XGetGeometry failed")
        return w.value, h.value

    def grab(self, window: int) -> Frame:
        width, height = self.geometry(window)
        with self.lock:
            image = self.x.XGetImage(self.display, window, 0, 0, width, height,
                                     ctypes.c_ulong(0xFFFFFFFF), 2)  # ZPixmap
            require(bool(image), "XGetImage failed (window unmapped?)")
            try:
                img = image.contents
                require(img.bits_per_pixel == 32, f"Unsupported X visual bpp {img.bits_per_pixel}")
                stride = img.bytes_per_line
                raw = ctypes.string_at(img.data, stride * height)
            finally:
                image.contents.f.destroy_image(image)
        at = time.monotonic()
        if stride != width * 4:
            raw = b"".join(raw[row * stride: row * stride + width * 4] for row in range(height))
        rgb = bytearray(width * height * 3)
        rgb[0::3], rgb[1::3], rgb[2::3] = raw[2::4], raw[1::4], raw[0::4]  # BGRX -> RGB
        return Frame(width, height, bytes(rgb), at)

    def close(self):
        with contextlib.suppress(Exception):
            self.x.XCloseDisplay(self.display)


def write_png(path: Path, frame: Frame):
    """Deterministic PNG: ImageMagick (adaptive filters) if present, else stdlib."""
    if shutil.which("magick"):
        subprocess.run(["magick", "-size", f"{frame.width}x{frame.height}", "-depth", "8",
                        "rgb:-", "-strip", "-define", "png:exclude-chunks=date,time",
                        f"png24:{path}"], input=frame.rgb, check=True, capture_output=True,
                       timeout=30)
        return
    stride = frame.width * 3
    rows = b"".join(b"\x00" + frame.rgb[y * stride:(y + 1) * stride] for y in range(frame.height))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(
            ">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", frame.width, frame.height, 8, 2, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
                     + chunk(b"IDAT", zlib.compress(rows, 6)) + chunk(b"IEND", b""))


def diff_box(a: Frame, b: Frame):
    """Bounding box (x0, y0, x1, y1) of differing pixels, sampled per row."""
    if (a.width, a.height) != (b.width, b.height):
        return None
    stride = a.width * 3
    ys = [y for y in range(a.height) if a.rgb[y * stride:(y + 1) * stride] != b.rgb[y * stride:(y + 1) * stride]]
    if not ys:
        return None
    x0, x1 = a.width, 0
    for y in ys:
        ra, rb = a.rgb[y * stride:(y + 1) * stride], b.rgb[y * stride:(y + 1) * stride]
        first = next(i for i in range(0, stride, 3) if ra[i:i + 3] != rb[i:i + 3]) // 3
        last = next(i for i in range(stride - 3, -1, -3) if ra[i:i + 3] != rb[i:i + 3]) // 3
        x0, x1 = min(x0, first), max(x1, last)
    return x0, ys[0], x1 + 1, ys[-1] + 1


def region_contrast(frame: Frame, box):
    x0, y0, x1, y1 = box
    stride = frame.width * 3
    values = []
    for y in range(y0, y1):
        row = frame.rgb[y * stride + x0 * 3: y * stride + x1 * 3]
        values.extend(row[1::3])  # green channel approximates luminance
    if not values:
        return 0.0
    mean = sum(values) / len(values)
    return sum((v - mean) ** 2 for v in values) / len(values)


# --------------------------------------------------------------------------- MCP
class MCP:
    def __init__(self, binary: Path, env: dict, log_dir: Path, number: int):
        self.serial = 0
        self.messages: queue.Queue = queue.Queue()
        self.transcript = (log_dir / f"mcp-{number}.jsonl").open("w", encoding="utf-8")
        self.stderr = (log_dir / f"mcp-{number}.stderr.log").open("wb")
        self.process = subprocess.Popen([str(binary), "--mcp"], env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.stderr, text=True,
                                        bufsize=1)
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.last_rtt_ms = None

    def _read(self):
        try:
            for line in self.process.stdout:
                try:
                    self.messages.put(("packet", json.loads(line)))
                except Exception as error:
                    self.messages.put(("invalid", f"{error}: {line!r}"))
        finally:
            self.messages.put(("eof", None))

    def _write(self, message):
        self.transcript.write(json.dumps({"dir": "out", "t": time.time(), "packet": message}) + "\n")
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params=None, timeout=25.0):
        self.serial += 1
        serial = self.serial
        began = time.monotonic()
        self._write({"jsonrpc": "2.0", "id": serial, "method": method, "params": params or {}})
        while True:
            try:
                kind, packet = self.messages.get(timeout=timeout)
            except queue.Empty:
                raise AssertionError(f"MCP timeout: {method}") from None
            require(kind == "packet", f"MCP stdout failure: {kind} {packet!r}")
            if packet.get("id") == serial:
                break
        self.last_rtt_ms = (time.monotonic() - began) * 1000
        self.transcript.write(json.dumps({"dir": "in", "t": time.time(), "rtt_ms": round(self.last_rtt_ms, 2),
                                          "packet": packet})[:20000] + "\n")
        self.transcript.flush()
        return packet

    def initialize(self):
        response = self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                           "clientInfo": {"name": "shadowroom-hi-capture", "version": "1"}})
        require("result" in response, f"initialize failed: {response}")
        self._write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call(self, name, arguments=None, allow_error=False):
        response = self.rpc("tools/call", {"name": name, "arguments": arguments or {}})
        require("result" in response, f"JSON-RPC failure for {name}: {response}")
        result = response["result"]
        if result.get("isError"):
            text = " ".join(c.get("text", "") for c in result.get("content", []))
            if allow_error:
                return {"__error__": text}
            raise AssertionError(f"{name}({arguments}) failed: {text}")
        return result.get("structuredContent", {})

    def close(self):
        with contextlib.suppress(Exception):
            self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        for stream in (self.stderr, self.transcript):
            with contextlib.suppress(Exception):
                stream.close()


# --------------------------------------------------------------------------- harness
class Harness:
    def __init__(self, args):
        self.args = args
        self.binary = Path(args.binary).expanduser().resolve()
        self.photos_src = Path(args.photos).expanduser().resolve()
        self.out = Path(args.out).expanduser().resolve()
        self.session = Path(args.session).expanduser().resolve()
        require(self.binary.is_file() and os.access(self.binary, os.X_OK), "--binary must be executable")
        require(self.photos_src.is_dir(), "--photos must be a directory")
        self.only = set(args.only.split(",")) if args.only else None
        self.xserver = None
        self.app = None
        self.mcp = None
        self.window = None
        self.launches = 0
        self.records: list[dict] = []
        self.launch_timings: list[dict] = []
        self.notes: list[str] = []
        self.theme = DARK
        self.size = LARGE
        self.pass_name = ""
        self.ids: list[int] = []
        self.by_name: dict[str, int] = {}
        self.transition = None
        self.cleanups = []
        self.theme_switches: list[dict] = []

    # ---------------------------------------------------------------- environment
    def prepare_session(self):
        if self.session.exists():
            shutil.rmtree(self.session)
        for sub in ("home", "config/shadowroom", "data", "cache", "state", "logs", "Pictures/Exports",
                    "Pictures/Library/2025 Archive", "Pictures/Library/2026 Travel"):
            (self.session / sub).mkdir(parents=True, exist_ok=True)
        shoot = self.session / "Pictures/Demo Shoot"
        card = self.session / "Pictures/Card"
        shoot.mkdir(parents=True)
        card.mkdir(parents=True)
        sources = sorted(p for p in self.photos_src.iterdir()
                         if p.is_file() and p.suffix.lower() in (".jpg", ".jpeg", ".png", ".tif", ".tiff",
                                                                  ".heic", ".dng", ".cr3", ".nef", ".arw"))
        require(len(sources) >= 4, "Need at least four demo photos")
        self.originals = {str(p): (sha256_file(p), p.stat().st_mtime_ns) for p in sources}
        for p in sources:
            shutil.copy2(p, shoot / p.name)
            shutil.copy2(p, card / p.name)
        # One byte-identical copy gives the Duplicates view real results.
        dup = shoot / (sources[-1].stem + " copy" + sources[-1].suffix)
        shutil.copy2(sources[-1], dup)
        self.shoot, self.card = shoot, card
        settings = {
            "auto_discover_drives": False, "auto_advance": False, "grouping_mode": "off",
            "theme_mode": "manual", "theme": DARK, "memory_budget_mb": 512,
            "ingest_library_root": str(self.session / "Pictures/Library"),
            "ingest_on_insert": "ask",
        }
        (self.session / "config/shadowroom/settings.json").write_text(json.dumps(settings, indent=2))
        env = os.environ.copy()
        for key in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET"):
            env.pop(key, None)
        env.update({
            "HOME": str(self.session / "home"),
            "XDG_CONFIG_HOME": str(self.session / "config"),
            "XDG_DATA_HOME": str(self.session / "data"),
            "XDG_CACHE_HOME": str(self.session / "cache"),
            "XDG_STATE_HOME": str(self.session / "state"),
            "GPUI_X11_SCALE_FACTOR": "1",
            "RUST_BACKTRACE": "1",
            "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "TZ": "UTC",
        })
        self.env = env

    def real_profile_snapshot(self):
        home = Path(os.path.expanduser("~"))
        roots = [Path(os.environ.get("XDG_CONFIG_HOME", home / ".config")) / "shadowroom",
                 Path(os.environ.get("XDG_DATA_HOME", home / ".local/share")) / "shadowroom",
                 Path(os.environ.get("XDG_CACHE_HOME", home / ".cache")) / "shadowroom",
                 Path(os.environ.get("XDG_STATE_HOME", home / ".local/state")) / "shadowroom",
                 home / "Pictures/Shadowroom"]
        snapshot = {}
        for root in roots:
            if not root.exists():
                snapshot[str(root)] = None
                continue
            entries = {}
            for path in sorted(root.rglob("*")):
                with contextlib.suppress(OSError):
                    st = path.lstat()
                    entries[str(path.relative_to(root))] = [st.st_size, st.st_mtime_ns]
            st = root.stat()
            snapshot[str(root)] = {"mtime_ns": st.st_mtime_ns, "entries": entries}
        return snapshot

    def start_xserver(self):
        self.display = os.environ.get("DISPLAY", "")
        if self.args.use_current_display or not (shutil.which("Xwayland") and os.environ.get("WAYLAND_DISPLAY")):
            require(self.display, "No DISPLAY and no private Xwayland available")
            self.notes.append(f"Using existing DISPLAY {self.display}; synthetic input targets the focused window")
            return
        number = next(n for n in range(87, 200)
                      if not Path(f"/tmp/.X11-unix/X{n}").exists() and not Path(f"/tmp/.X{n}-lock").exists())
        log = (self.session / "logs/xwayland.log").open("wb")
        self.xserver = subprocess.Popen(
            ["Xwayland", f":{number}", "-geometry", f"{ROOT_SIZE[0]}x{ROOT_SIZE[1]}",
             "-nolisten", "tcp", "-noreset", "-decorate"],
            stdout=log, stderr=log, start_new_session=True)
        wait_for(lambda: Path(f"/tmp/.X11-unix/X{number}").exists(), 10, "private Xwayland socket")
        self.display = f":{number}"
        time.sleep(0.5)

    def xdotool_path(self):
        if self.args.xdotool:
            return Path(self.args.xdotool)
        found = shutil.which("xdotool")
        if found:
            return Path(found)
        local = Path.home() / ".local/share/shadowroom-build/usr/bin/xdotool"
        require(local.is_file(), "xdotool not found (pass --xdotool)")
        return local

    def setup_tools(self):
        self.env["DISPLAY"] = self.display
        self.tool = self.xdotool_path()
        self.tool_env = dict(self.env)
        lib64 = self.tool.parent.parent / "lib64"
        if lib64.is_dir():
            self.tool_env["LD_LIBRARY_PATH"] = str(lib64) + (
                ":" + self.env["LD_LIBRARY_PATH"] if self.env.get("LD_LIBRARY_PATH") else "")
        self.xd_conn = XDisplay(self.display)

    def xd(self, *arguments, check=True):
        result = subprocess.run([str(self.tool), *map(str, arguments)], env=self.tool_env,
                                capture_output=True, text=True, timeout=10)
        if check and result.returncode != 0:
            raise AssertionError(f"xdotool {arguments} failed: {result.stderr.strip()}")
        return result.stdout.strip()

    # ---------------------------------------------------------------- app lifecycle
    def launch(self, size=LARGE):
        self.launches += 1
        number = self.launches
        logs = self.session / "logs"
        manifest = self.session / "config/shadowroom/automation.json"
        with contextlib.suppress(FileNotFoundError):
            manifest.unlink()
        began = time.monotonic()
        self.app = subprocess.Popen([str(self.binary)], env=self.env,
                                    stdout=(logs / f"app-{number}.stdout.log").open("wb"),
                                    stderr=(logs / f"app-{number}.stderr.log").open("wb"),
                                    start_new_session=True)

        def ready():
            require(self.app.poll() is None, f"App exited during launch; see {logs}")
            try:
                return json.loads(manifest.read_text()).get("pid") == self.app.pid
            except (FileNotFoundError, json.JSONDecodeError):
                return False

        wait_for(ready, 30, "private automation endpoint")
        endpoint_ms = (time.monotonic() - began) * 1000
        self.mcp = MCP(self.binary, self.env, logs, number)
        self.mcp.initialize()

        def find_window():
            out = self.xd("search", "--all", "--pid", self.app.pid, "--name", "Shadowroom$", check=False)
            lines = [line for line in out.splitlines() if line.isdigit()]
            return lines[0] if lines else None

        self.window = int(wait_for(find_window, 15, "app window"))
        require(self.xd("getwindowpid", self.window) == str(self.app.pid), "Window belongs to another process")
        self.xd("windowmove", self.window, 0, 0)
        self.resize(size)
        self.xd("windowfocus", self.window, check=False)
        self.park_pointer()
        first = self.grab()
        frames = self.settle(first, STABLE_S, timeout=10)
        timing = {"launch": number, "endpoint_ready_ms": round(endpoint_ms, 1),
                  "settled_frame_ms": round((frames[-1].at - began) * 1000, 1)}
        self.launch_timings.append(timing)
        return timing

    def resize(self, size):
        self.size = size
        self.xd("windowsize", self.window, size[0], size[1])
        wait_for(lambda: self.xd_conn.geometry(self.window) == size, 5, f"window size {size}")
        wait_for(lambda: (lambda s: s["layout"]["workspace_width"] == size[0]
                          and s["layout"]["workspace_height"] == size[1])(self.state()),
                 5, "app layout to follow window size")

    def park_pointer(self):
        self.xd("mousemove", PARK[0], PARK[1], check=False)

    def quit_app(self):
        if self.app is None:
            return
        if self.app.poll() is None and self.mcp is not None:
            with contextlib.suppress(Exception):
                self.mcp.call("quit", allow_error=True)
            try:
                self.app.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.notes.append(f"launch {self.launches}: app did not exit within 20 s of MCP quit; terminated")
                self.kill_app()
        if self.mcp:
            self.mcp.close()
        self.mcp = None
        self.app = None

    def kill_app(self):
        if self.app is not None and self.app.poll() is None:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(self.app.pid, signal.SIGTERM)
            try:
                self.app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(self.app.pid, signal.SIGKILL)
                self.app.wait(timeout=5)

    # ---------------------------------------------------------------- MCP helpers
    def call(self, name, arguments=None, allow_error=False):
        return self.mcp.call(name, arguments, allow_error)

    def cmd(self, name):
        return self.call("command", {"name": name})

    def state(self):
        return self.call("workspace_state")

    def previews_idle(self):
        stats = self.call("cache_stats")
        return stats.get("queued", 0) == 0 and stats.get("active", 0) == 0

    def durable(self, timeout=15):
        wait_for(lambda: self.state()["persistence"]["state"] == "durable", timeout, "durable decisions")

    # ---------------------------------------------------------------- frames
    def grab(self) -> Frame:
        return self.xd_conn.grab(self.window)

    def watch(self, quiet: float, timeout: float = 8.0, start: Frame | None = None,
              until=None):
        """Poll frames until no *novel* frame has appeared for `quiet` seconds.

        Revisiting an already-seen frame (a blinking caret or indicator) does not
        reset the quiet period, so a two-state blink still settles.  Returns a dict
        with the latest frame, recent distinct frames, first-change time relative
        to `start`, time of the last novel frame and whether it settled.
        """
        frame = start or self.grab()
        origin = frame.key
        seen = {frame.key}
        recent: dict[str, Frame] = {frame.key: frame}
        first_change = None
        last_novel = frame.at
        changes = 0
        deadline = time.monotonic() + timeout
        settled = False
        while time.monotonic() < deadline:
            time.sleep(0.012)
            current = self.grab()
            if current.key != frame.key:
                changes += 1
                if first_change is None and current.key != origin:
                    first_change = current.at
                if current.key not in seen:
                    seen.add(current.key)
                    last_novel = current.at
                recent.pop(current.key, None)
                recent[current.key] = current
                while len(recent) > 3:
                    recent.pop(next(iter(recent)))
            frame = current
            if until is not None and not until(first_change):
                continue
            if frame.at - last_novel >= quiet:
                settled = True
                break
        return {"frame": frame, "recent": recent, "first_change": first_change,
                "last_novel": last_novel, "settled": settled, "changes": changes, "distinct": len(seen)}

    def settle(self, start: Frame, quiet: float, timeout: float = 8.0):
        return [self.watch(quiet, timeout, start)["frame"]]

    def go(self, how: str, action, kind="mcp", min_wait=2.5):
        """Run the measured state transition: action in a worker thread while this
        thread polls the window for the first changed frame and the settle point."""
        before = self.watch(PRE_STABLE_S, 4)["frame"]
        box: dict = {}

        def worker():
            try:
                box["result"] = action()
            except BaseException as error:  # re-raised below
                box["error"] = error
            box["returned"] = time.monotonic()

        thread = threading.Thread(target=worker, daemon=True)
        began = time.monotonic()
        thread.start()
        # Keep watching until something changed (or min_wait passed without change),
        # then until no novel frame for STABLE_S.
        seen = self.watch(STABLE_S, 9.0, start=before,
                          until=lambda first: first is not None or time.monotonic() - began >= min_wait)
        thread.join(timeout=30)
        if "error" in box:
            raise box["error"]
        responded = box.get("returned", time.monotonic())
        first = seen["first_change"]
        self.transition = {
            "how": how, "kind": kind,
            "mcp_rtt_ms": round(self.mcp.last_rtt_ms, 2) if kind == "mcp" and self.mcp.last_rtt_ms else None,
            "action_return_ms": round((responded - began) * 1000, 2),
            "frame_change_ms": round((first - began) * 1000, 1) if first else None,
            "settle_ms": round((seen["last_novel"] - began) * 1000, 1) if first else None,
            "settled": seen["settled"],
        }
        return box.get("result")

    def go_mcp(self, name, arguments=None):
        how = f"mcp {name} {json.dumps(arguments or {}, sort_keys=True)}"
        return self.go(how, lambda: self.call(name, arguments), kind="mcp")

    def go_cmd(self, name):
        return self.go_mcp("command", {"name": name})

    def focus(self):
        self.xd("windowfocus", self.window, check=False)

    def key(self, *keys):
        self.focus()
        self.xd("key", "--clearmodifiers", "--delay", "40", *keys)

    def go_key(self, *keys):
        return self.go("xdotool key " + " ".join(keys), lambda: self.key(*keys), kind="xdotool")

    def type_text(self, text):
        self.focus()
        self.xd("type", "--delay", "35", text)

    def go_type(self, text):
        return self.go(f"xdotool type {text!r}", lambda: self.type_text(text), kind="xdotool")

    def click(self, x, y, button=1, park=False):
        self.xd("mousemove", x, y)
        time.sleep(0.05)
        self.xd("click", button)
        if park:
            time.sleep(0.05)
            self.park_pointer()

    def go_click(self, x, y, button=1, label=""):
        return self.go(f"xdotool click button {button} at ({x},{y}) {label}".strip(),
                       lambda: self.click(x, y, button), kind="xdotool")

    def go_hover(self, x, y, label=""):
        return self.go(f"xdotool mousemove ({x},{y}) {label}".strip(),
                       lambda: self.xd("mousemove", x, y), kind="xdotool")

    # ---------------------------------------------------------------- baseline
    def baseline(self, view="grid", select=0):
        """Return to a known state: no overlays, all photos, panels visible, fit zoom."""
        self.park_pointer()
        for _ in range(3):
            st = self.state()
            if st.get("image_context"):
                self.key("Escape")
            if st["deletion"]["open"] and not st["deletion"]["executing"]:
                # MCP refuses every mutation (even close_overlay) while it is open.
                self.key("Escape")
                wait_for(lambda: not self.state()["deletion"]["open"], 3, "deletion dialog to close")
            for _ in range(6):
                self.cmd("close_overlay")
            st = self.state()
            overlays = st["overlays"]
            if not any(overlays.values()) and not st["folder_picker"] and not st["deletion"]["open"]:
                break
        if self.ids:
            if st["filter"] != "all" or st["min_rating"] != 0 or st["duplicates_only"] or st["folder"]:
                self.call("filter", {"filter": "all", "min_rating": 0, "duplicates_only": False,
                                     "clear_folder": True})
            if st["review_subset"]:
                self.cmd("show_full_folder")
            if st["view"] != "grid":
                self.call("view", {"mode": "grid"})
            if st["zoom"] != "fit":
                self.call("zoom", {"mode": "fit"})
            if st["focus_peaking"]:
                self.cmd("toggle_focus_peaking")
            self.call("select", {"index": select})
            if st["inspector_tab"] != "review":
                self.call("view", {"mode": "grid", "panel": "review"})
        st = self.state()
        layout = st["layout"]
        if not layout["sidebar_visible"] or not st["sidebar"]:
            self.cmd("toggle_sidebar")
        if not st["inspector"]:
            self.cmd("toggle_inspector")
        if not st["histogram"]:
            self.cmd("toggle_histogram")
        self.watch(PRE_STABLE_S, 3)  # layout in workspace_state is from the last rendered frame
        st = self.state()
        if (not st["layout"]["inspector_visible"] and not st["layout"]["sidebar_visible"]
                and st["inspector"] and st["sidebar"]):
            # Focus mode hides both docks without changing the persisted flags
            # (it is not reported directly by workspace_state).
            self.cmd("toggle_focus")
            self.notes.append(f"baseline before state #{len(self.records) + 1} found focus mode on; toggled off")
        if self.ids and view != "grid":
            self.call("view", {"mode": view})
        st = self.state()
        if st.get("error"):
            self.cmd("close_overlay")

    def wait_previews(self, timeout=30):
        if self.ids:
            wait_for(self.previews_idle, timeout, "previews to finish decoding")

    # ---------------------------------------------------------------- capture
    def capture(self, spec, check=None):
        transition, self.transition = self.transition, None
        self.wait_previews()
        quiet = 0.5 if transition and transition.get("settled") else STABLE_S
        watched = self.watch(quiet, timeout=6.0)
        final = watched["frame"]
        stable, note = True, None
        # Sample briefly: a static frame yields one key, a blinking caret two.
        sample: dict[str, Frame] = {final.key: final}
        t_end = time.monotonic() + 1.2
        while time.monotonic() < t_end:
            f = self.grab()
            sample.setdefault(f.key, f)
            time.sleep(0.03)
        if len(sample) == 2:
            a, b = sample.values()
            box = diff_box(a, b)
            if box:  # widen so a solid caret contrasts with its surroundings
                box = (max(0, box[0] - 4), max(0, box[1] - 4), min(a.width, box[2] + 4), min(a.height, box[3] + 4))
            final = max((a, b), key=lambda f: region_contrast(f, box)) if box else a
            area = (box[2] - box[0]) * (box[3] - box[1]) if box else 0
            note = (f"two-state blink in box {box} ({area} px); kept the higher-contrast "
                    "(caret/indicator visible) frame")
        elif len(sample) > 2 or not watched["settled"]:
            final = self.grab()
            stable = False
            note = f"frame never stabilised ({len(sample)} distinct frames in 1.2 s); kept latest"
        st = self.state()
        name = f"{spec['id']}__{'dark' if self.theme == DARK else 'light'}__{self.size[0]}x{self.size[1]}"
        path = self.out / f"{name}.png"
        write_png(path, final)
        record = {
            "file": path.name, "state": spec["id"], "description": spec["desc"],
            "pass": self.pass_name, "theme": self.theme, "window": list(self.size),
            "how": spec.get("how", ""), "transition": None,
            "sha256": sha256_file(path), "pixel_sha256": hashlib.sha256(final.rgb).hexdigest(),
            "stable": stable, "note": note,
            "workspace": {k: st.get(k) for k in ("view", "zoom", "zoom_percent", "inspector_tab", "filter",
                                                  "min_rating", "duplicates_only", "selection", "selected_id",
                                                  "reference_id", "focus_peaking", "sidebar", "inspector",
                                                  "histogram", "status", "error", "overlays", "folder_picker",
                                                  "image_context", "review_subset")},
        }
        record["workspace"]["deletion_open"] = st["deletion"]["open"]
        record["workspace"]["persistence"] = st["persistence"]["state"]
        record["workspace"]["layout"] = {k: st["layout"].get(k) for k in ("sidebar_visible", "inspector_visible")}
        if record["workspace"]["folder_picker"]:
            fp = record["workspace"]["folder_picker"]
            record["workspace"]["folder_picker"] = {k: fp.get(k) for k in fp if k != "entries"} | {
                "entries": len(fp.get("entries") or [])}
        if transition:
            transition["visual_change"] = transition["frame_change_ms"] is not None
            record["transition"] = transition
        if check is not None:
            try:
                ok, detail = check(st)
            except Exception as error:
                ok, detail = False, f"check raised {error!r}"
            record["state_check"] = {"ok": bool(ok), "detail": detail}
        self.records.append(record)
        flag = "" if record.get("state_check", {}).get("ok", True) else "  !! state check failed"
        t = record["transition"] or {}
        print(f"  [{len(self.records):3d}] {path.name}  rtt={t.get('mcp_rtt_ms')} change={t.get('frame_change_ms')}"
              f" settle={t.get('settle_ms')}{flag}{'  (' + note + ')' if note else ''}", flush=True)
        return record

    def skip(self, spec, reason):
        self.records.append({"file": None, "state": spec["id"], "description": spec["desc"],
                             "pass": self.pass_name, "theme": self.theme, "window": list(self.size),
                             "skipped": True, "skip_reason": reason})
        print(f"  [skip] {spec['id']}: {reason}", flush=True)

    def run_state(self, spec):
        if self.only and spec["id"] not in self.only:
            return
        self.cleanups = []
        try:
            check = spec["fn"](self)
            self.capture(spec, check)
        except Skip as reason:
            self.skip(spec, str(reason))
        except Exception as error:
            traceback.print_exc()
            self.records.append({"file": None, "state": spec["id"], "description": spec["desc"],
                                 "pass": self.pass_name, "theme": self.theme, "window": list(self.size),
                                 "error": f"{type(error).__name__}: {error}"})
            if self.app is None or self.app.poll() is not None:
                raise
        finally:
            self.transition = None
            for fn in reversed(self.cleanups):
                try:
                    fn()
                except Exception as error:
                    self.notes.append(f"cleanup after {spec['id']} failed: {error}")

    def set_theme(self, theme):
        self.theme = theme
        if self.state()["theme"] == theme:
            return
        self.go_mcp("settings_set", {"settings": {"theme": theme, "theme_mode": "manual"}})
        wait_for(lambda: self.state()["theme"] == theme, 5, f"theme {theme}")
        self.theme_switches.append({"pass": self.pass_name, "to": theme} | self.transition)
        self.transition = None


# --------------------------------------------------------------------------- states
STATES: dict[str, dict] = {}


def state(sid, desc, how=""):
    def register(fn):
        STATES[sid] = {"id": sid, "desc": desc, "how": how, "fn": fn}
        return fn
    return register


def check_view(view, **extra):
    def check(st):
        problems = []
        if view and st["view"] != view:
            problems.append(f"view={st['view']} expected {view}")
        for key, wanted in extra.items():
            got = st.get(key)
            if callable(wanted):
                if not wanted(got):
                    problems.append(f"{key}={got!r} unexpected")
            elif got != wanted:
                problems.append(f"{key}={got!r} expected {wanted!r}")
        return not problems, "; ".join(problems) or "ok"
    return check


def overlay(name, wanted=True):
    def check(st):
        got = st["overlays"].get(name)
        return got == wanted, f"overlays.{name}={got}"
    return check


# ---- empty workspace (fresh profile) ------------------------------------------
@state("first_run_getting_started", "Fresh profile on first launch: Getting Started guide over an empty workspace",
       "fresh launch with show_getting_started=true")
def s_first_run(h):
    st = h.state()
    if not st["overlays"]["getting_started"]:
        h.go_cmd("getting_started")
    return overlay("getting_started")


@state("empty_workspace", "Empty workspace after dismissing the guide (no photos; default Review view)")
def s_empty(h):
    st = h.state()
    if st["view"] != "loupe":
        h.call("view", {"mode": "loupe"})
    if h.state()["overlays"]["getting_started"]:
        h.go_cmd("close_overlay")
    else:
        h.baseline(view="grid")
        h.go_mcp("view", {"mode": "loupe"})
    return check_view("loupe", total=0)


@state("empty_catalog_grid", "Empty Catalog grid (no photos)")
def s_empty_grid(h):
    h.baseline()
    h.go_mcp("view", {"mode": "grid"})
    return check_view("grid", total=0)


@state("empty_folder_picker", "Open-folder picker from an empty workspace")
def s_empty_picker(h):
    h.baseline()
    h.go_cmd("open_folder_picker")
    wait_for(lambda: (h.state()["folder_picker"] or {}).get("loading") is False, 8, "picker listing")
    return lambda st: (st["folder_picker"] is not None, "folder_picker open")


@state("first_import_loading", "First moments after importing the demo folder (indexing/preview placeholders; timing-dependent)",
       "mcp import; first changed frame captured without waiting for previews")
def s_loading(h):
    # Captured inline by the empty pass (see run_empty_pass) because it must not settle.
    raise Skip("captured inline")


# ---- catalog ------------------------------------------------------------------
@state("catalog_grid", "Catalog grid with mixed picks/rejects/ratings/labels, first photo active")
def s_grid(h):
    h.baseline(view="loupe")
    h.go_mcp("view", {"mode": "grid"})
    return check_view("grid", filter="all")


@state("grid_multiselect", "Catalog grid with a three-photo multi-selection (batch inspector)")
def s_multi(h):
    h.baseline()
    h.go_mcp("select", {"ids": [h.ids[1], h.ids[2], h.ids[3]]})
    return check_view("grid", selection=lambda s: len(s or []) == 3)


@state("grid_hover", "Catalog grid with the pointer hovering the third photo (hover affordances)")
def s_hover(h):
    h.baseline()
    x, y = h.grid_cell_center(2)
    h.go("xdotool mousemove in 4 steps onto the third catalog cell",
         lambda: [h.xd("mousemove", x - 12 + 4 * i, y) for i in range(4)], kind="xdotool")
    h.cleanups.append(h.park_pointer)
    return check_view("grid", catalog_hover=lambda v: v is not None)


@state("grid_context_menu", "Right-click context menu on a catalog photo")
def s_context(h):
    h.baseline()
    x, y = h.grid_cell_center(1)
    h.go_click(x, y, 3, "second catalog cell")
    h.cleanups.append(lambda: (h.key("Escape"), h.park_pointer()))
    return lambda st: (st.get("image_context") is not None, f"image_context={st.get('image_context')}")


def help_button_x(h, y=21):
    """Centre x of the title-bar '?' glyph, found by its bounds rather than a
    fixed offset: the title bar's right group is [search] [?] [Export…], and the
    Export plate's width follows its label ("Export 2 picks…"). Scan row `y`
    leftwards across the Export plate, then measure the first glyph after it.
    Falls back to the old fixed offset if the layout is not recognised."""
    f = h.grab()
    w = f.width

    def px(x, yy=y):
        i = (yy * w + x) * 3
        return f.rgb[i:i + 3]

    bg = px(w - 4, 2)  # title-bar background above the buttons

    def differs(x):
        return sum(abs(a - b) for a, b in zip(px(x), bg)) > 30

    x = w - 6
    while x > w // 2 and not differs(x):  # gap right of Export
        x -= 1
    quiet = 0
    while x > w // 2 and quiet < 4:  # across the Export plate to its left edge
        quiet = quiet + 1 if not differs(x) else 0
        x -= 1
    right = None
    quiet = 0
    while x > w // 2:
        if differs(x):
            right = right or x
            left = x
            quiet = 0
        elif right is not None:
            quiet += 1
            if quiet > 5:
                return (left + right) // 2
        elif w - x > 400:
            break
        x -= 1
    return w - 130


@state("tooltip_help_button", "Tooltip shown while hovering the title-bar '?' (Getting Started) button")
def s_tooltip(h):
    h.baseline()
    x = help_button_x(h)
    h.go(f"xdotool mousemove in 3 steps onto the '?' title-bar button (x={x}, found by its bounds)",
         lambda: [h.xd("mousemove", x - 3 + 3 * i, 21) for i in range(3)], kind="xdotool")
    time.sleep(1.2)  # tooltip delay
    h.cleanups.append(h.park_pointer)
    return None


# ---- review -------------------------------------------------------------------
@state("loupe_review", "Review (loupe) of a 5-star green-labelled pick, Review inspector tab")
def s_loupe(h):
    h.baseline(select=1)
    h.go_mcp("view", {"mode": "loupe", "panel": "review"})
    return check_view("loupe", inspector_tab="review", zoom="fit")


@state("loupe_100", "Review at 100% zoom")
def s_loupe_100(h):
    h.baseline(view="loupe", select=1)
    h.go_mcp("zoom", {"mode": "full"})
    return check_view("loupe", zoom="full")


@state("loupe_zoom_400", "Review at 400% custom zoom")
def s_loupe_400(h):
    h.baseline(view="loupe", select=1)
    h.go_mcp("zoom", {"mode": "custom", "percent": 400})
    return check_view("loupe", zoom_percent=lambda p: p is not None and abs(p - 400) < 1)


@state("loupe_info", "Review with the Info inspector tab (metadata)")
def s_info(h):
    h.baseline(view="loupe", select=1)
    h.go_mcp("view", {"mode": "loupe", "panel": "info"})
    return check_view("loupe", inspector_tab="info")


@state("loupe_reject_unrated", "Review of a rejected, unrated photo")
def s_loupe_reject(h):
    h.baseline(view="loupe", select=1)
    h.go_mcp("select", {"index": 4})
    return check_view("loupe")


@state("compare_pinned", "Compare with a pinned reference (left) and the active photo (right)")
def s_compare(h):
    h.baseline(select=0)
    h.call("pin", {"id": h.ids[0]})
    h.call("select", {"id": h.ids[5]})
    h.go_mcp("view", {"mode": "compare"})
    return check_view("compare", reference_id=h.ids[0])


@state("compare_100_peaking", "Compare at 100% zoom with focus peaking on both panes")
def s_compare_peak(h):
    h.baseline(select=0)
    h.call("pin", {"id": h.ids[0]})
    h.call("select", {"id": h.ids[5]})
    h.call("view", {"mode": "compare"})
    h.call("zoom", {"mode": "full"})
    h.go_cmd("toggle_focus_peaking")
    return check_view("compare", focus_peaking=True, zoom="full")


@state("survey_burst", "Survey of a burst (time grouping, 60-minute gap groups the whole shoot)")
def s_survey_burst(h):
    h.baseline(select=1)
    h.call("group", {"mode": "time", "gap_ms": 3600000})
    h.cleanups.append(lambda: h.call("group", {"mode": "off"}))
    wait_for(lambda: h.state()["groups"], 10, "time grouping")
    h.go_mcp("view", {"mode": "survey"})
    return check_view("survey", groups=lambda g: bool(g))


@state("survey", "Survey with a four-photo selection while burst grouping is off (empty state)")
def s_survey(h):
    h.baseline()
    h.call("select", {"ids": h.ids[:4]})
    h.go_mcp("view", {"mode": "survey"})
    return check_view("survey")


@state("focus_mode", "Focus mode (side panels hidden) in Review")
def s_focus(h):
    h.baseline(view="loupe", select=1)
    h.go_cmd("toggle_focus")
    h.cleanups.append(lambda: h.cmd("toggle_focus"))
    return lambda st: (not st["layout"]["sidebar_visible"] and not st["layout"]["inspector_visible"],
                       f"layout sidebar={st['layout']['sidebar_visible']} inspector={st['layout']['inspector_visible']}")


@state("focus_peaking", "Review with cyan focus peaking")
def s_peaking(h):
    h.baseline(view="loupe", select=3)
    h.go_cmd("toggle_focus_peaking")
    return check_view("loupe", focus_peaking=True)


@state("histogram_off", "Review with the histogram hidden")
def s_hist(h):
    h.baseline(view="loupe", select=1)
    h.go_cmd("toggle_histogram")
    return check_view("loupe", histogram=False)


@state("panels_hidden", "Catalog grid with sidebar and inspector hidden")
def s_panels(h):
    h.baseline()
    h.cmd("toggle_sidebar")
    h.go_cmd("toggle_inspector")
    return check_view("grid", sidebar=False, inspector=False)


# ---- filters ------------------------------------------------------------------
@state("rating_filter_menu", "Rating filter menu open over the Catalog")
def s_rating_menu(h):
    h.baseline()
    h.go_cmd("show_rating_filter")
    return overlay("rating_filter")


@state("filter_picks", "Picks filter")
def s_picks(h):
    h.baseline()
    h.go_mcp("filter", {"filter": "picks"})
    return check_view("grid", filter="picks", filtered_count=lambda n: n == 2)


@state("filter_min_rating", "Rated 3 stars and up filter")
def s_min3(h):
    h.baseline()
    h.go_mcp("filter", {"filter": "all", "min_rating": 3})
    return check_view("grid", min_rating=3)


@state("filter_empty_result", "Filter with no matches (Rejects with 3+ stars)")
def s_empty_filter(h):
    h.baseline()
    h.go_mcp("filter", {"filter": "rejects", "min_rating": 3})
    return check_view("grid", filtered_count=0)


@state("filter_empty_review", "Review view when the active filter has no matches")
def s_empty_filter_review(h):
    h.baseline(view="loupe")
    h.go_mcp("filter", {"filter": "rejects", "min_rating": 3})
    return check_view("loupe", filtered_count=0)


@state("trash_view_empty", "Trash view with nothing in Trash")
def s_trash_empty(h):
    h.baseline()
    h.go_mcp("filter", {"filter": "trash"})
    return check_view("grid", filter="trash", filtered_count=0)


@state("trash_view", "Trash view with one in-app trashed photo")
def s_trash(h):
    h.baseline()
    h.call("trash", {"ids": [h.ids[2]], "trashed": True})
    h.cleanups.append(lambda: (h.call("trash", {"ids": [h.ids[2]], "trashed": False}), h.durable()))
    h.durable()
    h.go_mcp("filter", {"filter": "trash"})
    return check_view("grid", filter="trash", filtered_count=1)


@state("duplicates_before_scan", "Duplicates view before any scan")
def s_dupes_empty(h):
    h.baseline()
    h.go_cmd("show_duplicates")
    return check_view(None, duplicates_only=True)


@state("duplicates_results", "Duplicates view after an exact-duplicate scan (one byte-identical pair)")
def s_dupes(h):
    h.baseline()
    scan = h.state().get("duplicate_scan") or {}
    if scan.get("phase") != "completed":
        h.call("duplicates", {"action": "scan"})
        wait_for(lambda: (h.state().get("duplicate_scan") or {}).get("phase") in ("completed", "failed", "cancelled"),
                 30, "duplicate scan")
    h.go_cmd("show_duplicates")
    return check_view(None, duplicates_only=True, filtered_count=lambda n: n >= 2)


# ---- palette / prefs / help -----------------------------------------------------
@state("palette_empty", "Command palette open with an empty query")
def s_palette(h):
    h.baseline()
    h.go_cmd("toggle_palette")
    return overlay("palette")


@state("palette_query", "Command palette filtered by the typed query 'exp'")
def s_palette_query(h):
    h.baseline()
    h.cmd("toggle_palette")
    wait_for(lambda: h.state()["overlays"]["palette"], 3, "palette")
    h.go_type("exp")
    return overlay("palette")


@state("palette_no_match", "Command palette with a query that matches nothing")
def s_palette_none(h):
    h.baseline()
    h.cmd("toggle_palette")
    wait_for(lambda: h.state()["overlays"]["palette"], 3, "palette")
    h.go_type("zzqx")
    return overlay("palette")


PREFS_TABS = ["Appearance", "Culling", "Import", "Performance", "Controller", "Help"]
PREFS_TAB_OFFSETS = [60, 161, 249, 351, 467, 557]   # x offset from the dialog's left edge
PREFS_DIALOG = {LARGE: (380, 188), SMALL: (172, 100)}  # (dialog left x, tab row y)


def _prefs_section(name):
    def fn(h):
        h.baseline()
        h.cmd("show_appearance")
        wait_for(lambda: h.state()["overlays"]["preferences"], 3, "preferences")
        left, y = PREFS_DIALOG[h.size]
        x = left + PREFS_TAB_OFFSETS[PREFS_TABS.index(name)]
        h.go("xdotool click Preferences tab " + repr(name), lambda: h.click(x, y, 1, park=True), kind="xdotool")
        return overlay("preferences")
    return fn


@state("preferences_appearance", "Preferences → Appearance")
def s_prefs_appearance(h):
    h.baseline()
    h.go_cmd("show_appearance")
    return overlay("preferences")


for _name in PREFS_TABS[1:]:
    state(f"preferences_{_name.lower()}", f"Preferences → {_name}",
          "mcp command show_appearance, then click the tab")(_prefs_section(_name))


@state("theme_picker", "Theme JSON file picker opened from Preferences → Appearance")
def s_theme_picker(h):
    h.baseline()
    h.cmd("show_appearance")
    wait_for(lambda: h.state()["overlays"]["preferences"], 3, "preferences")
    h.go_cmd("open_theme_picker")
    wait_for(lambda: (h.state()["folder_picker"] or {}).get("loading") is False, 8, "picker listing")
    return lambda st: (st["folder_picker"] is not None, "folder_picker open")


@state("getting_started", "Getting Started / help guide (F1)")
def s_guide(h):
    h.baseline()
    h.go_cmd("getting_started")
    return overlay("getting_started")


@state("getting_started_scrolled", "Getting Started guide scrolled to its end (keyboard shortcut reference)")
def s_guide_scrolled(h):
    h.baseline()
    h.cmd("getting_started")
    wait_for(lambda: h.state()["overlays"]["getting_started"], 3, "guide")
    w, hgt = h.size
    h.xd("mousemove", w // 2, hgt // 2)
    h.go("xdotool scroll wheel x30 over guide",
         lambda: h.xd("click", "--repeat", "30", "--delay", "15", 5), kind="xdotool")
    h.cleanups.append(h.park_pointer)
    return overlay("getting_started")


@state("folder_picker", "Built-in open-folder picker (Ctrl+O / Add photos)")
def s_picker(h):
    h.baseline()
    h.go_cmd("open_folder_picker")
    wait_for(lambda: (h.state()["folder_picker"] or {}).get("loading") is False, 8, "picker listing")
    return lambda st: (st["folder_picker"] is not None, "folder_picker open")


@state("export_picks_dialog", "Export Picks destination picker (Ctrl+Shift+E)")
def s_export(h):
    h.baseline()
    h.go_key("ctrl+shift+e")
    wait_for(lambda: (h.state()["folder_picker"] or {}).get("loading") is False, 8, "picker listing")
    return lambda st: (st["folder_picker"] is not None, f"folder_picker={bool(st['folder_picker'])}")


@state("export_xmp_dialog", "Export XMP Sidecars destination picker (command palette)")
def s_export_xmp(h):
    h.baseline()
    h.cmd("toggle_palette")
    wait_for(lambda: h.state()["overlays"]["palette"], 3, "palette")
    h.type_text("Export XMP")
    time.sleep(0.3)
    h.go_key("Return")
    wait_for(lambda: (h.state()["folder_picker"] or {}).get("loading") is False, 8, "picker listing")
    return lambda st: (st["folder_picker"] is not None, f"folder_picker={bool(st['folder_picker'])}")


@state("export_complete", "Status after an Export Picks job completes")
def s_export_done(h):
    h.baseline()
    dest = h.session / "Pictures/Exports" / f"picks-{len(h.records)}"
    job = h.go_mcp("export_picks", {"destination": str(dest)})
    job_id = job.get("job_id")
    wait_for(lambda: any(j.get("id") == job_id and j.get("state") in ("completed", "failed")
                         for j in h.state()["export_jobs"]), 20, "export job")
    return lambda st: (any(j.get("id") == job_id and j.get("state") == "completed" for j in st["export_jobs"]),
                       "export completed")


@state("import_panel", "Import panel after scanning a card folder")
def s_import(h):
    h.baseline()
    h.go_mcp("ingest_scan", {"path": str(h.card)})
    wait_for(lambda: (lambda s: not s["scanning"] and s["total"] >= 4)(h.call("ingest_status")), 20, "card scan")
    h.cleanups.append(lambda: h.cmd("close_overlay"))
    return None


@state("delete_confirm_focus_confirm", "Move-to-Trash confirmation after Tab moves focus to the destructive button")
def s_delete_tab(h):
    h.baseline(select=2)
    h.key("Delete")
    wait_for(lambda: h.state()["deletion"]["open"], 3, "deletion confirmation")
    h.go_key("Tab")
    h.cleanups.append(lambda: h.key("Escape"))
    return lambda st: (st["deletion"]["open"] and not st["deletion"]["executing"], "confirmation still open, not executing")


def _open_import(h):
    h.call("ingest_scan", {"path": str(h.card)})
    wait_for(lambda: (lambda s: not s["scanning"] and s["total"] >= 4 and s["plan"] is not None)(
        h.call("ingest_status")), 20, "card scan and plan")
    h.watch(PRE_STABLE_S, 4)
    h.cleanups.append(lambda: h.cmd("close_overlay"))


@state("import_destination_scrolled", "Import panel with the Destination/Naming column scrolled to its end")
def s_import_dest(h):
    h.baseline()
    _open_import(h)
    w, hgt = h.size
    h.xd("mousemove", w - 150, hgt // 2)
    h.go("xdotool scroll wheel x25 over the Destination column",
         lambda: h.xd("click", "--repeat", "25", "--delay", "15", 5), kind="xdotool")
    h.cleanups.append(h.park_pointer)
    return None


@state("import_job_code_editing", "Import panel: typing into the Job code text field (focus ring + caret)")
def s_import_field(h):
    if h.size != LARGE:
        raise Skip("field position only measured at 1440x900")
    h.baseline()
    _open_import(h)
    # "J" focuses Job code where the build supports it (wave 2 io-15);
    # older builds keep the field at its measured position.
    h.key("j")
    time.sleep(0.3)
    if (h.state().get("layout", {}).get("import") or {}).get("active_field") != 3:
        h.click(1288, 748, 1)
    h.park_pointer()
    h.go_type("Iceland")
    return None


@state("import_timeline_tab", "Import panel (narrow layout) with the Timeline tab selected")
def s_import_timeline(h):
    if h.size != SMALL:
        raise Skip("Timeline is a separate tab only in the narrow layout")
    h.baseline()
    _open_import(h)
    h.go("xdotool click 'Timeline' tab", lambda: h.click(335, 111, 1, park=True), kind="xdotool")
    return None


@state("import_history", "Import History (command palette)")
def s_import_history(h):
    h.baseline()
    h.cmd("toggle_palette")
    wait_for(lambda: h.state()["overlays"]["palette"], 3, "palette")
    h.type_text("Import History")
    time.sleep(0.3)
    h.go_key("Return")
    h.cleanups.append(lambda: h.cmd("close_overlay"))
    return None


@state("delete_confirm", "Move-to-Trash confirmation (Delete key) for one catalog photo")
def s_delete(h):
    h.baseline(select=2)
    h.go_key("Delete")
    h.cleanups.append(lambda: h.key("Escape"))
    return lambda st: (st["deletion"]["open"], f"deletion.open={st['deletion']['open']}")


@state("delete_confirm_batch", "Move-to-Trash confirmation for a three-photo selection")
def s_delete_batch(h):
    h.baseline()
    h.call("select", {"ids": [h.ids[1], h.ids[2], h.ids[3]]})
    h.go_key("Delete")
    h.cleanups.append(lambda: h.key("Escape"))
    return lambda st: (st["deletion"]["open"], f"deletion.open={st['deletion']['open']}")


@state("permanent_delete_confirm", "Permanent-deletion confirmation (Shift+Delete)")
def s_perm_delete(h):
    h.baseline(select=2)
    h.go_key("shift+Delete")
    h.cleanups.append(lambda: h.key("Escape"))
    return lambda st: (st["deletion"]["open"], f"deletion.open={st['deletion']['open']}")


@state("add_host_dialog", "Network: Add host dialog (command palette 'Add host…')")
def s_add_host(h):
    h.baseline()
    h.cmd("toggle_palette")
    wait_for(lambda: h.state()["overlays"]["palette"], 3, "palette")
    h.type_text("Add host")
    time.sleep(0.3)
    h.go_key("Return")
    h.cleanups.append(lambda: h.cmd("close_overlay"))
    return None


@state("error_toast", "Error toast: saving the Add host dialog with an empty alias")
def s_error(h):
    h.baseline()
    h.cmd("toggle_palette")
    wait_for(lambda: h.state()["overlays"]["palette"], 3, "palette")
    h.type_text("Add host")
    time.sleep(0.3)
    h.key("Return")
    time.sleep(0.3)
    h.go_key("Return")
    h.cleanups.append(lambda: (h.cmd("close_overlay"), h.cmd("close_overlay")))
    return lambda st: (bool(st.get("error")), f"error={st.get('error')!r}")


@state("help_shortcut_question_mark", "'?' key as a shortcut-help trigger")
def s_qmark(h):
    h.baseline()
    h.go_key("question")
    st = h.state()
    if not st["overlays"]["getting_started"]:
        raise Skip("'?' is not bound to anything (Shift+/ produces no visible change); shortcut help lives in "
                   "Getting Started (F1 or the title-bar '?' button), captured as getting_started")
    return overlay("getting_started")


# ---- persistence failure states (run last; they lock the catalog) --------------
@state("save_pending", "Decision saving while the catalog writer is busy (pending save indicator)")
def s_save_pending(h):
    raise Skip("captured inline")


@state("save_failed", "Decision save failed after the SQLite busy timeout (unsaved-decisions error)")
def s_save_failed(h):
    raise Skip("captured inline")


@state("quit_blocked_unsaved", "Quit attempt refused while decisions are unsaved")
def s_quit_blocked(h):
    raise Skip("captured inline")


@state("saves_recovered", "After Retry unsaved decisions succeeds")
def s_recovered(h):
    raise Skip("captured inline")


# --------------------------------------------------------------------------- layout probes
def _grid_cell_center(self, index):
    st = self.state()
    layout = st["layout"]
    cols = st["catalog_scroll"]["columns"] or 4
    left = layout["sidebar_width"] if layout["sidebar_visible"] else 0
    width = layout["center_width"]
    cell = width / cols
    top = self.GRID_TOP
    row, col = divmod(index, cols)
    return int(left + cell * col + cell / 2), int(top + cell * 0.75 * row + cell * 0.35)


Harness.grid_cell_center = _grid_cell_center
Harness.GRID_TOP = 100


# --------------------------------------------------------------------------- passes
EMPTY_LIGHT = ["empty_workspace", "empty_catalog_grid"]
EMPTY_SMALL = ["empty_workspace"]

MAIN = [
    "catalog_grid", "grid_multiselect", "grid_hover", "grid_context_menu", "tooltip_help_button",
    "loupe_review", "loupe_100", "loupe_zoom_400", "loupe_info", "loupe_reject_unrated",
    "compare_pinned", "compare_100_peaking", "survey_burst", "survey", "focus_mode", "focus_peaking", "histogram_off",
    "panels_hidden", "rating_filter_menu", "filter_picks", "filter_min_rating", "filter_empty_result",
    "filter_empty_review", "trash_view_empty", "trash_view", "duplicates_before_scan", "duplicates_results",
    "palette_empty", "palette_query", "palette_no_match",
    "preferences_appearance", "preferences_culling", "preferences_performance", "preferences_import",
    "preferences_controller", "preferences_help", "theme_picker",
    "getting_started", "getting_started_scrolled", "help_shortcut_question_mark",
    "folder_picker", "export_picks_dialog", "export_xmp_dialog", "export_complete",
    "import_panel", "import_destination_scrolled", "import_job_code_editing", "import_history",
    "delete_confirm", "delete_confirm_focus_confirm", "delete_confirm_batch", "permanent_delete_confirm",
    "add_host_dialog", "error_toast",
]
LIGHT_MAJOR = [
    "catalog_grid", "grid_multiselect", "grid_context_menu", "loupe_review", "loupe_100", "loupe_info",
    "compare_pinned", "survey_burst", "focus_mode", "focus_peaking", "rating_filter_menu", "filter_empty_result",
    "trash_view", "duplicates_results", "palette_empty", "palette_query",
    "preferences_appearance", "preferences_culling", "preferences_performance", "preferences_import",
    "preferences_controller", "preferences_help", "theme_picker", "getting_started", "folder_picker",
    "export_picks_dialog", "import_panel", "import_destination_scrolled", "import_job_code_editing", "delete_confirm",
    "add_host_dialog", "error_toast",
]
SMALL_MAIN = [
    "catalog_grid", "grid_multiselect", "grid_context_menu", "loupe_review", "loupe_info", "compare_pinned",
    "survey_burst", "focus_mode", "rating_filter_menu", "filter_empty_result", "palette_query", "preferences_appearance",
    "preferences_culling", "preferences_import", "getting_started", "folder_picker", "export_picks_dialog",
    "import_panel", "import_destination_scrolled", "import_timeline_tab", "delete_confirm", "add_host_dialog",
    "error_toast",
]


def run_empty_pass(h: Harness):
    h.pass_name = "empty"
    print("== pass: empty profile (dark 1440x900)", flush=True)
    h.launch(LARGE)
    h.theme = DARK
    h.run_state(STATES["first_run_getting_started"])
    h.run_state(STATES["empty_workspace"])
    h.run_state(STATES["empty_catalog_grid"])
    h.run_state(STATES["empty_folder_picker"])
    h.set_theme(LIGHT)
    for sid in EMPTY_LIGHT:
        h.run_state(STATES[sid])
    h.set_theme(DARK)
    h.resize(SMALL)
    for sid in EMPTY_SMALL:
        h.run_state(STATES[sid])
    h.resize(LARGE)
    # Import the demo folder and capture the first loading frame (timing-dependent).
    h.baseline()
    h.call("view", {"mode": "grid"})
    spec = dict(STATES["first_import_loading"])
    h.pass_name = "empty"
    if not h.only or spec["id"] in h.only:
        before = h.watch(PRE_STABLE_S, 4)["frame"]
        began = time.monotonic()
        h.call("import", {"path": str(h.shoot)})
        rtt = h.mcp.last_rtt_ms
        frame = wait_for(lambda: (lambda f: f if f.key != before.key else None)(h.grab()), 5,
                         "first frame after import", interval=0.004)
        h.transition = {"how": "mcp import (first changed frame)", "kind": "mcp", "mcp_rtt_ms": round(rtt, 2),
                        "action_return_ms": None, "frame_change_ms": round((frame.at - began) * 1000, 1),
                        "settle_ms": None, "settled": False}
        st = h.state()
        name = f"{spec['id']}__dark__{LARGE[0]}x{LARGE[1]}"
        path = h.out / f"{name}.png"
        write_png(path, frame)
        t = h.transition
        h.transition = None
        h.records.append({"file": path.name, "state": spec["id"], "description": spec["desc"], "pass": "empty",
                          "theme": DARK, "window": list(LARGE), "how": spec["how"],
                          "sha256": sha256_file(path), "pixel_sha256": hashlib.sha256(frame.rgb).hexdigest(),
                          "stable": False, "deterministic": False,
                          "note": "first frame after import; content depends on indexing/decoding speed",
                          "transition": t | {"visual_change": True},
                          "workspace": {"status": st.get("status"), "indexing": st.get("indexing"),
                                        "total": st.get("total")}})
        print(f"  [{len(h.records):3d}] {path.name} (non-deterministic)", flush=True)
    else:
        h.call("import", {"path": str(h.shoot)})
    wait_for(lambda: (lambda s: s["total"] >= 7 and not s["indexing"])(h.state()), 60, "demo import")
    h.call("settings_set", {"settings": {"show_getting_started": False}})
    h.load_ids()
    h.apply_decisions()
    h.quit_app()


def _load_ids(self):
    photos = self.call("library_list", {"scope": "all", "limit": 100})["photos"]
    photos.sort(key=lambda p: p["filename"])
    self.by_name = {p["filename"]: p["id"] for p in photos}
    # Order used by recipes: catalog order of the filtered list.
    listing = self.call("library_list", {"limit": 100})["photos"]
    self.ids = [p["id"] for p in listing]


def _apply_decisions(self):
    ids = self.ids
    self.call("flag", {"ids": [ids[1], ids[3]], "flag": "pick"})
    self.call("flag", {"ids": [ids[4]], "flag": "reject"})
    self.call("rate", {"ids": [ids[1]], "rating": 5})
    self.call("rate", {"ids": [ids[3]], "rating": 4})
    self.call("rate", {"ids": [ids[0]], "rating": 2})
    self.call("rate", {"ids": [ids[2]], "rating": 3})
    self.call("label", {"ids": [ids[1]], "label": "green"})
    self.call("label", {"ids": [ids[2]], "label": "red"})
    self.call("label", {"ids": [ids[0]], "label": "yellow"})
    self.call("label", {"ids": [ids[5]], "label": "blue"})
    self.durable()


Harness.load_ids = _load_ids
Harness.apply_decisions = _apply_decisions


def run_pass(h: Harness, name, theme, size, state_ids):
    h.pass_name = name
    print(f"== pass: {name} ({theme}, {size[0]}x{size[1]})", flush=True)
    h.launch(size)
    h.load_ids()
    h.set_theme(theme)
    h.wait_previews()
    for sid in state_ids:
        h.run_state(STATES[sid])
        if h.app.poll() is not None:
            h.notes.append(f"app exited during pass {name} after {sid}")
            break
    h.quit_app()


def run_failure_pass(h: Harness):
    """Lock the catalog writer to expose pending/failed save and blocked-quit UI."""
    h.pass_name = "persistence"
    wanted = ["save_pending", "save_failed", "quit_blocked_unsaved", "saves_recovered"]
    if h.only and not (h.only & set(wanted)):
        return
    print("== pass: persistence failure (dark 1440x900)", flush=True)
    h.launch(LARGE)
    h.load_ids()
    h.set_theme(DARK)
    h.baseline(view="loupe", select=0)
    h.wait_previews()
    database = next((h.session / "data").rglob("catalog.sqlite3"))
    connection = sqlite3.connect(database, timeout=1)
    try:
        connection.execute("BEGIN IMMEDIATE")
        h.go_mcp("rate", {"rating": 1})
        rec = h.capture(STATES["save_pending"],
                        lambda st: (st["persistence"]["state"] == "pending", f"persistence={st['persistence']['state']}"))
        if rec and rec.get("note") is None:
            pass
        wait_for(lambda: h.state()["persistence"]["state"] == "failed", 15, "busy-timeout failure", interval=0.2)
        h.capture(STATES["save_failed"], lambda st: (st["persistence"]["state"] == "failed",
                                                     f"persistence={st['persistence']['state']}"))
        h.go_mcp("quit")
        time.sleep(0.3)
        h.capture(STATES["quit_blocked_unsaved"],
                  lambda st: (h.app.poll() is None and bool(st.get("error")), f"error={st.get('error')!r}"))
    finally:
        connection.rollback()
        connection.close()
    h.go_cmd("retry_saves")
    h.durable()
    h.capture(STATES["saves_recovered"], lambda st: (st["persistence"]["state"] == "durable", "durable"))
    # Undo the 1-star rating so later runs start from identical decisions.
    h.call("rate", {"ids": [h.ids[0]], "rating": 2})
    h.durable()
    h.quit_app()


# Pairs whose pixels are identical by app design (recorded as findings, not
# navigation failures): both export flows open the same titled picker, and
# Catalog cells track hover without drawing any hover affordance.
KNOWN_IDENTICAL = {
    frozenset({"export_picks_dialog", "export_xmp_dialog"}):
        "Export Picks and Export XMP open the same 'Choose export destination' picker (workspace_state purpose is identical)",
    frozenset({"catalog_grid", "grid_hover"}):
        "Catalog cells set workspace_state.catalog_hover but render no hover affordance",
}


def summarize(h: Harness):
    captured = [r for r in h.records if r.get("file")]
    by_pixels: dict[tuple, list] = {}
    for r in captured:
        by_pixels.setdefault((r["pixel_sha256"],), []).append(r)
    duplicates = []
    for group in by_pixels.values():
        if len(group) > 1:
            names = [r["file"] for r in group]
            explained = KNOWN_IDENTICAL.get(frozenset(r["state"] for r in group))
            for r in group:
                r["identical_to"] = [n for n in names if n != r["file"]]
                if explained:
                    r["identical_explained"] = explained
            duplicates.append({"files": names, "explained": explained})
    timings = [r["transition"] for r in captured if r.get("transition")]

    def stats(key, kind=None):
        values = sorted(t[key] for t in timings if t.get(key) is not None and (kind is None or t["kind"] == kind))
        if not values:
            return None
        pick = lambda q: values[min(len(values) - 1, int(q * (len(values) - 1) + 0.5))]
        return {"n": len(values), "min": values[0], "median": pick(0.5), "p90": pick(0.9), "max": values[-1]}

    return {
        "captured": len(captured),
        "skipped": [{"state": r["state"], "pass": r["pass"], "reason": r["skip_reason"]}
                    for r in h.records if r.get("skipped")],
        "errors": [{"state": r["state"], "pass": r["pass"], "error": r["error"]}
                   for r in h.records if r.get("error")],
        "failed_state_checks": [{"file": r["file"], "detail": r["state_check"]["detail"]}
                                for r in captured if r.get("state_check") and not r["state_check"]["ok"]],
        "no_visual_change": [r["file"] for r in captured
                             if r.get("transition") and not r["transition"].get("visual_change")],
        "unstable": [{"file": r["file"], "note": r.get("note")} for r in captured if not r.get("stable", True)],
        "identical_captures": duplicates,
        "unexplained_identical": [d["files"] for d in duplicates if not d["explained"]],
        "timing": {
            "mcp_rtt_ms": stats("mcp_rtt_ms", "mcp"),
            "frame_change_ms_mcp": stats("frame_change_ms", "mcp"),
            "frame_change_ms_xdotool": stats("frame_change_ms", "xdotool"),
            "settle_ms_mcp": stats("settle_ms", "mcp"),
            "settle_ms_xdotool": stats("settle_ms", "xdotool"),
        },
    }


def _decode_png(path: Path) -> bytes | None:
    if not shutil.which("magick") or not path.is_file():
        return None
    return subprocess.run(["magick", str(path), "-depth", "8", "rgb:-"], capture_output=True,
                          check=True, timeout=30).stdout


def compare_runs(previous_manifest: Path, out: Path, records: list[dict], tolerance: int = 8) -> dict:
    """Classify each capture against an earlier run.

    identical: same pixels; noise: every channel within `tolerance` levels (GPU
    resampling jitter); changed: pixels beyond tolerance, with count and bbox.
    Known run-to-run variance in this app: the Import panel's free-space text and
    which preview tier (embedded thumbnail vs decoded) the first catalog/filmstrip
    thumbnails settle on, so small 'changed' boxes there are not regressions.
    """
    previous_dir = previous_manifest.resolve().parent
    previous = {r["file"]: r for r in json.loads(previous_manifest.read_text())["states"] if r.get("file")}
    current = {r["file"]: r for r in records if r.get("file")}
    result = {"against": str(previous_manifest.resolve()), "tolerance_levels": tolerance,
              "identical": [], "noise": [], "changed": [],
              "new": sorted(set(current) - set(previous)), "missing": sorted(set(previous) - set(current))}
    for name in sorted(set(current) & set(previous)):
        if previous[name]["pixel_sha256"] == current[name]["pixel_sha256"]:
            result["identical"].append(name)
            continue
        a, b = _decode_png(previous_dir / name), _decode_png(out / name)
        if a is None or b is None or len(a) != len(b):
            result["changed"].append({"file": name, "pixels": None, "bbox": None,
                                      "detail": "size differs or images unavailable"})
            continue
        width = current[name]["window"][0]
        big = [i // 3 for i in range(len(a)) if a[i] != b[i] and abs(a[i] - b[i]) > tolerance]
        if not big:
            result["noise"].append(name)
            continue
        pixels = sorted(set(big))
        xs, ys = [p % width for p in pixels], [p // width for p in pixels]
        result["changed"].append({"file": name, "pixels": len(pixels),
                                  "bbox": [min(xs), min(ys), max(xs) + 1, max(ys) + 1]})
    result["counts"] = {k: len(result[k]) for k in ("identical", "noise", "changed", "new", "missing")}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", help="shadowroom executable (required unless --compare-only)")
    parser.add_argument("--photos", help="demo photo folder, never modified (required unless --compare-only)")
    parser.add_argument("--out", required=True)
    parser.add_argument("--session", default=str(Path(tempfile.gettempdir()) / "shadowroom-hi-session"),
                        help="Fixed scratch profile dir (wiped each run; a fixed path keeps paths shown in the UI stable)")
    parser.add_argument("--xdotool")
    parser.add_argument("--only", help="Comma-separated state ids (debugging)")
    parser.add_argument("--passes", default="empty,dark,light,small,persistence")
    parser.add_argument("--use-current-display", action="store_true",
                        help="Use $DISPLAY instead of a private rootful Xwayland")
    parser.add_argument("--keep-session", action="store_true")
    parser.add_argument("--compare", help="Earlier manifest.json: classify captures as identical/noise/changed")
    parser.add_argument("--compare-only", action="store_true",
                        help="Do not launch anything; add the --compare result to --out/manifest.json")
    args = parser.parse_args()

    if args.compare_only:
        require(args.compare, "--compare-only needs --compare")
        path = Path(args.out) / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest["comparison"] = compare_runs(Path(args.compare), Path(args.out).resolve(), manifest["states"])
        path.write_text(json.dumps(manifest, indent=2) + "\n")
        print(json.dumps(manifest["comparison"]["counts"]))
        return 0
    if not (args.binary and args.photos):
        parser.error("--binary and --photos are required")
    h = Harness(args)
    h.out.mkdir(parents=True, exist_ok=True)
    for old in h.out.glob("*.png"):
        old.unlink()
    profile_before = h.real_profile_snapshot()
    started = time.time()
    passes = args.passes.split(",")
    failure = None
    try:
        h.prepare_session()
        h.start_xserver()
        h.setup_tools()
        run_empty_pass(h)
        if "dark" in passes:
            run_pass(h, "dark", DARK, LARGE, MAIN)
        if "light" in passes:
            run_pass(h, "light", LIGHT, LARGE, LIGHT_MAJOR)
        if "small" in passes:
            run_pass(h, "small", DARK, SMALL, SMALL_MAIN)
        if "persistence" in passes:
            run_failure_pass(h)
    except Exception as error:
        failure = f"{type(error).__name__}: {error}"
        traceback.print_exc()
    finally:
        with contextlib.suppress(Exception):
            h.quit_app()
        h.kill_app()
        if h.xserver is not None:
            with contextlib.suppress(Exception):
                h.xd_conn.close()
            with contextlib.suppress(ProcessLookupError):
                os.killpg(h.xserver.pid, signal.SIGTERM)
            with contextlib.suppress(Exception):
                h.xserver.wait(timeout=5)
    profile_after = h.real_profile_snapshot()
    originals_ok = all((sha256_file(Path(p)), Path(p).stat().st_mtime_ns) == v
                       for p, v in getattr(h, "originals", {}).items())
    summary = summarize(h)
    manifest = {
        "tool": "capture_states.py", "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(started)),
        "elapsed_s": round(time.time() - started, 1),
        "binary": str(h.binary), "binary_sha256": sha256_file(h.binary),
        "binary_mtime": time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(h.binary.stat().st_mtime)),
        "photos": str(h.photos_src), "display": getattr(h, "display", None),
        "x_backend": "private rootful Xwayland" if h.xserver else "existing DISPLAY",
        "scale": "GPUI_X11_SCALE_FACTOR=1", "session": str(h.session),
        "failure": failure, "notes": h.notes, "launches": h.launch_timings, "theme_switches": h.theme_switches,
        "originals_unchanged": originals_ok,
        "real_profile_untouched": profile_before == profile_after,
        "real_profile_snapshot": profile_before,
        "summary": summary, "states": h.records,
    }
    if args.compare:
        manifest["comparison"] = compare_runs(Path(args.compare), h.out, h.records)
    (h.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    if not args.keep_session and failure is None:
        shutil.rmtree(h.session, ignore_errors=True)
    print(json.dumps({"captured": summary["captured"], "skipped": len(summary["skipped"]),
                      "errors": len(summary["errors"]), "failed_checks": len(summary["failed_state_checks"]),
                      "unexplained_identical": summary["unexplained_identical"], "manifest": str(h.out / "manifest.json"),
                      "profile_untouched": manifest["real_profile_untouched"],
                      "originals_unchanged": originals_ok, "failure": failure}, indent=2))
    return 0 if failure is None else 1


if __name__ == "__main__":
    sys.exit(main())
