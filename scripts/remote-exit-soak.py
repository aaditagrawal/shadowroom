#!/usr/bin/env python3
"""Disconnect a real stdio agent while a sensor RAW preview is running."""
import argparse
import json
import os
import pathlib
import select
import shutil
import struct
import subprocess
import time


def send(process, serial, message):
    body = json.dumps(dict(id=serial, done=True, payload_len=0, message=message)).encode()
    process.stdin.write(struct.pack("<II", len(body)+4, len(body))+body)
    process.stdin.flush()


def receive(process):
    deadline = time.monotonic()+30
    def take(n):
        result = b""
        while len(result) < n:
            if not select.select([process.stdout], [], [], max(0, deadline-time.monotonic()))[0]:
                raise AssertionError("Agent frame timed out")
            chunk = os.read(process.stdout.fileno(), n-len(result))
            if not chunk:
                raise AssertionError("Agent closed mid-frame")
            result += chunk
        return result
    total, size = struct.unpack("<II", take(8))
    assert 0 < size <= total <= 16*1024*1024
    header = json.loads(take(size))
    take(total-4-size)
    return header


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", required=True)
    parser.add_argument("--fixture", required=True)
    parser.add_argument("--rounds", type=int, default=10)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    root = pathlib.Path(args.output).resolve()
    root.mkdir(parents=True, exist_ok=False)
    source = root / "source"
    source.mkdir()
    shutil.copyfile(args.fixture, source / "camera.CR2")
    report = dict(result="RUNNING", checks=[], scope="real agent stdin EOF during requested Full RAW; not proof of a particular codec stage")
    for number in range(args.rounds):
        with (root / f"agent-{number}.stderr").open("wb") as stderr:
            process = subprocess.Popen([str(pathlib.Path(args.agent).resolve()), "serve", "--stdio", "--no-discover",
                "--root", str(source), "--state-dir", str(root / "state")],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr)
            try:
                send(process, 1, dict(type="hello", protocol_version=1, app_version="exit-soak"))
                assert receive(process)["message"]["type"] == "hello"
                send(process, 2, dict(type="list_roots"))
                identity = receive(process)["message"]["roots"][0]["id"]
                send(process, 3, dict(type="index", root=identity))
                while not receive(process)["done"]:
                    pass
                send(process, 4, dict(type="photos", root=identity, since_revision=0))
                key = None
                while True:
                    frame = receive(process)
                    if frame["message"]["type"] == "photo":
                        key = frame["message"]["key"]
                    if frame["done"]:
                        break
                assert key is not None
                send(process, 5, dict(type="preview", key=key, kind="Full", max_bytes=16*1024*1024-256*1024))
                time.sleep(.2 + (number % 3)*.15)
                process.stdin.close()
                process.wait(timeout=15)
                assert process.returncode == 0, f"Agent exit {process.returncode} in round {number}"
                report["checks"].append(dict(round=number+1, exit=process.returncode))
                print(f"PASS agent disconnect round {number+1}", flush=True)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                process.stdout.close()
    report["result"] = "PASS"
    (root / "report.json").write_text(json.dumps(report, indent=2)+"\n")


if __name__ == "__main__":
    main()
