#!/usr/bin/env python3
"""Launch an actual AppImage without FUSE and verify its native MCP endpoint."""
import argparse
import importlib.util
import json
import os
import pathlib
import stat
import subprocess
import time
import traceback

spec = importlib.util.spec_from_file_location("extended", pathlib.Path(__file__).with_name("native-extended.py"))
ext = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ext)
s = ext.smoke


def descendant(pid, ancestor):
    for _ in range(20):
        if pid == ancestor:
            return True
        try:
            fields = pathlib.Path(f"/proc/{pid}/status").read_text().splitlines()
            pid = int(next(line.split()[1] for line in fields if line.startswith("PPid:")))
        except (OSError, StopIteration):
            return False
        if pid <= 1:
            return False
    return False


def exercise(args, root, report):
    binary = pathlib.Path(args.binary).resolve()
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    for k in ("config", "data", "cache", "state"):
        env[f"XDG_{k.upper()}_HOME"] = str(root / k)
    (root / "tmp").mkdir()
    env.update(APPIMAGE_EXTRACT_AND_RUN="1", TMPDIR=str(root / "tmp"))
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(json.dumps({"auto_discover_drives": False, "auto_advance": False}))
    source = root / "source"
    source.mkdir()
    s.bmp(source / "photo.bmp", 640, 400, 5)
    process = client = None
    try:
        with (root / "native.stdout").open("wb") as stdout, (root / "native.stderr").open("wb") as stderr:
            process = subprocess.Popen([str(binary)], env=env, stdout=stdout, stderr=stderr)
            descriptor = config / "automation.json"
            def ready():
                s.require(process.poll() is None, "AppImage exited during startup")
                try:
                    value = json.loads(descriptor.read_text())
                    return value if descendant(value["pid"], process.pid) else None
                except (OSError, json.JSONDecodeError):
                    return None
            endpoint = s.wait_for(ready, 30, "AppImage-owned native endpoint")
            s.require(stat.S_IMODE(descriptor.stat().st_mode) == 0o600, "Descriptor permissions")
            client = s.MCP(binary, env, root, 1)
            client.initialize()
            client.call("import", {"path": str(source)})
            s.wait_for(lambda: client.call("workspace_state")["total"] == 1, 15, "AppImage indexing")
            p = client.call("library_list", {"scope": "all", "limit": 10})["photos"][0]
            client.call("view", {"mode": "loupe"})
            client.call("zoom", {"mode": "full"})
            preview = s.wait_for(lambda: (lambda p: p if p and p["kind"] == "Full" else None)(
                client.call("photo_get", {"id": p["id"]})["preview"]), 15, "AppImage native Full preview")
            s.require([preview["width"], preview["height"]] == [640, 400], "AppImage geometry")
            client.call("quit")
            client.finish()
            process.wait(timeout=20)
            s.require(process.returncode == 0 and not descriptor.exists(), "AppImage orderly shutdown")
            report.update(result="PASS", appimage=str(binary), sha256=s.digest(binary),
                launch_mode="actual AppImage runtime, APPIMAGE_EXTRACT_AND_RUN=1; no FUSE", native_pid=endpoint["pid"],
                launcher_pid=process.pid, tools_registered=len(s.TOOLS), tools_exercised=sorted(client.used), preview=preview)
    finally:
        if client and client.process.poll() is None:
            client.abort()
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True)
    p.add_argument("--output", required=True)
    args = p.parse_args()
    root = pathlib.Path(args.output).resolve()
    root.mkdir(parents=True, exist_ok=False)
    report = dict(result="RUNNING")
    start = time.monotonic()
    try:
        exercise(args, root, report)
    except Exception as e:
        report.update(result="FAIL", failure=str(e))
        (root / "failure.txt").write_text(traceback.format_exc())
    report["elapsed_seconds"] = time.monotonic()-start
    (root / "report.json").write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps(dict(result=report["result"], report=str(root / "report.json"))))
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
