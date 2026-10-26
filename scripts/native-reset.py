#!/usr/bin/env python3
"""Isolated native X11 acceptance for resetting a photo's rating and flag."""

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
import traceback

SPEC = importlib.util.spec_from_file_location(
    "extended", pathlib.Path(__file__).with_name("native-extended.py")
)
extended = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(extended)
s = extended.smoke


def exercise(args, root, report):
    binary = pathlib.Path(args.binary).resolve()
    tool = s.xdotool_path(None)
    s.require(binary.is_file(), "Built native executable required")
    s.require(
        tool is not None and shutil.which("magick") and shutil.which("tesseract"),
        "xdotool, ImageMagick and Tesseract required",
    )
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env["GPUI_X11_SCALE_FACTOR"] = "1"
    env.update(
        {
            f"XDG_{name.upper()}_HOME": str(root / name)
            for name in ("config", "data", "cache", "state")
        }
    )
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(
        json.dumps(
            {
                "auto_discover_drives": False,
                "auto_advance": False,
                "show_getting_started": False,
                "grouping_mode": "off",
                "view_mode": "grid",
                "memory_budget_mb": 256,
            }
        )
    )
    source = root / "photos"
    source.mkdir()
    for index in range(6):
        s.bmp(source / f"frame-{index:02}.bmp", 256, 160, index)
    originals = {p: (s.digest(p), p.stat().st_mtime_ns) for p in source.iterdir()}
    tenv = dict(env)
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tenv["LD_LIBRARY_PATH"] = str(libraries) + (
            ":" + env["LD_LIBRARY_PATH"] if env.get("LD_LIBRARY_PATH") else ""
        )
    report.update(
        binary=str(binary),
        binary_sha256=s.digest(binary),
        display=env.get("DISPLAY"),
        backend="native GPUI X11 / owned-window XTEST",
    )
    runs = []

    def record(name):
        report["checks"].append({"name": name, "result": "PASS"})
        print("PASS " + name, flush=True)

    def xd(*arguments):
        return subprocess.check_output(
            [str(tool), *map(str, arguments)], env=tenv, text=True, timeout=10
        ).strip()

    def owned_focus():
        s.require(
            run.process.poll() is None
            and xd("getwindowpid", window) == str(run.process.pid),
            "Refusing input to a window outside the test PID",
        )
        xd("windowfocus", window)
        s.require(xd("getwindowfocus") == window, "Owned test window must hold focus")

    def key(value):
        owned_focus()
        xd("key", "--clearmodifiers", value)
        time.sleep(0.12)

    def type_text(value):
        owned_focus()
        xd("type", "--clearmodifiers", value)
        time.sleep(0.15)

    def capture(name):
        time.sleep(0.2)
        path = root / f"{name}.png"
        subprocess.run(
            ["magick", "import", "-window", window, str(path)],
            env=env,
            check=True,
            capture_output=True,
            timeout=10,
        )
        return path

    def click_text(label):
        result = subprocess.check_output(
            ["tesseract", str(capture("current")), "stdout", "--psm", "11", "tsv"],
            stderr=subprocess.DEVNULL,
            text=True,
        )
        matches = [
            row
            for row in csv.DictReader(io.StringIO(result), delimiter="\t")
            if row["text"].strip().casefold() == label.casefold()
        ]
        s.require(matches, f"Missing visible {label!r} control")
        row = max(matches, key=lambda item: int(item["top"]))
        owned_focus()
        xd(
            "mousemove",
            "--window",
            window,
            int(row["left"]) + int(row["width"]) // 2,
            int(row["top"]) + int(row["height"]) // 2,
        )
        xd("click", 1)
        time.sleep(0.15)
        return row

    def decision(photo_id):
        photo = c.call("photo_get", {"id": photo_id})["photo"]
        return photo["rating"], photo["flag"], photo["label"]

    def expect(photo_id, rating, flag, label, reason):
        actual = decision(photo_id)
        s.require(actual == (rating, flag, label), f"{reason}: got {actual}")

    def seed(photo_id, rating=4, flag="pick", label="purple"):
        c.call("rate", {"ids": [photo_id], "rating": rating})
        c.call("flag", {"ids": [photo_id], "flag": flag})
        c.call("label", {"ids": [photo_id], "label": label})

    try:
        run = extended.Run.__new__(extended.Run)
        runs.append(run)
        extended.Run.__init__(run, binary, env, root, 1)
        c = run.client
        c.call("import", {"path": str(source)})
        s.wait_for(
            lambda: run.state()["total"] == 6 and not run.state()["indexing"],
            20,
            "six-photo catalog",
        )
        ids = [
            photo["id"] for photo in c.call("library_list", {"scope": "all"})["photos"]
        ]
        a, b, outsider = ids[:3]
        window = xd(
            "search", "--all", "--pid", run.process.pid, "--name", "^Shadowroom$"
        ).splitlines()[0]
        owned_focus()
        xd("windowsize", window, 1600, 1000)
        time.sleep(0.3)
        c.call("select", {"id": a})
        c.call("view", {"mode": "loupe"})
        c.call("command", {"name": "show_full_folder"})
        s.require(
            run.state()["filtered_count"] == 6,
            "Need neighboring photos to detect advancement",
        )

        # Setting a decision twice continues to mean "set", including with stars.
        for value, wanted in [("p", "Pick"), ("x", "Reject")]:
            key(value)
            key(value)
            s.require(
                decision(a)[1] == wanted, "Repeated flag key unexpectedly toggled"
            )
        key("4")
        key("4")
        expect(a, 4, "Reject", "None", "Repeated rating key must stay at four stars")
        record("repeated P, X and star keys keep setting the same decision")

        seed(a)
        key("r")
        expect(a, 0, "None", "Purple", "R must clear rating and flag, preserving label")
        revision = run.durable()["persistence"]["revision"]
        key("r")
        key("r")
        s.require(
            run.state()["persistence"]["revision"] == revision,
            "Already-clear reset created a persistence revision",
        )
        key("ctrl+z")
        expect(
            a,
            4,
            "Pick",
            "Purple",
            "One undo must restore both fields past no-op resets",
        )
        key("ctrl+shift+z")
        expect(a, 0, "None", "Purple", "One redo must clear both fields atomically")
        record(
            "R is atomic, preserves color, supports undo/redo and ignores already-clear repeats"
        )

        c.call("settings_set", {"settings": {"auto_advance": True}})
        for flag in ["pick", "reject", "none"]:
            seed(a, flag=flag)
            c.call("select", {"id": a})
            key("r")
            expect(a, 0, "None", "Purple", "Reset with auto-advance enabled failed")
            s.require(run.state()["selected_id"] == a, "R advanced to another photo")
        record(
            "R clears picks, rejects and four-star-only ratings without auto-advancing"
        )

        seed(a)
        click_text("Reset")
        expect(a, 0, "None", "Purple", "Visible Reset button failed")
        s.require(run.state()["selected_id"] == a, "Reset button advanced")
        capture("reset-button")
        record("visible Reset action bar button clears rating and flag")

        seed(a, flag="reject")
        key("ctrl+k")
        s.require(run.state()["overlays"]["palette"], "Command palette did not open")
        type_text("reset rating")
        expect(a, 4, "Reject", "Purple", "Typing R in palette must not reset the photo")

        def palette_result_visible():
            text = subprocess.check_output(
                ["tesseract", str(capture("reset-palette")), "stdout", "--psm", "11"],
                stderr=subprocess.DEVNULL,
                text=True,
            )
            return "reset rating and flag" in text.casefold()

        s.wait_for(palette_result_visible, 10, "visible filtered reset palette result")
        key("Return")
        s.require(
            not run.state()["overlays"]["palette"], "Palette command did not execute"
        )
        expect(a, 0, "None", "Purple", "Reset palette command failed")
        s.require(run.state()["selected_id"] == a, "Palette reset auto-advanced")
        seed(a)
        c.call("command", {"name": "reset_decision"})
        expect(a, 0, "None", "Purple", "MCP reset_decision failed")
        s.require(run.state()["selected_id"] == a, "MCP reset auto-advanced")
        record("palette item and MCP reset_decision use the reset action")

        c.call("settings_set", {"settings": {"auto_advance": False}})
        c.call("view", {"mode": "grid"})
        seed(a, flag="pick", label="red")
        seed(b, rating=3, flag="reject", label="blue")
        seed(outsider, rating=2, label="green")
        c.call("select", {"ids": [a, b]})
        key("r")
        expect(a, 0, "None", "Red", "Catalog batch reset missed first photo")
        expect(b, 0, "None", "Blue", "Catalog batch reset missed second photo")
        expect(
            outsider, 2, "Pick", "Green", "Catalog batch reset escaped its selection"
        )
        s.require(
            set(run.state()["selection"]) == {a, b}, "Reset lost Catalog selection"
        )
        key("ctrl+z")
        expect(a, 4, "Pick", "Red", "Catalog batch undo missed first photo")
        expect(b, 3, "Reject", "Blue", "Catalog batch undo was not atomic")
        key("ctrl+shift+z")
        expect(a, 0, "None", "Red", "Catalog batch redo missed first photo")
        expect(b, 0, "None", "Blue", "Catalog batch redo was not atomic")
        record("Catalog reset and atomic undo/redo affect only the selected batch")

        seed(a, flag="pick", label="red")
        seed(b, rating=3, flag="reject", label="blue")
        c.call("view", {"mode": "loupe"})
        s.require(set(run.state()["review_subset"]) == {a, b}, "Review subset missing")
        s.require(
            not run.state()["selection"], "Review retained a batch edit selection"
        )
        active = run.state()["selected_id"]
        other = b if active == a else a
        other_before = decision(other)
        key("r")
        s.require(
            decision(active)[:2] == (0, "None"), "Review active photo did not reset"
        )
        s.require(
            decision(other) == other_before,
            "Review reset changed the rest of its subset",
        )
        s.require(run.state()["selected_id"] == active, "Review reset advanced")
        record(
            "Review reset affects the active photo, preserving the rest of its selected subset"
        )

        c.call("view", {"mode": "grid"})
        seed(a, label="red")
        seed(b, rating=3, flag="reject", label="blue")
        c.call("select", {"ids": [a, b]})
        c.call("view", {"mode": "compare"})
        state = run.state()
        reference, current = state["reference_id"], state["selected_id"]
        s.require(
            reference != current and {reference, current} == {a, b},
            "Expected two comparison panes",
        )
        current_before = decision(current)
        key("Tab")
        s.require(
            run.state()["focused_id"] == reference, "Tab did not focus reference pane"
        )
        key("r")
        s.require(
            decision(reference)[:2] == (0, "None"), "Reference pane did not reset"
        )
        s.require(
            decision(current) == current_before, "Reference reset affected current pane"
        )
        key("Tab")
        s.require(
            run.state()["focused_id"] == current,
            "Tab did not return current-pane focus",
        )
        key("r")
        s.require(
            decision(current)[:2] == (0, "None"),
            "Current comparison pane did not reset",
        )
        capture("reset-compare")
        record("Compare reset follows the focused reference or current pane")

        # The text R must remain input rather than becoming a photo decision.
        seed(current)
        c.call("ingest_scan", {"path": str(source)})
        s.wait_for(lambda: not c.call("ingest_status")["scanning"], 15, "import scan")
        time.sleep(0.3)
        result = subprocess.check_output(
            [
                "tesseract",
                str(capture("import-fields")),
                "stdout",
                "--psm",
                "11",
                "tsv",
            ],
            stderr=subprocess.DEVNULL,
            text=True,
        )
        jobs = [
            row
            for row in csv.DictReader(io.StringIO(result), delimiter="\t")
            if row["text"].strip() == "Job"
        ]
        s.require(jobs, "Job code text field must be visible")
        row = jobs[-1]
        owned_focus()
        xd(
            "mousemove",
            "--window",
            window,
            int(row["left"]) + 55,
            int(row["top"]) + int(row["height"]) + 20,
        )
        xd("click", 1)
        s.require(
            run.state()["layout"]["import"]["active_field"] == 3,
            "Job code field not focused",
        )
        key("r")
        s.require(
            run.state()["layout"]["import"]["active_text"] == "r",
            "R was not entered as text",
        )
        expect(
            current,
            4,
            "Pick",
            "Purple",
            "Typing R in an input reset the underlying photo",
        )
        key("Return")
        key("Escape")
        record("R stays ordinary text inside the import Job code input")

        run.durable()
        expected = {photo_id: decision(photo_id) for photo_id in ids}
        c.call("quit")
        run.finish()
        restarted = extended.Run.__new__(extended.Run)
        runs.append(restarted)
        extended.Run.__init__(restarted, binary, env, root, 2)
        c = restarted.client
        s.wait_for(lambda: restarted.state()["total"] == 6, 15, "catalog restart")
        for photo_id, wanted in expected.items():
            s.require(
                decision(photo_id) == wanted,
                "Reset or retained decision lost on restart",
            )
        c.call("quit")
        restarted.finish()
        record("reset decisions and preserved labels survive durable restart")
        for path, wanted in originals.items():
            s.require(
                (s.digest(path), path.stat().st_mtime_ns) == wanted,
                f"Original changed: {path.name}",
            )
        record("all original bytes and modification times remain unchanged")
    finally:
        for run in reversed(runs):
            run.abort()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/shadowroom")
    parser.add_argument("--output")
    args = parser.parse_args()
    root = (
        pathlib.Path(args.output).resolve()
        if args.output
        else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-reset-"))
    )
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = {"result": "RUNNING", "checks": [], "session": str(root)}
    began = time.monotonic()
    try:
        exercise(args, root, report)
        report["result"] = "PASS"
    except Exception as error:  # noqa: BLE001 - preserve evidence for any harness failure
        report.update(result="FAIL", failure=str(error))
        (root / "failure.txt").write_text(traceback.format_exc())
        print("FAIL " + str(error), flush=True)
    report["elapsed_seconds"] = time.monotonic() - began
    path = root / "report.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"result": report["result"], "report": str(path)}), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
