#!/usr/bin/env python3
"""Native X11 regression checks for preference confirmation and grid range selection."""

import argparse
import csv
import importlib.util
import io
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time

SPEC = importlib.util.spec_from_file_location(
    "extended", pathlib.Path(__file__).with_name("native-extended.py")
)
extended = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(extended)
s = extended.smoke


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/shadowroom")
    parser.add_argument("--output")
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve()
    root = (
        pathlib.Path(args.output).resolve()
        if args.output
        else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-interactions-"))
    )
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    tool = s.xdotool_path(None)
    s.require(
        tool is not None and shutil.which("tesseract") and shutil.which("magick"),
        "xdotool, Tesseract, and ImageMagick required",
    )
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env["GPUI_X11_SCALE_FACTOR"] = "1"
    env.update(
        {
            f"XDG_{key.upper()}_HOME": str(root / key)
            for key in ("config", "data", "cache", "state")
        }
    )
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    settings_file = config / "settings.json"
    settings_file.write_text(
        json.dumps(
            {
                "auto_discover_drives": False,
                "auto_advance": False,
                "grouping_mode": "off",
                "memory_budget_mb": 512,
                "view_mode": "grid",
            }
        )
    )
    source = root / "photos"
    source.mkdir()
    for index in range(20):
        s.bmp(source / f"frame-{index:02}.bmp", 128, 80, index)
    before = {p: (s.digest(p), p.stat().st_mtime_ns) for p in source.iterdir()}
    tenv = dict(env)
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tenv["LD_LIBRARY_PATH"] = str(libraries) + (
            ":" + env["LD_LIBRARY_PATH"] if env.get("LD_LIBRARY_PATH") else ""
        )
    report = {
        "binary_sha256": s.digest(binary),
        "backend": "native GPUI X11, XTEST input",
        "checks": [],
    }
    run = extended.Run.__new__(extended.Run)

    def xd(*arguments):
        return subprocess.check_output(
            [str(tool), *map(str, arguments)], env=tenv, text=True, timeout=8
        ).strip()

    def record(name):
        report["checks"].append(name)
        print("PASS " + name, flush=True)

    try:
        extended.Run.__init__(run, binary, env, root, 1)
        c = run.client
        c.call("import", {"path": str(source)})
        s.wait_for(
            lambda: run.state()["total"] == 20 and not run.state()["indexing"],
            20,
            "test library",
        )
        ids = [
            photo["id"] for photo in c.call("library_list", {"scope": "all"})["photos"]
        ]
        window = xd(
            "search", "--all", "--pid", run.process.pid, "--name", "^Shadowroom$"
        ).splitlines()[0]
        s.require(
            xd("getwindowpid", window) == str(run.process.pid),
            "Owned test window required",
        )
        xd("windowsize", window, 1280, 800)
        xd("windowfocus", window)
        time.sleep(0.4)

        def key(value):
            xd("key", "--clearmodifiers", value)
            time.sleep(0.12)

        def capture(name):
            time.sleep(0.3)  # MCP acceptance can precede the next rendered frame.
            path = root / f"{name}.png"
            subprocess.run(
                ["magick", "import", "-window", window, str(path)],
                env=env,
                check=True,
                capture_output=True,
                timeout=10,
            )
            return path

        def words():
            path = capture("current")
            pixels = subprocess.check_output(
                [
                    "magick",
                    str(path),
                    "-crop",
                    "680x660+300+70",
                    "+repage",
                    "-resize",
                    "200%",
                    "-colorspace",
                    "Gray",
                    "-negate",
                    "png:-",
                ]
            )
            result = subprocess.check_output(
                ["tesseract", "stdin", "stdout", "--psm", "11", "tsv"],
                stderr=subprocess.DEVNULL,
                input=pixels,
            ).decode()
            return [
                row
                for row in csv.DictReader(io.StringIO(result), delimiter="\t")
                if row["text"].strip()
            ]

        def click_text(label):
            found = [
                row
                for row in words()
                if row["text"].strip().lower() == label.lower()
                or (label.isdecimal() and row["text"].strip().lower() == label + "mb")
            ]
            if label == "Apply" and found:
                # The explanatory footer also contains the word Apply.
                found = [max(found, key=lambda row: int(row["left"]))]
            elif (
                label
                in {"Appearance", "Culling", "Import", "Performance", "Controller"}
                and found
            ):
                found = [min(found, key=lambda row: int(row["top"]))]
            s.require(
                len(found) == 1,
                f"Expected one visible '{label}' control, got {len(found)}",
            )
            row = found[0]
            xd(
                "mousemove",
                "--window",
                window,
                300 + (int(row["left"]) + int(row["width"]) // 2) // 2,
                70 + (int(row["top"]) + int(row["height"]) // 2) // 2,
            )
            xd("click", 1)
            time.sleep(0.15)

        for width in [1280, 960]:
            xd("windowsize", window, width, 800)
            time.sleep(0.25)
            c.call("select", {"id": ids[2]})
            key("Down")
            stride = run.state()["selected_index"] - 2
            s.require(stride > 0, "Native Down input did not move the grid focus")
            c.call("select", {"id": ids[2]})
            key("shift+Down")
            s.require(
                set(run.state()["selection"]) == set(ids[2 : 3 + stride]),
                "Shift+Down omitted intervening photos",
            )
            key("shift+Down")
            s.require(
                set(run.state()["selection"]) == set(ids[2 : 3 + stride * 2]),
                "Repeated Shift+Down lost its anchor",
            )
            key("shift+Up")
            s.require(
                set(run.state()["selection"]) == set(ids[2 : 3 + stride]),
                "Shift+Up did not shrink the range",
            )
            key("shift+Up")
            s.require(
                set(run.state()["selection"]) == {ids[2]},
                "Returning to the anchor left stale selection",
            )
            key("Down")
            s.require(
                not run.state()["selection"], "Plain navigation did not clear the range"
            )
            record(
                f"row selection extends and shrinks with {stride} columns at {width}px"
            )
        c.call("select", {"id": ids[-2]})
        key("shift+Down")
        s.require(
            set(run.state()["selection"]) == set(ids[-2:]),
            "Final row did not clamp to the final photo",
        )
        record("range clamps at the final row")

        xd("windowsize", window, 1280, 800)
        c.call("command", {"name": "toggle_preferences"})
        time.sleep(0.25)
        s.wait_for(
            lambda: run.state()["settings_persistence"]["state"] == "durable",
            10,
            "navigation settings saved",
        )
        baseline = c.call("settings_get")
        saved_bytes = settings_file.read_bytes()
        click_text("Sans")
        s.require(
            c.call("settings_get") == baseline, "Font choice changed active preferences"
        )
        s.require(
            settings_file.read_bytes() == saved_bytes,
            "Draft preferences were written to disk",
        )
        selected_before = run.state()["selected_index"]
        key("shift+Down")
        s.require(
            run.state()["selected_index"] == selected_before,
            "Preferences did not isolate photo navigation",
        )
        capture("preferences-draft")
        click_text("Cancel")
        s.require(
            c.call("settings_get") == baseline, "Cancel committed preference changes"
        )
        record(
            "draft font choice leaves active settings and disk untouched; Cancel discards it"
        )

        c.call("command", {"name": "toggle_preferences"})
        click_text("Sans")
        click_text("Apply")
        s.wait_for(
            lambda: run.state()["settings_persistence"]["state"] == "durable",
            10,
            "applied settings saved",
        )
        s.require(
            c.call("settings_get")["interface_font"] == "DM Sans",
            "Apply did not activate the chosen font",
        )
        s.require(
            json.loads(settings_file.read_text())["interface_font"] == "DM Sans",
            "Apply did not save the chosen font",
        )
        record("Apply activates and durably saves preferences")

        c.call("command", {"name": "toggle_preferences"})
        click_text("Geist")
        key("Escape")
        s.require(
            c.call("settings_get")["interface_font"] == "DM Sans",
            "Escape committed the draft",
        )
        record("Escape discards preference edits")
        c.call("command", {"name": "toggle_preferences"})
        click_text("Import")
        click_text("ask")
        s.require(
            c.call("settings_get")["ingest_on_insert"] == "ask",
            "Draft enabled automatic import",
        )
        click_text("Performance")
        click_text("256")
        s.require(
            c.call("cache_stats")["memory_budget_bytes"] == 512 * 1024 * 1024,
            "Draft changed the active memory budget",
        )
        click_text("Cancel")
        s.require(
            c.call("settings_get")["memory_budget_mb"] == 512,
            "Cancel committed a memory-budget change",
        )
        record("browsing import behavior and memory settings has no live side effects")
        c.call("quit")
        run.finish()
        run = extended.Run.__new__(extended.Run)
        extended.Run.__init__(run, binary, env, root, 2)
        restored = run.client.call("settings_get")
        s.require(
            restored["interface_font"] == "DM Sans"
            and restored["ingest_on_insert"] == "ask"
            and restored["memory_budget_mb"] == 512,
            "Restart restored canceled edits or lost applied edits",
        )
        run.client.call("quit")
        run.finish()
        record("restart preserves applied preferences and excludes canceled changes")
        s.require(
            all(
                (s.digest(path), path.stat().st_mtime_ns) == value
                for path, value in before.items()
            ),
            "An original changed",
        )
        report.update(result="PASS", originals_unchanged=True)
    except Exception as error:
        report.update(result="FAIL", error=str(error))
        raise
    finally:
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        run.abort()
    print(root / "report.json")


if __name__ == "__main__":
    main()
