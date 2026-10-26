#!/usr/bin/env python3
"""Isolated native ingest/network acceptance on a real Linux X11 desktop."""
import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time
import traceback

spec = importlib.util.spec_from_file_location("smoke", pathlib.Path(__file__).with_name("native-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class Run(smoke.NativeRun):
    def __init__(self, binary, env, root, number):
        self.stdout = (root / f"native-{number}.stdout.log").open("wb")
        self.stderr = (root / f"native-{number}.stderr.log").open("wb")
        self.process = subprocess.Popen([str(binary)], env=env, stdout=self.stdout, stderr=self.stderr)
        manifest = root / "config/shadowroom/automation.json"

        def ready():
            smoke.require(self.process.poll() is None, "Native app exited before readiness")
            try:
                return json.loads(manifest.read_text()).get("pid") == self.process.pid
            except (FileNotFoundError, json.JSONDecodeError):
                return False

        smoke.wait_for(ready, 20, "private native endpoint")
        self.client = smoke.MCP(binary, env, root, number)
        self.client.initialize()


def exercise(args, root, report):
    binary, agent = pathlib.Path(args.binary).resolve(), pathlib.Path(args.agent).resolve()
    smoke.require(binary.is_file() and agent.is_file(), "Built app and remote agent required")
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.update({f"XDG_{key.upper()}_HOME": str(root / key) for key in ("config", "data", "cache", "state")})
    env["RUST_BACKTRACE"] = "1"
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(json.dumps({"auto_discover_drives": False,
        "auto_advance": False, "memory_budget_mb": 128, "grouping_mode": "off"}))
    report.update(binary=str(binary), binary_sha256=smoke.digest(binary), agent=str(agent),
                  backend="native X11", host=os.uname().nodename)
    card = root / "card"
    card.mkdir()
    for i in range(4):
        smoke.bmp(card / f"frame-{i}.bmp", 256, 160, i)
    (card / "frame-0.xmp").write_text("<x:xmpmeta>sidecar fixture</x:xmpmeta>")
    originals = {p: (smoke.digest(p), p.stat().st_mtime_ns) for p in card.iterdir()}
    runs, used = [], set()

    def record(name, **details):
        report["checks"].append(dict(name=name, result="PASS", **details))
        print("PASS " + name, flush=True)

    def launch():
        run = Run.__new__(Run)
        runs.append(run)
        Run.__init__(run, binary, env, root, len(runs))
        return run

    def ingest_status():
        return c.call("ingest_status")

    def scan(path, count):
        c.call("ingest_scan", {"path": str(path)})
        return smoke.wait_for(lambda: (lambda s: s if not s["scanning"] and s["total"] == count else None)(ingest_status()),
                              20, "complete source scan")

    def plan(options):
        c.call("ingest_plan", {"options": options})
        return smoke.wait_for(lambda: (lambda s: s if s["plan"] is not None else None)(ingest_status()),
                              15, "asynchronous import plan")

    def terminal():
        return smoke.wait_for(lambda: (lambda s: s if s["progress"]["phase"] in ("completed", "failed", "cancelled") else None)(ingest_status()),
                              40, "terminal import")

    try:
        run = launch()
        c = run.client
        smoke.require(run.state()["total"] == 0, "Isolated catalog must start empty")
        scanned = scan(card, 4)
        paged = c.call("ingest_status", {"offset": 1, "limit": 2})
        smoke.require(len(paged["items"]) == 2 and paged["total"] == 4, "Scan pagination wrong")
        c.call("ingest_mark_imported", {"ids": [scanned["items"][3]["id"]]})
        smoke.wait_for(lambda: any(i["seen"] is not None for i in ingest_status()["items"]), 8, "durable seen mark")
        library, backup = root / "library", root / "backup"
        library.mkdir()
        # One collision must move the whole photo+sidecar group together.
        smoke.bmp(library / "frame-0.bmp", 128, 80, 99)
        preexisting_hash = smoke.digest(library / "frame-0.bmp")
        options = dict(library_root=str(library), backup_roots=[str(backup)], folder_template="",
                       file_template="{original}", collision="suffix", selected_ids=[i["id"] for i in scanned["items"]],
                       include_videos=True, selection="all")
        prepared = plan(options)
        smoke.require(prepared["plan"]["ready"], "Valid plan refused: " + repr(prepared))
        c.call("ingest_start")
        done = terminal()
        smoke.require(done["progress"]["phase"] == "completed", "Import failed: " + repr(done))
        for dest in (library, backup):
            smoke.require(smoke.digest(dest / "frame-0-1.bmp") == originals[card / "frame-0.bmp"][0], "Group suffix/copy wrong")
            smoke.require((dest / "frame-0-1.xmp").read_bytes() == (card / "frame-0.xmp").read_bytes(), "Sidecar did not share suffix")
            for i in range(1, 4):
                smoke.require(smoke.digest(dest / f"frame-{i}.bmp") == originals[card / f"frame-{i}.bmp"][0], "Destination bytes wrong")
        smoke.require(smoke.digest(library / "frame-0.bmp") == preexisting_hash, "Existing file overwritten")
        manifest = pathlib.Path(done["progress"]["manifest"])
        smoke.require(manifest.is_file() and json.loads(manifest.read_text()), "Final manifest missing")
        c.call("ingest_history", {"limit": 20})
        history = smoke.wait_for(lambda: (lambda s: s["history"] if s["history"].get("jobs") else None)(ingest_status()),
                                 8, "durable ingest history")
        smoke.wait_for(lambda: run.state()["total"] == 5, 10, "destination catalog publication including existing photo")
        record("ingest scan/paging, seen mark, group collision naming, primary+backup verification and durable history",
               progress=done["progress"], history=history)
        c.call("eject", {"path": str(card)})
        smoke.wait_for(lambda: "unsupported" in str(ingest_status()["error"]).lower(), 8, "folder eject rejection")
        scan(card, 4)
        smoke.require(all(i["seen"] is not None for i in ingest_status()["items"]), "Completed source groups not recognized")
        record("source rescan recognizes committed groups; folder eject never invokes a device operation")

        # Bounded sparse video gives pause/cancel time without private media or downloads.
        slow = root / "slow-card"
        slow.mkdir()
        with (slow / "clip.mov").open("wb") as f:
            f.truncate(256 * 1024 * 1024)
        scan(slow, 1)
        slow_options = dict(options, library_root=str(root / "slow-library"), backup_roots=[], selected_ids=[0])
        smoke.require(plan(slow_options)["plan"]["ready"], "Video plan refused")
        c.call("ingest_start")
        c.call("ingest_pause")
        paused = smoke.wait_for(lambda: (lambda s: s if s["progress"]["phase"] == "paused" else None)(ingest_status()),
                                8, "paused native import")
        c.call("ingest_resume")
        c.call("ingest_pause")
        smoke.wait_for(lambda: ingest_status()["progress"]["phase"] == "paused", 8, "second pause")
        c.call("ingest_cancel")
        cancelled = terminal()
        smoke.require(cancelled["progress"]["phase"] == "cancelled", "Paused import did not cancel")
        smoke.require(not list((root / "slow-library").rglob("clip.mov")), "Cancelled incomplete group published")
        smoke.require(not [p for p in (root / "slow-library").rglob("*") if ".shadowroom-part-" in p.name], "Staging files remain")
        record("native pause/resume/cancel cleans incomplete 256 MiB video", progress=cancelled["progress"])

        # A real remote protocol child on Wiker exercises network paths without
        # depending on fleet availability or granting new SSH access.
        remote_source = root / "remote-photos"
        remote_source.mkdir()
        for i in range(3):
            smoke.bmp(remote_source / f"remote-{i}.bmp", 300, 200, i)
        remote_count = 3
        if args.heif_fixture:
            shutil.copyfile(args.heif_fixture, remote_source / "remote-heif.heic")
            remote_count += 1
        c.call("remote_host_add", {"id": "wiker-test", "display_name": "Wiker test agent", "ssh_alias": "test-only",
            "local_program": str(agent), "local_args": ["serve", "--stdio", "--no-discover", "--root", str(remote_source),
                "--state-dir", str(root / "remote-agent-state")]})
        def host():
            return next((h for h in c.call("remote_hosts")["hosts"] if h["config"]["id"] == "wiker-test"), {})
        smoke.wait_for(lambda: host().get("state") == "connected", 20, "remote hello connection")
        roots = c.call("remote_roots", {"host": "wiker-test"})["roots"]
        smoke.require(len(roots) == 1, "Unexpected remote roots")
        remote_root = roots[0]["id"]
        c.call("remote_index", {"host": "wiker-test", "root": remote_root})
        def remote_photos():
            return [p for p in c.call("library_list", {"scope": "all", "limit": 100})["photos"]
                    if p.get("source") != "Local" and p["filename"].startswith("remote-")]
        photos = smoke.wait_for(lambda: remote_photos() if len(remote_photos()) == remote_count and host().get("state") == "connected" else None,
                                25, "remote catalog and analysis")
        smoke.require(all(p["dhash"] is not None for p in photos), "Remote agent did not analyze every image")
        photo_id = photos[0]["id"]
        c.call("filter", {"filter": "all", "clear_folder": True})
        c.call("select", {"id": photo_id})
        c.call("view", {"mode": "loupe"})
        smoke.wait_for(lambda: list((root / "data").glob("**/remote-previews/**/*.jpg")),
                       15, "actual remote JPEG transferred into the local cache")
        media = smoke.wait_for(lambda: (lambda v: v if v["queued"] == 0 and v["active"] == 0 else None)(c.call("cache_stats")),
                               15, "remote display decode completion")
        smoke.require(media["errors"] == 0 and media["decoded"] > 0, "Remote preview decode failed: " + repr(media))
        c.call("rate", {"ids": [photo_id], "rating": 5})
        run.durable()
        c.call("remote_sync", {"host": "wiker-test", "root": remote_root, "full": True})
        smoke.wait_for(lambda: host().get("state") == "connected", 10, "full remote sync")
        c.call("remote_cancel", {"host": "wiker-test"})
        c.call("remote_disconnect", {"host": "wiker-test"})
        smoke.wait_for(lambda: host().get("state") == "offline", 8, "offline remote")
        c.call("label", {"ids": [photo_id], "label": "purple"})
        run.durable()
        smoke.require(c.call("photo_get", {"id": photo_id})["photo"]["rating"] == 5, "Offline decision lost")
        c.call("remote_connect", {"host": "wiker-test"})
        smoke.wait_for(lambda: host().get("state") == "connected", 15, "remote reconnect")
        c.call("remote_status", {"host": "wiker-test"})
        c.call("remote_host_remove", {"host": "wiker-test"})
        smoke.wait_for(lambda: not c.call("remote_hosts")["hosts"], 8, "host removal")
        smoke.require(c.call("photo_get", {"id": photo_id})["photo"]["label"] == "Purple", "Host removal discarded local decision")
        record("all 10 remote tools: real agent connect/index/analyze/sync, offline decisions, reconnect and removal",
               transport="local stdio child using actual remote protocol; no SSH/LAN claim", analyzed=remote_count)
        restart_count = run.state()["total"]
        used.update(c.used)
        c.call("quit")
        run.finish()
        run = launch()
        c = run.client
        smoke.wait_for(lambda: run.state()["total"] == restart_count, 10, "catalog snapshot after restart")
        smoke.require(c.call("photo_get", {"id": photo_id})["photo"]["label"] == "Purple", "Remote decision lost across restart")
        c.call("ingest_history", {"limit": 20})
        smoke.wait_for(lambda: bool(ingest_status()["history"].get("jobs")), 8, "history after restart")
        c.call("quit")
        used.update(c.used)
        run.finish()
        for p, (digest, mtime) in originals.items():
            smoke.require((smoke.digest(p), p.stat().st_mtime_ns) == (digest, mtime), "Source mutated: " + str(p))
        extension = smoke.TOOLS - smoke.CORE_TOOLS
        smoke.require(extension <= used, "Missing extended tools: " + repr(extension - used))
        report["tools_exercised"] = sorted(used)
        record("restart retains import history and offline decisions; original bytes and mtimes unchanged")
        report["result"] = "PASS"
    finally:
        for run in reversed(runs):
            try:
                run.abort()
            except Exception as error:
                report.setdefault("cleanup_errors", []).append(str(error))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--agent", required=True)
    parser.add_argument("--heif-fixture", help="Optional existing SDR HEIC copied into the test agent root")
    parser.add_argument("--output", help="New evidence directory (must not exist)")
    args = parser.parse_args()
    root = pathlib.Path(args.output).resolve() if args.output else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-native-extended-"))
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = dict(result="RUNNING", session=str(root), checks=[])
    start = time.monotonic()
    try:
        exercise(args, root, report)
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
