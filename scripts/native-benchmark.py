#!/usr/bin/env python3
"""Bounded synthetic native-Linux 10k-photo benchmark; no builds or downloads."""
import argparse
import importlib.util
import json
import math
import os
import pathlib
import platform
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import traceback


SHARED = pathlib.Path(__file__).with_name("native-smoke.py")
SPEC = importlib.util.spec_from_file_location("shadowroom_native_smoke", SHARED)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, math.ceil(len(ordered) * fraction) - 1))]


def summarize(values):
    return {"samples": len(values), "p50_ms": statistics.median(values),
            "p95_ms": percentile(values, .95), "max_ms": max(values), "all_ms": values}


def process_sample(pid):
    status = pathlib.Path("/proc/%d/status" % pid).read_text()
    result = {"pid": pid}
    for line in status.splitlines():
        key, _, value = line.partition(":")
        if key in ("VmRSS", "VmHWM", "VmSize", "Threads"):
            result[key + ("_kib" if key != "Threads" else "")] = int(value.split()[0])
    try:
        for line in pathlib.Path("/proc/%d/io" % pid).read_text().splitlines():
            key, _, value = line.partition(":")
            if key in ("rchar", "read_bytes", "wchar", "write_bytes"):
                result[key] = int(value)
        result["open_fds"] = len(list(pathlib.Path("/proc/%d/fd" % pid).iterdir()))
    except (FileNotFoundError, PermissionError):
        pass
    return result


class MemorySampler:
    def __init__(self, pid):
        self.pid, self.samples = pid, []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        started = time.monotonic()
        while not self.stop.is_set():
            try:
                self.samples.append({"seconds": time.monotonic() - started, **process_sample(self.pid)})
            except FileNotFoundError:
                break
            self.stop.wait(.1)

    def finish(self):
        self.stop.set()
        self.thread.join(timeout=1)
        return {"interval_seconds": .1, "sample_count": len(self.samples),
                "max_sampled_rss_kib": max((v.get("VmRSS_kib", 0) for v in self.samples), default=0),
                "kernel_hwm_kib": max((v.get("VmHWM_kib", 0) for v in self.samples), default=0),
                "max_threads": max((v.get("Threads", 0) for v in self.samples), default=0),
                "samples": self.samples}


class BenchmarkRun(smoke.NativeRun):
    """Reuse acceptance harness MCP/state/durable/finish/owned-process cleanup."""
    def __init__(self, binary, environment, root):
        self.stdout = (root / "native-1.stdout.log").open("wb")
        self.stderr = (root / "native-1.stderr.log").open("wb")
        self.process = subprocess.Popen([str(binary)], env=environment,
                                        stdout=self.stdout, stderr=self.stderr)
        manifest = root / "config/shadowroom/automation.json"

        def ready():
            smoke.require(self.process.poll() is None, "Owned benchmark app exited; inspect native logs")
            try:
                return json.loads(manifest.read_text()).get("pid") == self.process.pid
            except (FileNotFoundError, json.JSONDecodeError):
                return False

        smoke.wait_for(ready, 20, "owned benchmark endpoint")
        self.client = smoke.MCP(binary, environment, root, 1)
        self.client.initialize()
        smoke.require(self.state()["total"] == 0, "Benchmark catalog must start empty")


def exercise(args, root, report):
    smoke.require(sys.platform.startswith("linux"), "Native Linux is required")
    smoke.require(os.environ.get("DISPLAY"), "An actual X11 DISPLAY is required")
    binary = pathlib.Path(args.binary).expanduser().resolve()
    smoke.require(binary.is_file() and os.access(binary, os.X_OK), "Built executable missing")
    fixtures, seeds = root / "originals", root / "seeds"
    fixtures.mkdir()
    seeds.mkdir()
    started = time.monotonic()
    seed_hashes = {}
    for number in range(16):
        path = seeds / ("seed-%02d.bmp" % number)
        smoke.bmp(path, 128, 80, number)
        os.utime(path, ns=(1700000000000000000, 1700000000000000000))
        seed_hashes[path] = smoke.digest(path)
    for number in range(args.count):
        folder = fixtures / ("folder-%03d" % (number // 250))
        folder.mkdir(exist_ok=True)
        os.link(seeds / ("seed-%02d.bmp" % (number % 16)), folder / ("photo-%05d.bmp" % number))
    report["fixture"] = {"paths": args.count, "distinct_pixel_buffers": 16,
                         "dimensions": [128, 80], "format": "24-bit BMP", "storage": "hardlinks",
                         "logical_file_bytes": args.count * next(iter(seed_hashes)).stat().st_size,
                         "unique_image_bytes": sum(path.stat().st_size for path in seed_hashes),
                         "generation_seconds": time.monotonic() - started,
                         "cache_condition": "New empty catalog; freshly generated source data is OS-page-cache warm"}
    environment = os.environ.copy()
    environment.pop("WAYLAND_DISPLAY", None)
    environment.update(RUST_BACKTRACE="1", XDG_CONFIG_HOME=str(root / "config"),
                       XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
                       XDG_STATE_HOME=str(root / "state"))
    configuration = root / "config/shadowroom"
    configuration.mkdir(parents=True)
    (configuration / "settings.json").write_text(json.dumps({"auto_discover_drives": False,
        "auto_advance": False, "show_getting_started": False,
        "grouping_mode": "off", "view_mode": "grid",
        "memory_budget_mb": args.memory_mb, "cache_budget_mb": 128}))
    report["environment"] = {"platform": platform.platform(), "cpu": platform.processor(),
        "logical_cpus": os.cpu_count(), "binary": str(binary), "binary_bytes": binary.stat().st_size,
        "binary_sha256": smoke.digest(binary),
        "binary_mtime_ns": binary.stat().st_mtime_ns, "build": "Existing %s binary; no compilation" % ("release" if binary.parent.name == "release" else "debug" if binary.parent.name == "debug" else "supplied"),
        "load_average_at_start": list(os.getloadavg()), "concurrent_work": args.context,
        "display": environment["DISPLAY"], "backend": "Native X11; WAYLAND_DISPLAY removed",
        "private_xdg": True, "auto_discover_drives": False, "memory_budget_mib": args.memory_mb,
        "disk_budget_mib": 128, "grouping": "off"}
    if not report["environment"]["cpu"]:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                report["environment"]["cpu"] = line.partition(":")[2].strip()
                break
    try:
        report["environment"]["session_filesystem"] = subprocess.run(
            ["findmnt", "-n", "-o", "FSTYPE,TARGET", "-T", str(root)],
            capture_output=True, text=True, timeout=2, check=True).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        pass
    run = BenchmarkRun.__new__(BenchmarkRun)
    sampler = None
    try:
        started = time.monotonic()
        BenchmarkRun.__init__(run, binary, environment, root)
        report["startup_seconds"] = time.monotonic() - started
        report["owned_pid"] = run.process.pid
        sampler = MemorySampler(run.process.pid)
        client = run.client
        timings = {}

        def measure(category, name, arguments=None):
            begin = time.perf_counter()
            result = client.call(name, arguments)
            timings.setdefault(category, []).append((time.perf_counter() - begin) * 1000)
            return result

        def cache():
            return smoke.wait_for(lambda: (lambda s: s if s.get("memory_budget_bytes", 0) else None)(client.call("cache_stats")),
                                  5, "uncontended cache statistics")

        def settle(timeout=6):
            stable = 0
            end = time.monotonic() + timeout
            latest = cache()
            while time.monotonic() < end:
                latest = cache()
                if latest["queued"] == 0 and latest["active"] == 0:
                    stable += 1
                    if stable >= 3:
                        return latest
                else:
                    stable = 0
                time.sleep(.05)
            return latest

        baseline = process_sample(run.process.pid)
        report["memory_empty"] = baseline
        imported = time.monotonic()
        acceptance = client.call("import", {"path": str(fixtures)})
        done = smoke.wait_for(lambda: (lambda s: s if s["total"] == args.count and not s["indexing"] else None)(run.state()),
                              min(60, args.timeout - 20), "complete synthetic library")
        report["import"] = {"acceptance": acceptance, "end_to_end_seconds": time.monotonic() - imported,
                            "total": done["total"], "filtered_count": done["filtered_count"],
                            "status": done["status"], "error": done["error"]}
        if done["error"]:
            report.setdefault("observed_ui_errors", []).append({"phase": "after_import", "message": done["error"]})
        smoke.require(done["view"] == "grid", "Import must be measured in native grid view")
        report["cache_after_import_grid"] = settle()
        report["memory_after_import_grid"] = process_sample(run.process.pid)
        print("Imported %d synthetic photos in %.3fs" % (args.count, report["import"]["end_to_end_seconds"]), flush=True)

        ids = []
        for offset in range(0, args.count, 1000):
            page = measure("library_page_1000", "library_list", {"scope": "all", "offset": offset, "limit": 1000})
            smoke.require(page["total"] == args.count, "Library page count mismatch")
            ids.extend(photo["id"] for photo in page["photos"])
        smoke.require(len(ids) == args.count and len(set(ids)) == args.count, "Pagination omitted/duplicated IDs")
        report["library_unique_ids"] = len(ids)
        for number in range(args.samples):
            measure("idle_workspace_state", "workspace_state")
            measure("idle_grid_select", "select", {"id": ids[(number * 397) % len(ids)]})
        for number in range(args.samples):
            measure("idle_rate_ack", "rate", {"ids": [ids[number % len(ids)]], "rating": number % 5 + 1})
        run.durable()
        database = next((root / "data").rglob("catalog.sqlite3"))
        lock = smoke.writer_lock(database)
        held = time.monotonic()
        under_write = 0
        try:
            client.call("rate", {"ids": [ids[0]], "rating": 2})
            for number in range(args.samples):
                if time.monotonic() - held > 3:
                    break
                measure("blocked_writer_grid_select", "select", {"id": ids[(number * 263 + 1) % len(ids)]})
                measure("blocked_writer_rate_ack", "rate", {"ids": [ids[number % len(ids)]], "rating": (number + 1) % 5 + 1})
                under_write += 1
            report["writer_blocked"] = {"seconds": time.monotonic() - held,
                                         "select_rate_pairs": under_write,
                                         "persistence_before_unlock": run.state()["persistence"]}
        finally:
            lock.rollback()
            lock.close()
        if run.state()["persistence"]["state"] == "failed":
            client.call("command", {"name": "retry_saves"})
        run.durable()
        report["cache_after_latency"] = settle()
        report["memory_after_latency"] = process_sample(run.process.pid)

        # Warm a small distant loupe working set, then reverse over the same paths.
        client.call("view", {"mode": "loupe"})
        client.call("command", {"name": "show_full_folder"})
        client.call("zoom", {"mode": "fit"})
        navigation = ids[len(ids) // 2:len(ids) // 2 + min(12, len(ids) // 2)]
        before_forward = settle()
        for photo_id in navigation:
            measure("forward_loupe_select", "select", {"id": photo_id})
            settle()
        after_forward = settle()
        for photo_id in reversed(navigation):
            measure("warm_reverse_loupe_select", "select", {"id": photo_id})
            settle()
        after_reverse = settle()
        report["navigation"] = {"working_set_paths": len(navigation),
            "forward_source_reads_delta": after_forward["source_reads"] - before_forward["source_reads"],
            "warm_reverse_source_reads_delta": after_reverse["source_reads"] - after_forward["source_reads"],
            "forward_decodes_delta": after_forward["decoded"] - before_forward["decoded"],
            "warm_reverse_decodes_delta": after_reverse["decoded"] - after_forward["decoded"],
            "before_forward": before_forward, "after_forward": after_forward, "after_reverse": after_reverse,
            "texture_counts": "not exposed by current API; CPU texture bytes and GPU pixel-byte estimate recorded"}
        client.call("view", {"mode": "grid"})
        report["cache_final_grid"] = settle()
        report["memory_final_grid"] = process_sample(run.process.pid)
        report["latency"] = {key: summarize(values) for key, values in timings.items()}
        report["memory_timeline"] = sampler.finish()
        sampler = None
        for path, original_hash in seed_hashes.items():
            smoke.require(smoke.digest(path) == original_hash, "A hardlinked original's bytes changed")
            smoke.require(path.stat().st_mtime_ns == 1700000000000000000, "An original's modification time changed")
        smoke.require(sum(1 for _ in fixtures.rglob("*.bmp")) == args.count, "Unexpected source file count")
        report["originals_unchanged"] = True
        run.durable()
        report["final_state"] = run.state()
        if report["final_state"]["error"] and not any(item["message"] == report["final_state"]["error"] for item in report.get("observed_ui_errors", [])):
            report.setdefault("observed_ui_errors", []).append({"phase": "final", "message": report["final_state"]["error"]})
        client.call("quit")
        run.finish()
        report["normal_exit"] = True
        report["result"] = "COMPLETED_WITH_ERRORS" if report.get("observed_ui_errors") else "PASS"
    finally:
        if sampler:
            report["memory_timeline"] = sampler.finish()
        run.abort()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=str(pathlib.Path(__file__).resolve().parents[1] / "target/debug/shadowroom"))
    parser.add_argument("--count", type=int, default=10000)
    parser.add_argument("--samples", type=int, default=20)
    parser.add_argument("--memory-mb", type=int, default=512, help="Preview budget; default matches application preference (512 MiB)")
    parser.add_argument("--timeout", type=int, default=115, help="Whole benchmark deadline including setup; default 115s")
    parser.add_argument("--context", default="Host not CPU-isolated", help="Record concurrent-work context without changing the workload")
    parser.add_argument("--output", help="New isolated evidence directory")
    args = parser.parse_args()
    smoke.require(100 <= args.count <= 20000 and 5 <= args.samples <= 50, "Unsupported benchmark size/sample count")
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-native-benchmark-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = {"result": "RUNNING", "session": str(root), "benchmark": "synthetic native executable latency", "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    started = time.monotonic()
    def expired(*_):
        raise TimeoutError("Whole benchmark deadline reached")
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, args.timeout)
    print("Benchmark evidence: " + str(root), flush=True)
    try:
        exercise(args, root, report)
    except Exception as error:
        report["result"] = "FAIL"
        report["failure"] = str(error)
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), file=sys.stderr)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    report["elapsed_seconds"] = time.monotonic() - started
    report["load_average_at_finish"] = list(os.getloadavg())
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"result": report["result"], "report": str(root / "report.json"), "seconds": round(report["elapsed_seconds"], 2)}), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
