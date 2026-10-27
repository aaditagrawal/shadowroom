#!/usr/bin/env python3
"""Isolated X11 acceptance for selection, comparison and import feedback fixes."""

import argparse
import csv
import importlib.util
import io
import json
import os
import pathlib
import shutil
import struct
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
        else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-feedback-"))
    )
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    tool = s.xdotool_path(None)
    s.require(
        tool and shutil.which("magick") and shutil.which("tesseract"),
        "xdotool, ImageMagick and Tesseract required",
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
                "view_mode": "grid",
                "memory_budget_mb": 512,
                "show_getting_started": True,
            }
        )
    )
    source = root / "photos"
    source.mkdir()
    for index in range(60):
        s.bmp(source / f"frame-{index:03}.bmp", 256, 160, index)
    large = root / "large-pairs"
    large.mkdir()
    subprocess.run(
        [
            "magick",
            "-size",
            "5184x3888",
            "pattern:checkerboard",
            "-auto-level",
            "-colorspace",
            "sRGB",
            "-quality",
            "90",
            str(large / "frame-0.jpg"),
        ],
        check=True,
        capture_output=True,
    )
    # Put real EXIF dimensions ahead of entropy data, as a camera JPEG does.
    tiff = b"II*\0" + struct.pack("<IH", 8, 3)
    for tag, value in [(256, 5184), (257, 3888), (274, 1)]:
        tiff += struct.pack("<HHII", tag, 4, 1, value)
    exif = b"Exif\0\0" + tiff + struct.pack("<I", 0)
    jpeg_path = large / "frame-0.jpg"
    encoded = jpeg_path.read_bytes()
    jpeg_path.write_bytes(
        encoded[:2]
        + b"\xff\xe1"
        + struct.pack(">H", len(exif) + 2)
        + exif
        + encoded[2:]
    )
    for index in range(3):
        if index:
            shutil.copyfile(large / "frame-0.jpg", large / f"frame-{index}.jpg")
        # Metadata-only TIFF/RAW: sensor dimensions differ from the JPEG.
        # There are no sensor pixels to develop, so Full must use the JPEG.
        raw = b"II*\0" + struct.pack("<IH", 8, 3)
        for tag, value in [(256, 10368), (257, 7776), (274, 1)]:
            raw += struct.pack("<HHII", tag, 4, 1, value)
        (large / f"frame-{index}.nef").write_bytes(raw + struct.pack("<I", 0))
    originals = {
        p: (s.digest(p), p.stat().st_mtime_ns)
        for folder in (source, large)
        for p in folder.iterdir()
    }
    tenv = dict(env)
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tenv["LD_LIBRARY_PATH"] = str(libraries) + (
            ":" + env["LD_LIBRARY_PATH"] if env.get("LD_LIBRARY_PATH") else ""
        )
    report = {
        "binary_sha256": s.digest(binary),
        "backend": "native GPUI X11 / XTEST",
        "checks": [],
        "timings_seconds": {},
    }
    runs = []

    def record(name):
        report["checks"].append(name)
        print("PASS " + name, flush=True)

    def xd(*arguments):
        return subprocess.check_output(
            [str(tool), *map(str, arguments)], env=tenv, text=True, timeout=10
        ).strip()

    def key(value):
        xd("key", "--clearmodifiers", value)
        time.sleep(0.15)

    def capture(name):
        path = root / f"{name}.png"
        subprocess.run(
            ["magick", "import", "-window", window, str(path)],
            env=env,
            check=True,
            capture_output=True,
            timeout=12,
        )
        return path

    def words():
        result = subprocess.check_output(
            ["tesseract", str(capture("current")), "stdout", "--psm", "11", "tsv"],
            stderr=subprocess.DEVNULL,
            text=True,
        )
        return [
            r
            for r in csv.DictReader(io.StringIO(result), delimiter="\t")
            if r["text"].strip()
        ]

    def word(label):
        matches = [
            r for r in words() if r["text"].strip().casefold() == label.casefold()
        ]
        s.require(matches, f"Could not find visible {label!r}")
        return matches[-1]

    def click(x, y):
        xd("mousemove", "--window", window, int(x), int(y))
        xd("click", 1)
        time.sleep(0.15)

    def drag(x, first, last):
        xd("mousemove", "--window", window, int(x), int(first))
        xd("mousedown", 1)
        for step in range(1, 9):
            xd(
                "mousemove",
                "--window",
                window,
                int(x),
                int(first + (last - first) * step / 8),
            )
            time.sleep(0.025)
        xd("mouseup", 1)
        time.sleep(0.25)

    try:
        run = extended.Run(binary, env, root, 1, dismiss_guide=False)
        runs.append(run)
        c = run.client
        window = xd(
            "search", "--all", "--pid", run.process.pid, "--name", "^Shadowroom$"
        ).splitlines()[0]
        xd("windowsize", window, 1600, 1000)
        xd("windowfocus", window)
        s.wait_for(
            lambda: run.state()["overlays"]["getting_started"], 10, "startup guide"
        )
        capture("startup-guide")
        key("Escape")
        key("F1")
        s.require(
            run.state()["overlays"]["getting_started"], "F1 did not reopen the guide"
        )
        row = word("startup")
        click(
            int(row["left"]) + int(row["width"]) // 2,
            int(row["top"]) + int(row["height"]) // 2,
        )
        s.require(
            not c.call("settings_get")["show_getting_started"],
            "Startup opt-out was not applied",
        )
        key("Escape")
        record("startup guide, F1 reopening, explicit startup opt-out")

        c.call("import", {"path": str(source)})
        s.wait_for(
            lambda: run.state()["total"] == 60 and not run.state()["indexing"],
            30,
            "catalog",
        )
        ids = [p["id"] for p in c.call("library_list", {"scope": "all"})["photos"]]
        subset = ids[4:9]
        c.call("select", {"ids": subset})
        key("Return")
        state = run.state()
        s.require(
            state["filtered_count"] == 5 and set(state["review_subset"]) == set(subset),
            "Review did not freeze the selected subset",
        )
        s.require(not state["selection"], "Review retained a batch-edit selection")
        visited = set()
        for _ in range(9):
            visited.add(run.state()["selected_id"])
            c.call("command", {"name": "next"})
        s.require(visited <= set(subset), "Navigation escaped the subset")
        c.call("flag", {"flag": "pick"})
        s.require(
            sum(
                c.call("photo_get", {"id": i})["photo"]["flag"] == "Pick"
                for i in subset
            )
            == 1,
            "A single-frame decision changed the whole subset",
        )
        c.call("view", {"mode": "compare"})
        s.require(
            run.state()["reference_id"] != run.state()["selected_id"]
            and run.state()["filtered_count"] == 5,
            "Compare at the subset end repeated the same image",
        )
        c.call("view", {"mode": "grid"})
        s.require(
            run.state()["filtered_count"] == 60
            and set(run.state()["selection"]) == set(subset),
            "Catalog did not restore full context and selection",
        )
        record(
            "Review stays within selection; decisions affect one frame; Catalog restores context"
        )

        c.call("select", {"id": ids[0]})
        key("Right")
        c.call("view", {"mode": "loupe"})
        s.require(
            run.state()["filtered_count"] == 60
            and run.state()["review_subset"] is None
            and run.state()["selected_id"] == ids[1],
            "Catalog keyboard focus was not carried into Review",
        )
        c.call("view", {"mode": "grid"})
        c.call("settings_set", {"settings": {"auto_advance": True}})
        c.call("select", {"id": ids[0]})
        key("p")
        s.require(
            run.state()["selected_id"] == ids[1],
            "Single-frame Catalog selection blocked auto-advance",
        )
        c.call("settings_set", {"settings": {"auto_advance": False}})
        record(
            "single-image Review keeps full context; keyboard focus and automatic advance remain usable"
        )

        c.call("select", {"id": ids[0]})
        time.sleep(0.3)
        layout = run.state()["layout"]
        x = 1600 - layout["inspector_width"] - 7
        drag(x, 105, 850)
        s.require(
            run.state()["catalog_scroll"]["visible_rows"][1]
            >= (60 + run.state()["catalog_scroll"]["columns"] - 1)
            // run.state()["catalog_scroll"]["columns"],
            "Catalog scrollbar did not reach the bottom",
        )
        capture("catalog-scroll-bottom")
        record("draggable Catalog scrollbar reaches final row")

        c.call("ingest_scan", {"path": str(source)})
        s.wait_for(
            lambda: (
                not c.call("ingest_status")["scanning"]
                and c.call("ingest_status")["total"] == 60
            ),
            20,
            "import scan",
        )
        time.sleep(0.3)
        layout = run.state()["layout"]
        imp = layout["import"]
        click(layout["sidebar_width"] + imp["timeline_width"] + 8 + 70, 140)
        s.require(
            len(run.state()["layout"]["import"]["selected_ids"]) == 1,
            "Import click did not select one frame",
        )
        key("shift+Down")
        imp = run.state()["layout"]["import"]
        s.require(
            len(imp["selected_ids"]) == imp["columns"] + 1,
            "Import Shift+Down omitted intervening frames",
        )
        s.require(
            any(g["state"] == "partial" for g in imp["time_groups"]),
            "Partially selected time group lacks an indeterminate state",
        )
        key("shift+Up")
        s.require(
            len(run.state()["layout"]["import"]["selected_ids"]) == 1,
            "Import Shift+Up left stale selections",
        )
        capture("partial-time-group")
        record(
            "import range selection extends/shrinks and time groups show partial selection"
        )
        x = 1600 - imp["destination_width"] - 8 - 7
        drag(x, 105, 850)
        imp = run.state()["layout"]["import"]
        s.require(
            imp["visible_rows"][1] >= (60 + imp["columns"] - 1) // imp["columns"],
            "Import scrollbar did not reach the bottom",
        )
        capture("import-scroll-bottom")
        record("draggable import scrollbar reaches final row")

        row = word("Job")
        x, y = int(row["left"]), int(row["top"]) + int(row["height"]) + 20
        click(x + 55, y)
        s.require(
            run.state()["layout"]["import"]["active_field"] == 3,
            "Job field not focused",
        )
        xd("type", "--clearmodifiers", "Alph")
        key("Home")
        key("Right")
        key("Delete")
        s.require(
            run.state()["layout"]["import"]["active_text"] == "Aph",
            "Caret-relative editing did not delete the expected character",
        )
        key("ctrl+a")
        xd("type", "--clearmodifiers", "Alpha")
        time.sleep(0.12)
        on = capture("caret-on")
        time.sleep(0.50)
        off = capture("caret-off")
        for name, path in [("on", on), ("off", off)]:
            subprocess.run(
                [
                    "magick",
                    str(path),
                    "-crop",
                    f"200x28+{x}+{y - 14}",
                    "+repage",
                    str(root / f"caret-crop-{name}.png"),
                ],
                check=True,
            )
        difference = subprocess.run(
            [
                "magick",
                "compare",
                "-metric",
                "AE",
                str(root / "caret-crop-on.png"),
                str(root / "caret-crop-off.png"),
                "null:",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        s.require(
            difference.returncode == 1
            and float(difference.stderr.strip().split()[0]) > 0,
            "Focused input pixels did not change across the caret blink",
        )
        record(
            "text caret blinks visibly; Home/arrows/Delete and select-all edit at its position"
        )
        key("Return")
        key("Escape")

        c.call("import", {"path": str(large)})
        s.wait_for(
            lambda: run.state()["total"] == 63 and not run.state()["indexing"],
            30,
            "large RAW+JPEG pairs",
        )
        c.call("filter", {"folder": str(large)})
        c.call("view", {"mode": "loupe"})
        large_ids = [p["id"] for p in c.call("library_list")["photos"]]
        c.call("select", {"id": large_ids[0]})
        began = time.monotonic()
        c.call("zoom", {"mode": "full"})

        def full_ready(photo_id):
            photo = c.call("photo_get", {"id": photo_id})
            return (
                photo
                if (
                    (photo.get("preview") or {}).get("kind") == "Full"
                    or (photo.get("display_texture") or {}).get("kind") == "Full"
                )
                else None
            )

        first = s.wait_for(lambda: full_ready(large_ids[0]), 40, "20 MP JPEG zoom")
        report["timings_seconds"]["first_20mp_full"] = time.monotonic() - began
        s.require(
            not (first.get("preview") or {}).get("rendered_raw")
            and not first["full_error"],
            "Zoom rendered RAW or hit the memory constraint",
        )
        s.require(
            first["photo"]["width"] == 10368,
            "RAW metadata fixture dimensions were not retained",
        )
        c.call("zoom", {"mode": "fit"})
        time.sleep(0.2)
        fit_percent = run.state()["fit_zoom_percent"]
        key("equal")
        s.require(
            fit_percent < run.state()["zoom_percent"] <= fit_percent * 1.3,
            "Zoom In from Fit used sensor metadata instead of displayed JPEG geometry",
        )
        s.wait_for(
            lambda: full_ready(large_ids[1]), 40, "zoomed neighboring JPEG read-ahead"
        )
        c.call("zoom", {"mode": "custom", "percent": 175})
        began = time.monotonic()
        key("Right")
        s.require(
            run.state()["selected_id"] == large_ids[1]
            and run.state()["zoom_percent"] == 175,
            "Adjacent switch lost zoom or selected the wrong photo",
        )
        s.require(
            full_ready(large_ids[1]), "Prefetched full JPEG was lost on navigation"
        )
        report["timings_seconds"]["warm_switch_acceptance"] = time.monotonic() - began
        key("minus")
        s.require(
            12.5 <= run.state()["zoom_percent"] < 175,
            "Zoom-out did not change magnification",
        )
        c.call("view", {"mode": "compare"})
        s.wait_for(
            lambda: full_ready(run.state()["selected_id"]), 40, "comparison JPEG"
        )
        time.sleep(0.8)
        capture("jpeg-zoom-compare")
        s.require(
            not c.call("photo_get", {"id": run.state()["selected_id"]})["full_error"],
            "20 MP JPEG comparison failed at 512 MiB",
        )
        key("f")
        s.require(run.state()["focus_peaking"], "Focus peaking did not activate")
        s.wait_for(
            lambda: all(
                (c.call("photo_get", {"id": i}).get("display_texture") or {}).get(
                    "peaking"
                )
                for i in [run.state()["reference_id"], run.state()["selected_id"]]
            ),
            30,
            "peaking display textures",
        )
        time.sleep(0.25)
        peaked = capture("focus-peaking-compare")
        layout = run.state()["layout"]
        crop = f"{int(layout['center_width'] - 32)}x{int(layout['photo_height'] - 60)}+{int(layout['sidebar_width'] + 16)}+106"
        pixels = subprocess.check_output(
            ["magick", str(peaked), "-crop", crop, "+repage", "-depth", "8", "rgb:-"]
        )
        s.require(
            sum(
                pixels[i] < 110 and pixels[i + 1] > 200 and pixels[i + 2] > 200
                for i in range(0, len(pixels), 3)
            )
            > 100,
            "Focus peaking did not paint visible cyan detail",
        )
        key("f")
        s.require(not run.state()["focus_peaking"], "Focus peaking did not deactivate")
        record(
            "20 MP JPEG zoom/compare at 512 MiB, zoomed read-ahead, retained custom zoom and focus peaking"
        )

        run.durable()
        c.call("quit")
        run.finish()
        restarted = extended.Run(binary, env, root, 2, dismiss_guide=False)
        runs.append(restarted)
        s.require(
            not restarted.state()["overlays"]["getting_started"],
            "Startup guide opt-out did not survive restart",
        )
        restarted.client.call("quit")
        restarted.finish()
        record("startup guide preference survives restart")
        for path, expected in originals.items():
            s.require(
                (s.digest(path), path.stat().st_mtime_ns) == expected,
                f"Original changed: {path.name}",
            )
        record("all source bytes and mtimes unchanged")
        report["result"] = "PASS"
    finally:
        for run in runs:
            run.abort()
        (root / "report.json").write_text(json.dumps(report, indent=2))
        print("Evidence: " + str(root), flush=True)


if __name__ == "__main__":
    main()
