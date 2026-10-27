#!/usr/bin/env python3
"""Exercise confirmed file deletion against disposable sources and private OS Trash."""

import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location(
    "extended", pathlib.Path(__file__).with_name("native-extended.py")
)
ext = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ext)
s = ext.smoke


def exercise(binary, root, report):
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env["GPUI_X11_SCALE_FACTOR"] = "1"
    for name in ("config", "data", "cache", "state"):
        env[f"XDG_{name.upper()}_HOME"] = str(root / name)
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(
        json.dumps(
            {
                "auto_discover_drives": False,
                "auto_advance": False,
                "grouping_mode": "off",
                "view_mode": "grid",
                "show_getting_started": False,
            }
        )
    )
    source = root / "photos"
    source.mkdir()
    paths = {}
    for n, name in enumerate(
        ("keep", "cancel", "logical", "system", "other", "permanent", "changed")
    ):
        folder = source / ("second" if name == "other" else "first")
        folder.mkdir(exist_ok=True)
        path = folder / (name + ".bmp")
        s.bmp(path, 256, 160, n)
        paths[name] = path
    sidecar = paths["system"].with_suffix(".xmp")
    sidecar.write_bytes(b"disposable sidecar")
    permanent_sidecar = paths["permanent"].with_suffix(".bmp.xmp")
    permanent_sidecar.write_bytes(b"permanently deleted disposable sidecar")
    before = {
        path: (s.digest(path), path.stat().st_mtime_ns) for path in paths.values()
    }
    before[sidecar] = (s.digest(sidecar), sidecar.stat().st_mtime_ns)
    before[permanent_sidecar] = (
        s.digest(permanent_sidecar),
        permanent_sidecar.stat().st_mtime_ns,
    )
    tool = s.xdotool_path(None)
    s.require(tool, "xdotool required")
    tenv = dict(env)
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tenv["LD_LIBRARY_PATH"] = str(libraries) + (
            ":" + env["LD_LIBRARY_PATH"] if env.get("LD_LIBRARY_PATH") else ""
        )
    run = ext.Run(binary, env, root, 1)
    report.update(binary_sha256=s.digest(binary), checks=[], fixture_root=str(source))

    def record(name):
        report["checks"].append(name)
        print("PASS " + name, flush=True)

    def xd(*args):
        return subprocess.check_output(
            [str(tool), *map(str, args)], env=tenv, text=True, timeout=10
        ).strip()

    def key(value):
        xd("key", "--clearmodifiers", value)
        time.sleep(0.15)

    def confirmation():
        dialog = s.wait_for(
            lambda: run.state()["deletion"]["confirmation"],
            15,
            "frozen deletion confirmation",
        )
        capture = root / f"confirmation-{dialog['mode']}.png"
        if shutil.which("magick") and not capture.exists():
            time.sleep(0.15)
            subprocess.run(
                ["magick", "import", "-window", window, str(capture)],
                env=env,
                check=True,
                capture_output=True,
                timeout=10,
            )
        return dialog

    def confirm():
        key("Tab")
        key("Return")
        return s.wait_for(
            lambda: not run.state()["deletion"]["open"],
            20,
            "confirmed deletion completion",
        )

    try:
        c = run.client
        c.call("import", {"path": str(source)})
        s.wait_for(
            lambda: run.state()["total"] == 7 and not run.state()["indexing"],
            25,
            "disposable library",
        )
        ids = {
            p["filename"].removesuffix(".bmp"): p["id"]
            for p in c.call("library_list", {"scope": "all"})["photos"]
        }
        window = xd(
            "search", "--all", "--pid", run.process.pid, "--name", "^Shadowroom$"
        ).splitlines()[0]
        s.require(
            xd("getwindowpid", window) == str(run.process.pid),
            "Owned native window required",
        )
        xd("windowsize", window, 1280, 900)
        xd("windowfocus", window)
        s.require(
            c.call("settings_get")["trash_mode"] == "system",
            "OS Trash must be the default",
        )
        c.call("select", {"id": ids["cancel"]})
        key("Delete")
        dialog = confirmation()
        s.require(
            dialog["photo_count"] == 1
            and dialog["file_count"] == 1
            and dialog["mode"] == "system",
            repr(dialog),
        )
        key("Return")
        s.require(
            not run.state()["deletion"]["open"] and paths["cancel"].exists(),
            "Default Enter must cancel",
        )
        key("Delete")
        confirmation()
        key("Escape")
        s.require(
            paths["cancel"].exists() and not run.state()["deletion"]["open"],
            "Escape must cancel",
        )
        record(
            "OS Trash default; deletion confirmation; Enter and Escape cancel without changing files"
        )

        c.call("settings_set", {"settings": {"trash_mode": "in_app"}})
        c.call("select", {"id": ids["logical"]})
        key("Delete")
        dialog = confirmation()
        s.require(
            dialog["mode"] == "in_app" and "stay" in dialog["destination"], repr(dialog)
        )
        confirm()
        s.wait_for(
            lambda: run.state()["persistence"]["state"] == "durable",
            10,
            "in-app trash durability",
        )
        s.require(
            c.call("photo_get", {"id": ids["logical"]})["photo"]["trashed"]
            and paths["logical"].exists(),
            "In-app Trash moved files or lost hidden state",
        )
        c.call("undo")
        s.wait_for(
            lambda: run.state()["persistence"]["state"] == "durable",
            10,
            "restore durability",
        )
        s.require(
            not c.call("photo_get", {"id": ids["logical"]})["photo"]["trashed"],
            "Undo did not restore logical Trash",
        )
        record("optional in-app Trash confirms, keeps files, and restores with Undo")

        c.call("settings_set", {"settings": {"trash_mode": "system"}})
        c.call("select", {"ids": [ids["system"], ids["other"]]})
        key("Delete")
        dialog = confirmation()
        s.require(
            dialog["photo_count"] == 2
            and dialog["file_count"] == 3
            and len(dialog["sources"]) == 2,
            repr(dialog),
        )
        s.require(
            set(dialog["sources"])
            == {str(paths["system"].parent), str(paths["other"].parent)},
            "Source locations missing",
        )
        s.require(
            "Trash" in dialog["destination"] and not dialog["permanent"],
            "Trash destination unclear",
        )
        confirm()
        report["trash_report"] = run.state()["deletion"]["report"]
        s.require(
            all(
                not path.exists() for path in (paths["system"], paths["other"], sidecar)
            ),
            "Confirmed OS Trash did not move every grouped file",
        )
        trash = root / "data/Trash"
        trashed = list((trash / "files").iterdir())
        s.require(
            len(trashed) == 3, "Private OS Trash must contain exactly confirmed files"
        )
        s.require(
            sorted(s.digest(p) for p in trashed)
            == sorted(before[p][0] for p in (paths["system"], paths["other"], sidecar)),
            "Trash bytes changed",
        )
        infos = list((trash / "info").glob("*.trashinfo"))
        s.require(
            len(infos) == 3
            and all(
                "Path=" in p.read_text() and "DeletionDate=" in p.read_text()
                for p in infos
            ),
            "OS restoration information missing",
        )
        s.wait_for(lambda: run.state()["total"] == 5, 10, "catalog reconciliation")
        record(
            "multi-folder OS Trash moves photo and sidecar bytes with native restoration information; catalog reconciles"
        )

        c.call("select", {"id": ids["permanent"]})
        key("shift+Delete")
        dialog = confirmation()
        s.require(
            dialog["mode"] == "permanent"
            and dialog["permanent"]
            and dialog["file_count"] == 2,
            repr(dialog),
        )
        s.require(
            dialog["sources"] == [str(paths["permanent"].parent)],
            "Permanent deletion source unclear",
        )
        key("Escape")
        s.require(
            paths["permanent"].exists() and permanent_sidecar.exists(),
            "Cancel permanent deletion changed files",
        )
        key("shift+Delete")
        confirmation()
        confirm()
        s.require(
            not paths["permanent"].exists() and not permanent_sidecar.exists(),
            "Permanent deletion did not remove full group",
        )
        s.require(
            len(list((trash / "files").iterdir())) == 3,
            "Permanent deletion incorrectly moved files to Trash",
        )
        s.wait_for(
            lambda: run.state()["total"] == 4,
            10,
            "permanent deletion catalog reconciliation",
        )
        record(
            "Shift+Delete is separately confirmed, cancellable, permanently removes grouped files, and bypasses Trash"
        )

        c.call("select", {"id": ids["changed"]})
        key("Delete")
        confirmation()
        paths["changed"].write_bytes(
            paths["changed"].read_bytes() + b"changed after confirmation"
        )
        confirm()
        failure = run.state()["deletion"]["report"]
        s.require(
            paths["changed"].exists()
            and not any(f["status"] == "completed" for f in failure["files"]),
            "Changed confirmed file was removed",
        )
        s.require(
            run.state()["error"] and "changed" in run.state()["error"].lower(),
            "Changed file failure invisible",
        )
        record(
            "changed files after confirmation abort safely with visible failure and no permanent fallback"
        )
        for name in ("keep", "cancel", "logical"):
            p = paths[name]
            s.require(
                (s.digest(p), p.stat().st_mtime_ns) == before[p],
                f"Unselected original changed: {name}",
            )
        c.call("quit")
        run.finish()
        run = ext.Run(binary, env, root, 2)
        s.wait_for(
            lambda: run.state()["total"] == 4, 10, "reconciled library after restart"
        )
        run.client.call("quit")
        run.finish()
        record(
            "restart preserves reconciled catalog; all unselected originals retain bytes and mtimes"
        )
        report["result"] = "PASS"
    finally:
        run.abort()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", default="target/release/shadowroom")
    p.add_argument("--output")
    args = p.parse_args()
    root = (
        pathlib.Path(args.output).resolve()
        if args.output
        else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-deletion-"))
    )
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = {"result": "FAIL"}
    try:
        exercise(pathlib.Path(args.binary).resolve(), root, report)
    finally:
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(root / "report.json")


if __name__ == "__main__":
    main()
