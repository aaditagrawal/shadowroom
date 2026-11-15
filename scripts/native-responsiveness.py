#!/usr/bin/env python3
"""Measure how the native desktop responds to input, frame by frame (Linux/X11).

    uv run --no-project python scripts/native-responsiveness.py \
        --binary target/release/shadowroom --photos /path/to/demo/photos \
        --output /tmp/shadowroom-hi-1

The app runs on a private rootful Xwayland server with a private HOME and XDG
directories; the photos are copied into the output directory and never written.
The app records `SHADOWROOM_FRAME_TRACE` (see src/desktop/frame_trace.rs): key
and pointer input stamped before dispatch, `Workspace::render` start/end, and
the instant GPUI's draw + present returned. This driver injects input with
XTEST, stamping each event with CLOCK_MONOTONIC right after XFlush, and listens
for XDamage on the app window from a second X connection: damage arrives when
the X server has the new frame, which includes the Vulkan present into Xwayland
but not the host compositor or display scan-out.

For each input it reports send -> app keystroke, send -> next frame presented,
send -> damage, that frame's render and draw+present costs, and how many display
intervals each frame took. Idle periods record CPU time, main-thread wakeups and
frames drawn. Python stdlib only.
"""
from __future__ import annotations

import argparse
import contextlib
import ctypes
import ctypes.util
import json
import math
import os
import platform
import queue
import select
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

PHOTO_SUFFIXES = (".jpg", ".jpeg", ".png", ".tif", ".tiff", ".heic", ".dng", ".cr3", ".nef", ".arw")
WINDOW = (1440, 900)
ROOT = (1600, 1000)
CLK_TCK = os.sysconf("SC_CLK_TCK")


def now_ns() -> int:
    return time.monotonic_ns()


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def wait_for(fn, timeout=10.0, what="condition", interval=0.05):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = fn()
            if value:
                return value
        except Exception as error:  # transient while starting
            last = error
        time.sleep(interval)
    raise AssertionError(f"Timed out waiting for {what}" + (f" ({last})" if last else ""))


def percentile(values, fraction):
    ordered = sorted(values)
    if not ordered:
        return None
    return ordered[min(len(ordered) - 1, max(0, math.ceil(len(ordered) * fraction) - 1))]


def summary(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return {"n": len(values), "p50": round(statistics.median(values), 3),
            "p95": round(percentile(values, .95), 3), "max": round(max(values), 3)}


# --------------------------------------------------------------------------- X11
class X:
    """libX11 + XTEST + XDamage through ctypes (no xdotool process per event)."""

    def __init__(self, name: str):
        self.x = x = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
        self.xtst = ctypes.CDLL(ctypes.util.find_library("Xtst") or "libXtst.so.6")
        x.XInitThreads()
        x.XOpenDisplay.restype = ctypes.c_void_p
        x.XOpenDisplay.argtypes = [ctypes.c_char_p]
        x.XDefaultRootWindow.restype = ctypes.c_ulong
        x.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
        x.XFlush.argtypes = [ctypes.c_void_p]
        x.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
        x.XStringToKeysym.restype = ctypes.c_ulong
        x.XStringToKeysym.argtypes = [ctypes.c_char_p]
        x.XKeysymToKeycode.restype = ctypes.c_ubyte
        x.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        x.XQueryTree.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(ctypes.c_ulong),
                                 ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.POINTER(ctypes.c_ulong)),
                                 ctypes.POINTER(ctypes.c_uint)]
        x.XFree.argtypes = [ctypes.c_void_p]
        x.XInternAtom.restype = ctypes.c_ulong
        x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
        x.XGetWindowProperty.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_long,
                                         ctypes.c_long, ctypes.c_int, ctypes.c_ulong,
                                         ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_int),
                                         ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_ulong),
                                         ctypes.POINTER(ctypes.c_void_p)]
        x.XMoveResizeWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_int,
                                        ctypes.c_uint, ctypes.c_uint]
        x.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
        x.XRaiseWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        x.XGetGeometry.argtypes = [ctypes.c_void_p, ctypes.c_ulong] + [ctypes.c_void_p] * 7
        self.xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
        self.xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
        self.xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                                                   ctypes.c_ulong]
        self.d = x.XOpenDisplay(name.encode())
        require(self.d, f"Cannot open X display {name}")
        self.root = x.XDefaultRootWindow(self.d)
        self.lock = threading.Lock()

    def keycode(self, keysym_name: str) -> int:
        sym = self.x.XStringToKeysym(keysym_name.encode())
        require(sym, f"Unknown keysym {keysym_name}")
        code = self.x.XKeysymToKeycode(self.d, sym)
        require(code, f"No keycode for {keysym_name}")
        return code

    def key(self, name: str, press: bool) -> int:
        code = self.keycode(name)
        with self.lock:
            # Stamp before sending: a loaded host can deschedule this thread
            # after XFlush, and the app may receive the event before we resume.
            sent = now_ns()
            self.xtst.XTestFakeKeyEvent(self.d, code, int(press), 0)
            self.x.XFlush(self.d)
            return sent

    def motion(self, x: int, y: int) -> int:
        with self.lock:
            # Stamp before sending: a loaded host can deschedule this thread
            # after XFlush, and the app may receive the event before we resume.
            sent = now_ns()
            self.xtst.XTestFakeMotionEvent(self.d, 0, x, y, 0)
            self.x.XFlush(self.d)
            return sent

    def button(self, number: int, press: bool) -> int:
        with self.lock:
            # Stamp before sending: a loaded host can deschedule this thread
            # after XFlush, and the app may receive the event before we resume.
            sent = now_ns()
            self.xtst.XTestFakeButtonEvent(self.d, number, int(press), 0)
            self.x.XFlush(self.d)
            return sent

    def children(self, window: int):
        root, parent = ctypes.c_ulong(), ctypes.c_ulong()
        kids, count = ctypes.POINTER(ctypes.c_ulong)(), ctypes.c_uint()
        with self.lock:
            if not self.x.XQueryTree(self.d, window, ctypes.byref(root), ctypes.byref(parent),
                                     ctypes.byref(kids), ctypes.byref(count)):
                return []
            out = [kids[i] for i in range(count.value)]
            if kids:
                self.x.XFree(kids)
        return out

    def cardinal(self, window: int, atom_name: str):
        atom = self.x.XInternAtom(self.d, atom_name.encode(), 0)
        actual, fmt = ctypes.c_ulong(), ctypes.c_int()
        nitems, after, data = ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_void_p()
        with self.lock:
            status = self.x.XGetWindowProperty(self.d, window, atom, 0, 1, 0, 0, ctypes.byref(actual),
                                               ctypes.byref(fmt), ctypes.byref(nitems), ctypes.byref(after),
                                               ctypes.byref(data))
            if status != 0 or not data.value:
                return None
            value = ctypes.cast(data, ctypes.POINTER(ctypes.c_ulong))[0] if nitems.value else None
            self.x.XFree(data)
        return value

    def find_window(self, pid: int):
        stack = [self.root]
        while stack:
            window = stack.pop()
            for child in self.children(window):
                if self.cardinal(child, "_NET_WM_PID") == pid:
                    return child
                stack.append(child)
        return None

    def geometry(self, window):
        root, xx, yy = ctypes.c_ulong(), ctypes.c_int(), ctypes.c_int()
        w, h, b, d = ctypes.c_uint(), ctypes.c_uint(), ctypes.c_uint(), ctypes.c_uint()
        with self.lock:
            self.x.XGetGeometry(self.d, window, ctypes.byref(root), ctypes.byref(xx), ctypes.byref(yy),
                                ctypes.byref(w), ctypes.byref(h), ctypes.byref(b), ctypes.byref(d))
        return w.value, h.value

    def place(self, window, size):
        with self.lock:
            self.x.XMoveResizeWindow(self.d, window, 0, 0, size[0], size[1])
            self.x.XRaiseWindow(self.d, window)
            self.x.XSetInputFocus(self.d, window, 2, 0)
            self.x.XSync(self.d, 0)

    def focus(self, window):
        with self.lock:
            self.x.XSetInputFocus(self.d, window, 2, 0)
            self.x.XSync(self.d, 0)


class DamageWatcher:
    """A second connection that records when the X server sees new window content."""

    def __init__(self, display: str, window: int):
        self.x = x = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
        self.xd = xd = ctypes.CDLL(ctypes.util.find_library("Xdamage") or "libXdamage.so.1")
        x.XOpenDisplay.restype = ctypes.c_void_p
        x.XOpenDisplay.argtypes = [ctypes.c_char_p]
        x.XConnectionNumber.argtypes = [ctypes.c_void_p]
        x.XPending.argtypes = [ctypes.c_void_p]
        x.XNextEvent.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        x.XFlush.argtypes = [ctypes.c_void_p]
        x.XCloseDisplay.argtypes = [ctypes.c_void_p]
        xd.XDamageQueryExtension.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_int),
                                             ctypes.POINTER(ctypes.c_int)]
        xd.XDamageCreate.restype = ctypes.c_ulong
        xd.XDamageCreate.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int]
        xd.XDamageSubtract.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
        self.d = x.XOpenDisplay(display.encode())
        require(self.d, "Damage connection failed")
        base, err = ctypes.c_int(), ctypes.c_int()
        require(xd.XDamageQueryExtension(self.d, ctypes.byref(base), ctypes.byref(err)), "No XDamage")
        self.notify = base.value  # XDamageNotify == 0
        self.damage = xd.XDamageCreate(self.d, window, 3)  # XDamageReportNonEmpty
        x.XFlush(self.d)
        self.times: list[int] = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        fd = self.x.XConnectionNumber(self.d)
        event = ctypes.create_string_buffer(192)
        while not self.stop.is_set():
            readable, _, _ = select.select([fd], [], [], 0.1)
            if not readable and not self.x.XPending(self.d):
                continue
            while self.x.XPending(self.d):
                self.x.XNextEvent(self.d, event)
                kind = ctypes.cast(event, ctypes.POINTER(ctypes.c_int))[0]
                if kind == self.notify:
                    self.times.append(now_ns())
                    self.xd.XDamageSubtract(self.d, self.damage, 0, 0)
                    self.x.XFlush(self.d)

    def close(self):
        self.stop.set()
        self.thread.join(timeout=2)
        with contextlib.suppress(Exception):
            self.x.XCloseDisplay(self.d)


# --------------------------------------------------------------------------- MCP
class MCP:
    def __init__(self, binary: Path, env: dict, logs: Path):
        self.serial = 0
        self.messages: queue.Queue = queue.Queue()
        self.stderr = (logs / "mcp.stderr.log").open("wb")
        self.process = subprocess.Popen([str(binary), "--mcp"], env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.stderr, text=True, bufsize=1)
        threading.Thread(target=self._read, daemon=True).start()
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                "clientInfo": {"name": "shadowroom-responsiveness", "version": "1"}})
        self._write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _read(self):
        for line in self.process.stdout:
            with contextlib.suppress(json.JSONDecodeError):
                self.messages.put(json.loads(line))
        self.messages.put(None)

    def _write(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params=None, timeout=30.0):
        self.serial += 1
        serial = self.serial
        self._write({"jsonrpc": "2.0", "id": serial, "method": method, "params": params or {}})
        while True:
            packet = self.messages.get(timeout=timeout)
            require(packet is not None, "MCP closed")
            if packet.get("id") == serial:
                return packet

    def call(self, name, arguments=None):
        packet = self.rpc("tools/call", {"name": name, "arguments": arguments or {}})
        result = packet.get("result") or {}
        require(not result.get("isError"), f"{name} failed: {result}")
        return result.get("structuredContent", {})

    def close(self):
        with contextlib.suppress(Exception):
            self.process.stdin.close()
        with contextlib.suppress(Exception):
            self.process.wait(timeout=5)
        self.stderr.close()


# --------------------------------------------------------------------------- process stats
def proc_cpu_seconds(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / CLK_TCK


def main_thread_switches(pid: int) -> int:
    total = 0
    for line in Path(f"/proc/{pid}/task/{pid}/status").read_text().splitlines():
        if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
            total += int(line.split()[1])
    return total


def thread_cpu(pid: int) -> dict:
    out = {}
    for task in Path(f"/proc/{pid}/task").iterdir():
        with contextlib.suppress(FileNotFoundError):
            stat = (task / "stat").read_text()
            name = stat[stat.index("(") + 1:stat.rindex(")")]
            fields = stat.rsplit(")", 1)[1].split()
            out[name] = out.get(name, 0) + (int(fields[11]) + int(fields[12])) / CLK_TCK
    return out


# --------------------------------------------------------------------------- driver
class Driver:
    def __init__(self, args):
        self.args = args
        self.binary = Path(args.binary).resolve()
        self.out = Path(args.output).resolve()
        require(not self.out.exists(), "--output must be a new directory")
        self.out.mkdir(parents=True)
        self.inputs: list[dict] = []
        self.idle: list[dict] = []
        self.scenario = "setup"
        self.xserver = None
        self.app = None
        self.mcp = None

    # ------------------------------------------------------------ environment
    def prepare(self):
        source = Path(self.args.photos).resolve()
        photos = sorted(p for p in source.iterdir() if p.is_file() and p.suffix.lower() in PHOTO_SUFFIXES)
        require(len(photos) >= 2, "Need at least two photos")
        self.originals = {str(p): (p.stat().st_size, p.stat().st_mtime_ns) for p in photos}
        session = self.out / "session"
        for sub in ("home", "config/shadowroom", "data", "cache", "state", "logs"):
            (session / sub).mkdir(parents=True)
        shoot = session / "Shoot"
        shoot.mkdir()
        # Repeat the demo set so held keys and grids have room to move.
        for copy in range(self.args.copies):
            for p in photos:
                shutil.copy2(p, shoot / f"{copy:02d}-{p.name}")
        self.photo_count = len(photos) * self.args.copies
        settings = {"auto_discover_drives": False, "auto_advance": True, "grouping_mode": "off",
                    "theme_mode": "manual", "theme": "Shadowroom Dark", "memory_budget_mb": 512,
                    "show_getting_started": False, "ingest_on_insert": "ask"}
        (session / "config/shadowroom/settings.json").write_text(json.dumps(settings))
        env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET")}
        env.update({"HOME": str(session / "home"), "XDG_CONFIG_HOME": str(session / "config"),
                    "XDG_DATA_HOME": str(session / "data"), "XDG_CACHE_HOME": str(session / "cache"),
                    "XDG_STATE_HOME": str(session / "state"), "GPUI_X11_SCALE_FACTOR": "1",
                    "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "TZ": "UTC",
                    "SHADOWROOM_FRAME_TRACE": str(self.out / "trace.jsonl")})
        if self.args.reduce_motion is not None:
            env["SHADOWROOM_REDUCE_MOTION"] = "1" if self.args.reduce_motion else "0"
        self.session, self.shoot, self.env, self.logs = session, shoot, env, session / "logs"

    def start_x(self):
        require(shutil.which("Xwayland") and os.environ.get("WAYLAND_DISPLAY"),
                "A Wayland session with Xwayland is required for the private X server")
        number = next(n for n in range(120, 220)
                      if not Path(f"/tmp/.X11-unix/X{n}").exists() and not Path(f"/tmp/.X{n}-lock").exists())
        log = (self.logs / "xwayland.log").open("wb")
        self.xserver = subprocess.Popen(["Xwayland", f":{number}", "-geometry", f"{ROOT[0]}x{ROOT[1]}",
                                         "-nolisten", "tcp", "-noreset", "-decorate"],
                                        stdout=log, stderr=log, start_new_session=True)
        wait_for(lambda: Path(f"/tmp/.X11-unix/X{number}").exists(), 10, "private Xwayland")
        time.sleep(0.5)
        self.display = f":{number}"
        self.env["DISPLAY"] = self.display
        self.x = X(self.display)
        xr = subprocess.run(["xrandr", "--display", self.display, "--current"], capture_output=True, text=True)
        self.xrandr = xr.stdout
        rate = None
        for line in xr.stdout.splitlines():
            if "*" in line:
                for token in line.split():
                    if "*" in token:
                        rate = float(token.strip("*+"))
        self.refresh_hz = rate or 60.0

    def launch(self):
        manifest = self.session / "config/shadowroom/automation.json"
        began = time.monotonic()
        self.app = subprocess.Popen([str(self.binary), str(self.shoot)], env=self.env,
                                    stdout=(self.logs / "app.stdout.log").open("wb"),
                                    stderr=(self.logs / "app.stderr.log").open("wb"), start_new_session=True)

        def ready():
            require(self.app.poll() is None, "App exited during launch")
            with contextlib.suppress(FileNotFoundError, json.JSONDecodeError):
                return json.loads(manifest.read_text()).get("pid") == self.app.pid
            return False

        wait_for(ready, 30, "automation endpoint")
        self.mcp = MCP(self.binary, self.env, self.logs)
        self.window = wait_for(lambda: self.x.find_window(self.app.pid), 15, "app window")
        self.x.place(self.window, WINDOW)
        wait_for(lambda: self.x.geometry(self.window) == WINDOW, 5, "window size")
        wait_for(lambda: (lambda s: s["total"] >= self.photo_count and not s["indexing"])(self.state()),
                 120, "catalog indexing")
        self.startup_s = time.monotonic() - began
        self.damage = DamageWatcher(self.display, self.window)
        self.x.motion(300, 20)

    def state(self):
        return self.mcp.call("workspace_state")

    def previews_idle(self):
        stats = self.mcp.call("cache_stats")
        return stats.get("queued", 0) == 0 and stats.get("active", 0) == 0

    def settle(self, seconds=None):
        wait_for(self.previews_idle, 30, "preview queue idle", interval=0.1)
        time.sleep(self.args.settle if seconds is None else seconds)

    # ------------------------------------------------------------ input
    KEYSYMS = {"right": "Right", "left": "Left", "up": "Up", "down": "Down", "escape": "Escape",
               "enter": "Return", "space": "space", "comma": "comma", "ctrl": "Control_L",
               "shift": "Shift_L"}

    GPUI_KEYS = {"comma": ",", "BackSpace": "backspace", "enter": "enter"}

    def keysym(self, key):
        return self.KEYSYMS.get(key, key)

    def tap(self, combo: str, label=None, gap=None):
        """Press and release a key chord, recording the main key's send time."""
        parts = combo.split("+")
        mods, main = parts[:-1], parts[-1]
        self.x.focus(self.window)
        for mod in mods:
            self.x.key(self.keysym(mod), True)
        sent = self.x.key(self.keysym(main), True)
        time.sleep(0.012)
        self.x.key(self.keysym(main), False)
        for mod in reversed(mods):
            self.x.key(self.keysym(mod), False)
        expect = "-".join(mods + [self.GPUI_KEYS.get(main, main.lower())])
        self.inputs.append({"scenario": self.scenario, "label": label or combo, "kind": "key", "sent": sent,
                            "expect": expect})
        time.sleep(self.args.gap if gap is None else gap)

    def hold(self, key: str, seconds: float, label=None):
        """Hold a key; the X server's autorepeat generates the repeats."""
        self.x.focus(self.window)
        sent = self.x.key(self.keysym(key), True)
        began = now_ns()
        time.sleep(seconds)
        released = self.x.key(self.keysym(key), False)
        self.inputs.append({"scenario": self.scenario, "label": label or f"hold {key}", "kind": "hold",
                            "sent": sent, "until": released, "began": began})
        time.sleep(self.args.gap)

    def drag(self, start, end, steps=40, interval=0.016, label="drag"):
        self.x.motion(*start)
        time.sleep(0.05)
        self.x.button(1, True)
        time.sleep(0.05)
        for i in range(1, steps + 1):
            x = round(start[0] + (end[0] - start[0]) * i / steps)
            y = round(start[1] + (end[1] - start[1]) * i / steps)
            sent = self.x.motion(x, y)
            self.inputs.append({"scenario": self.scenario, "label": label, "kind": "drag", "sent": sent})
            time.sleep(interval)
        self.x.button(1, False)
        time.sleep(self.args.gap)

    def measure_idle(self, label, seconds=None):
        seconds = seconds or self.args.idle
        time.sleep(1.5)  # outlast GPUI's one-second post-input presentation window
        pid = self.app.pid
        cpu0, sw0, threads0, t0 = proc_cpu_seconds(pid), main_thread_switches(pid), thread_cpu(pid), now_ns()
        time.sleep(seconds)
        cpu1, sw1, threads1, t1 = proc_cpu_seconds(pid), main_thread_switches(pid), thread_cpu(pid), now_ns()
        elapsed = (t1 - t0) / 1e9
        busy = {k: round((threads1.get(k, 0) - v) / elapsed * 100, 2) for k, v in threads0.items()
                if threads1.get(k, 0) - v > 0}
        self.idle.append({"scenario": label, "start": t0, "end": t1, "seconds": round(elapsed, 3),
                          "cpu_percent": round((cpu1 - cpu0) / elapsed * 100, 2),
                          "main_thread_wakeups_per_s": round((sw1 - sw0) / elapsed, 1),
                          "threads_cpu_percent": busy})

    # ------------------------------------------------------------ scenarios
    def run_scenarios(self):
        n = self.photo_count
        steps = min(self.args.samples, n - 1)
        mcp = self.mcp
        mcp.call("view", {"mode": "loupe"})
        mcp.call("zoom", {"mode": "fit"})
        mcp.call("select", {"index": 0})
        self.settle()
        self.scenario = "idle_loupe"
        self.measure_idle("idle_loupe")

        self.scenario = "loupe_next"
        for _ in range(steps):
            self.tap("right")
        self.scenario = "loupe_previous"
        for _ in range(steps):
            self.tap("left")
        self.settle(0.5)
        self.scenario = "loupe_held"
        self.hold("right", self.args.hold)
        self.settle(0.5)
        self.hold("left", self.args.hold)
        self.settle(0.5)

        # Ratings auto-advance; alternate through 1-5 and clear afterwards via MCP.
        mcp.call("select", {"index": 0})
        self.settle()
        self.scenario = "rate_advance"
        for i in range(min(steps, 20)):
            self.tap(str(1 + i % 5))
        self.scenario = "flag_advance"
        mcp.call("select", {"index": 0})
        self.settle()
        for i in range(min(steps, 20)):
            self.tap("p" if i % 2 == 0 else "x")

        # Without auto-advance the decision changes on the photo in place.
        self.scenario = "rate_in_place"
        mcp.call("settings_set", {"settings": {"auto_advance": False}})
        mcp.call("select", {"index": 1})
        self.settle()
        for i in range(min(steps, 20)):
            self.tap(str(1 + i % 5))
        mcp.call("settings_set", {"settings": {"auto_advance": True}})

        self.scenario = "zoom_toggle"
        mcp.call("select", {"index": 2})
        self.settle()
        for _ in range(10):
            self.tap("z", gap=0.4)
        self.state_zoom_full()
        self.scenario = "pan_drag"
        cx, cy = WINDOW[0] // 2, WINDOW[1] // 2
        for i in range(4):
            sign = 1 if i % 2 == 0 else -1
            self.drag((cx - 150 * sign, cy - 80 * sign), (cx + 150 * sign, cy + 80 * sign))
        mcp.call("zoom", {"mode": "fit"})

        self.scenario = "compare_next"
        ids = [p["id"] for p in mcp.call("library_list", {"limit": 200})["photos"]]
        mcp.call("pin", {"id": ids[0]})
        mcp.call("select", {"id": ids[1]})
        mcp.call("view", {"mode": "compare"})
        self.settle()
        for _ in range(min(steps, 24)):
            self.tap("right")
        self.scenario = "compare_previous"
        for _ in range(min(steps, 24)):
            self.tap("left")
        self.settle()
        self.measure_idle("idle_compare")

        self.scenario = "view_switch"
        mcp.call("select", {"index": 0})
        for _ in range(8):
            self.tap("g", label="to grid", gap=0.5)
            self.tap("l", label="to loupe", gap=0.5)
            self.tap("c", label="to compare", gap=0.5)
        self.tap("g", label="to grid", gap=0.5)
        self.settle()
        self.measure_idle("idle_grid")

        self.scenario = "grid_arrows"
        mcp.call("select", {"index": 0})
        self.settle(0.5)
        for key in ["right"] * 10 + ["down"] * 6 + ["left"] * 10 + ["up"] * 6:
            self.tap(key)
        self.scenario = "grid_held"
        self.hold("right", self.args.hold)
        self.settle(0.5)

        self.scenario = "palette_open_close"
        for _ in range(10):
            self.tap("ctrl+k", label="open palette", gap=0.45)
            self.tap("escape", label="close palette", gap=0.45)
        self.scenario = "palette_typing"
        for _ in range(4):
            self.tap("ctrl+k", label="open palette", gap=0.45)
            for ch in "export":
                self.tap(ch, label="type", gap=0.18)
            for _ in range(6):
                self.tap("BackSpace", label="backspace", gap=0.18)
            self.tap("escape", label="close palette", gap=0.45)
        self.tap("ctrl+k", label="open palette", gap=0.45)
        self.measure_idle("idle_palette_open")
        # Starts more than ten seconds after the last key: the caret has stopped blinking.
        self.measure_idle("idle_palette_open_after_10s")
        self.tap("escape", label="close palette", gap=0.45)

        self.scenario = "preferences_open_close"
        for _ in range(10):
            self.tap("ctrl+comma", label="open preferences", gap=0.5)
            self.tap("escape", label="close preferences", gap=0.5)
        self.tap("ctrl+comma", label="open preferences", gap=0.5)
        self.measure_idle("idle_preferences_open")
        self.tap("escape", label="close preferences", gap=0.5)
        self.settle(0.5)

    def state_zoom_full(self):
        if self.state()["zoom"] == "fit":
            self.tap("z", gap=0.4)
        self.settle(0.5)

    # ------------------------------------------------------------ lifecycle
    def quit(self):
        if self.mcp:
            with contextlib.suppress(Exception):
                self.mcp.call("quit")
        if self.app:
            try:
                self.app.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(self.app.pid, signal.SIGTERM)
                self.app.wait(timeout=5)
        if self.mcp:
            self.mcp.close()
        with contextlib.suppress(Exception):
            self.damage.close()

    def stop_x(self):
        if self.xserver and self.xserver.poll() is None:
            os.killpg(self.xserver.pid, signal.SIGTERM)
            with contextlib.suppress(subprocess.TimeoutExpired):
                self.xserver.wait(timeout=5)

    def originals_intact(self):
        return all((Path(p).stat().st_size, Path(p).stat().st_mtime_ns) == v for p, v in self.originals.items())


# --------------------------------------------------------------------------- analysis
def load_trace(path: Path):
    """Inputs, handled stamps, presented frames and GPUI's pre-dispatch draws."""
    inputs, handled, frames, dispatch_draws = [], [], [], []
    for line in path.read_text().splitlines():
        record = json.loads(line)
        kind = record.get("k")
        if kind == "input":
            inputs.append(record)
        elif kind == "handled":
            handled.append(record["t"])
        elif kind == "frame":
            (frames if record.get("presented", True) else dispatch_draws).append(record)
    frames.sort(key=lambda f: f["render_start"])
    return inputs, handled, frames, dispatch_draws


def first_at_or_after(values, t, key=lambda v: v):
    lo, hi = 0, len(values)
    while lo < hi:
        mid = (lo + hi) // 2
        if key(values[mid]) < t:
            lo = mid + 1
        else:
            hi = mid
    return lo


def split(frame):
    """Render, layout + prepaint, paint and present shares of one frame."""
    if not frame.get("painted"):
        return {}
    return {"layout_prepaint_ms": (frame["prepainted"] - frame["render_end"]) / 1e6,
            "paint_ms": (frame["painted"] - frame["prepainted"]) / 1e6,
            "present_ms": (frame["present"] - frame["painted"]) / 1e6}


def analyse(driver_inputs, idle, trace_path: Path, damage: list[int], refresh_hz: float):
    app_inputs, handled, frames, dispatch_draws = load_trace(trace_path)
    interval_ms = 1000.0 / refresh_hz
    starts = [f["render_start"] for f in frames]
    samples = []
    # Expand held keys into the app's repeated keystrokes.
    expanded = []
    for item in driver_inputs:
        if item["kind"] == "hold":
            lo = first_at_or_after(app_inputs, item["sent"], key=lambda r: r["t"])
            for record in app_inputs[lo:]:
                if record["t"] > item["until"]:
                    break
                if record["kind"] == "key":
                    expanded.append(dict(item, sent=record["t"], app_t=record["t"], repeat=True))
        else:
            expanded.append(item)
    used = set()
    for index, item in enumerate(expanded):
        app_t = item.get("app_t")
        if app_t is None:
            want = "key" if item["kind"] == "key" else "drag"
            lo = first_at_or_after(app_inputs, item["sent"], key=lambda r: r["t"])
            for j in range(lo, len(app_inputs)):
                record = app_inputs[j]
                if record["t"] - item["sent"] > 500_000_000:
                    break
                if j in used or record["kind"] != want:
                    continue
                if want == "key" and record["detail"].lower() != item["expect"]:
                    continue
                used.add(j)
                app_t = record["t"]
                break
        sample = {"scenario": item["scenario"], "label": item["label"], "sent": item["sent"],
                  "repeat": item.get("repeat", False)}
        if app_t is None:
            sample["missing"] = True
            samples.append(sample)
            continue
        next_sent = expanded[index + 1]["sent"] if index + 1 < len(expanded) else None
        h = first_at_or_after(handled, app_t)
        fi = first_at_or_after(starts, app_t)
        if fi >= len(frames) or (next_sent is not None and frames[fi]["render_start"] > next_sent
                                 and item["kind"] != "drag"):
            sample.update(deliver_ms=(app_t - item["sent"]) / 1e6, no_frame=True)
            samples.append(sample)
            continue
        frame = frames[fi]
        di = first_at_or_after(damage, frame["present"])
        damage_t = damage[di] if di < len(damage) else None
        # The last frame before the next input (or within 1 s) approximates completion.
        limit = min(next_sent or float("inf"), app_t + 1_000_000_000)
        last = fi
        while last + 1 < len(frames) and frames[last + 1]["render_start"] < limit:
            last += 1
        # Loupe/compare: when the current photo first showed a full preview.
        sharp = next((f for f in frames[fi:last + 1] if f.get("photo") in ("fit", "full")), None)
        sample.update({
            "first_frame_photo": frame.get("photo"),
            "input_to_sharp_ms": (sharp["present"] - item["sent"]) / 1e6 if sharp else None,
            "deliver_ms": (app_t - item["sent"]) / 1e6,
            "handle_ms": (handled[h] - app_t) / 1e6 if item["kind"] != "drag" and h < len(handled) else None,
            "wait_ms": (frame["render_start"] - (handled[h] if item["kind"] != "drag" and h < len(handled) else app_t)) / 1e6,
            "render_ms": (frame["render_end"] - frame["render_start"]) / 1e6,
            "cpu_ms": frame.get("cpu_ns", 0) / 1e6,
            **split(frame),
            "frame_ms": (frame["present"] - frame["render_start"]) / 1e6,
            "input_to_present_ms": (frame["present"] - item["sent"]) / 1e6,
            "input_to_damage_ms": (damage_t - item["sent"]) / 1e6 if damage_t and (
                damage_t - frame["present"]) < 100_000_000 else None,
            "input_to_last_frame_ms": (frames[last]["present"] - item["sent"]) / 1e6,
            "frames_until_next_input": last - fi + 1,
            "view": frame["view"], "overlay": frame["overlay"], "textures": frame["textures"],
        })
        samples.append(sample)

    scenarios = {}
    order = []
    for sample in samples:
        if sample["scenario"] not in scenarios:
            scenarios[sample["scenario"]] = []
            order.append(sample["scenario"])
        scenarios[sample["scenario"]].append(sample)
    table = {}
    for name in order:
        rows = scenarios[name]
        good = [r for r in rows if "input_to_present_ms" in r]
        span = (min(r["sent"] for r in rows), max(r["sent"] for r in rows) + 1_000_000_000)
        in_span = [f for f in frames if span[0] <= f["render_start"] <= span[1]]
        frame_ms = [(f["present"] - f["render_start"]) / 1e6 for f in in_span]
        regions = {}
        for f in in_span:
            for region, costs in (f.get("regions") or {}).items():
                regions.setdefault(region, []).append(sum(costs) / 1e6)
        hidden = [f for f in dispatch_draws if span[0] <= f["render_start"] <= span[1]]
        render_ms = [(f["render_end"] - f["render_start"]) / 1e6 for f in in_span]
        table[name] = {
            "inputs": len(rows),
            "inputs_without_new_frame": sum(1 for r in rows if r.get("no_frame")),
            "inputs_unmatched": sum(1 for r in rows if r.get("missing")),
            "deliver_ms": summary([r.get("deliver_ms") for r in rows]),
            "input_to_present_ms": summary([r["input_to_present_ms"] for r in good]),
            "input_to_damage_ms": summary([r["input_to_damage_ms"] for r in good]),
            "input_to_last_frame_ms": summary([r["input_to_last_frame_ms"] for r in good]),
            "input_to_sharp_ms": summary([r["input_to_sharp_ms"] for r in good if r["view"] != "grid"]),
            "first_frame_sharp": sum(1 for r in good if r.get("first_frame_photo") in ("fit", "full")),
            "next_frame_render_ms": summary([r["render_ms"] for r in good]),
            "next_frame_draw_present_ms": summary([r["frame_ms"] for r in good]),
            "all_frames": len(in_span),
            "all_frames_cpu_ms": summary([f["cpu_ns"] / 1e6 for f in in_span if f.get("cpu_ns")]),
            "region_cpu_ms_mean": {k: round(statistics.fmean(v), 3) for k, v in sorted(regions.items())},
            "dispatch_draws": len(hidden),
            "dispatch_draw_cpu_ms": summary([f["cpu_ns"] / 1e6 for f in hidden if f.get("cpu_ns")]),
            "all_frames_render_ms": summary(render_ms),
            "all_frames_draw_present_ms": summary(frame_ms),
            "all_frames_layout_prepaint_ms": summary([split(f).get("layout_prepaint_ms") for f in in_span]),
            "all_frames_paint_ms": summary([split(f).get("paint_ms") for f in in_span]),
            "all_frames_present_ms": summary([split(f).get("present_ms") for f in in_span]),
            "frames_over_one_interval": sum(1 for v in frame_ms if v > interval_ms),
            "missed_ticks": sum(int(v // interval_ms) for v in frame_ms),
            "inputs_presented_after_two_intervals": sum(
                1 for r in good if r["input_to_present_ms"] - r["deliver_ms"] > 2 * interval_ms),
        }
    idle_rows = []
    for row in idle:
        drawn = [f for f in frames if row["start"] <= f["render_start"] <= row["end"]]
        idle_rows.append(dict(row, frames=len(drawn), frames_per_s=round(len(drawn) / row["seconds"], 2),
                              frames_from_poll=sum(1 for f in drawn if f["poll"])))
    return {"refresh_hz": refresh_hz, "interval_ms": round(interval_ms, 3), "scenarios": table,
            "idle": idle_rows, "samples": samples, "frame_count": len(frames), "dispatch_draw_count": len(dispatch_draws),
            "damage_events": len(damage)}


def print_report(result):
    print(f"\nDisplay interval {result['interval_ms']} ms ({result['refresh_hz']} Hz)")
    header = f"{'scenario':24} {'n':>4} {'in→present p50/p95/max ms':>28} {'render p50/p95/max':>22} " \
             f"{'layout p50/p95':>15} {'paint p50/p95':>15} {'present p50/p95':>16} {'cpu p50/p95':>12} {'>1 int':>6} {'nofr':>5}"
    print(header)
    for name, row in result["scenarios"].items():
        def fmt(s, keys=("p50", "p95", "max")):
            return "/".join(f"{s[k]:.1f}" for k in keys) if s else "-"
        print(f"{name:24} {row['inputs']:>4} {fmt(row['input_to_present_ms']):>28} "
              f"{fmt(row['all_frames_render_ms']):>22} {fmt(row['all_frames_layout_prepaint_ms'], ('p50', 'p95')):>15} "
              f"{fmt(row['all_frames_paint_ms'], ('p50', 'p95')):>15} {fmt(row['all_frames_present_ms'], ('p50', 'p95')):>16} "
              f"{fmt(row['all_frames_cpu_ms'], ('p50', 'p95')):>12} "
              f"{row['frames_over_one_interval']:>6} {row['inputs_without_new_frame']:>5}")
    print("\nMean main-thread CPU ms per presented frame by region (request_layout + prepaint + paint)")
    for name, row in result["scenarios"].items():
        regions = " ".join(f"{k}={v:.2f}" for k, v in row["region_cpu_ms_mean"].items())
        print(f"{name:24} dispatch draws {row['dispatch_draws']:>3}  {regions}")
    print(f"\n{'idle':24} {'cpu %':>7} {'wakeups/s':>10} {'frames/s':>9}")
    for row in result["idle"]:
        print(f"{row['scenario']:24} {row['cpu_percent']:>7} {row['main_thread_wakeups_per_s']:>10} "
              f"{row['frames_per_s']:>9}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary")
    parser.add_argument("--photos")
    parser.add_argument("--output", required=True, help="new directory for the session, trace and report")
    parser.add_argument("--copies", type=int, default=8, help="copies of each photo (default 8)")
    parser.add_argument("--samples", type=int, default=40, help="taps per navigation scenario")
    parser.add_argument("--gap", type=float, default=0.3, help="seconds between taps")
    parser.add_argument("--hold", type=float, default=2.0, help="seconds to hold a key")
    parser.add_argument("--settle", type=float, default=1.0)
    parser.add_argument("--idle", type=float, default=8.0, help="seconds per idle measurement")
    parser.add_argument("--reduce-motion", type=int, choices=(0, 1), default=None,
                        help="set SHADOWROOM_REDUCE_MOTION for the app")
    parser.add_argument("--label", default="")
    parser.add_argument("--keep-session", action="store_true",
                        help="keep the private profile, photo copies and caches after the run")
    parser.add_argument("--reanalyse", action="store_true",
                        help="re-read an existing --output directory instead of running the app")
    args = parser.parse_args()
    if args.reanalyse:
        out = Path(args.output)
        raw = json.loads((out / "driver.json").read_text())
        report = json.loads((out / "report.json").read_text())
        report.update(analyse(raw["inputs"], raw["idle"], out / "trace.jsonl", raw["damage"], raw["refresh_hz"]))
        (out / "report.json").write_text(json.dumps(report, indent=1))
        print_report(report)
        return
    if not (args.binary and args.photos):
        parser.error("--binary and --photos are required unless --reanalyse")
    driver = Driver(args)
    driver.loadavg_start = os.getloadavg()
    driver.prepare()
    report = {"schema_version": 1, "label": args.label, "binary": str(driver.binary),
              "binary_sha256": None, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    import hashlib
    report["binary_sha256"] = hashlib.sha256(driver.binary.read_bytes()).hexdigest()
    try:
        driver.start_x()
        driver.launch()
        report["startup_to_indexed_s"] = round(driver.startup_s, 3)
        driver.run_scenarios()
    finally:
        driver.quit()
        driver.stop_x()
        if not args.keep_session:
            # The photo copies and preview caches are large; keep the trace and report.
            shutil.rmtree(driver.session, ignore_errors=True)
    report["originals_intact"] = driver.originals_intact()
    report["environment"] = {
        "platform": platform.platform(), "machine": platform.machine(), "python": platform.python_version(),
        "cpu": next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                     if line.startswith("model name")), None),
        "logical_cpus": os.cpu_count(), "loadavg_start": driver.loadavg_start, "loadavg_end": os.getloadavg(),
        "x_server": "private rootful Xwayland", "window": list(WINDOW), "photos": driver.photo_count,
        "xrandr": driver.xrandr.strip().splitlines()[:3],
    }
    (driver.out / "driver.json").write_text(json.dumps(
        {"inputs": driver.inputs, "idle": driver.idle, "damage": driver.damage.times,
         "refresh_hz": driver.refresh_hz}))
    result = analyse(driver.inputs, driver.idle, driver.out / "trace.jsonl", driver.damage.times,
                     driver.refresh_hz)
    report.update(result)
    (driver.out / "report.json").write_text(json.dumps(report, indent=1))
    print_report(result)
    print(f"\nReport: {driver.out / 'report.json'}")
    require(report["originals_intact"], "Source photos changed")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
