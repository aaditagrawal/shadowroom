#!/usr/bin/env python3
"""Owned-window X11 acceptance for Review activation and image context actions."""

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

SPEC = importlib.util.spec_from_file_location(
    "extended", pathlib.Path(__file__).with_name("native-extended.py")
)
extended = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(extended)
s = extended.smoke


def exercise(args, root, report):
    binary = pathlib.Path(args.binary).resolve()
    tool = s.xdotool_path(None)
    s.require(binary.is_file() and tool, "Built native app and xdotool required")
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
    for index in range(12):
        s.bmp(source / f"frame-{index:02}.bmp", 256, 160, index)
    originals = {p: (s.digest(p), p.stat().st_mtime_ns) for p in source.iterdir()}
    tenv = dict(env)
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tenv["LD_LIBRARY_PATH"] = str(libraries) + (
            ":" + env["LD_LIBRARY_PATH"] if env.get("LD_LIBRARY_PATH") else ""
        )
    report.update(binary_sha256=s.digest(binary), backend="native GPUI X11 / XTEST")
    run = None

    def xd(*arguments):
        s.require(run.process.poll() is None, "Owned native app exited")
        return subprocess.check_output(
            [str(tool), *map(str, arguments)], env=tenv, text=True, timeout=10
        ).strip()

    def focus():
        s.require(
            xd("getwindowpid", window) == str(run.process.pid),
            "Refusing input to a window outside the test PID",
        )
        xd("windowraise", window)
        xd("windowfocus", window)
        s.require(xd("getwindowfocus") == window, "Owned window must hold focus")

    def key(value):
        focus()
        xd("key", "--clearmodifiers", value)
        time.sleep(0.15)

    def move(x, y):
        focus()
        xd("mousemove", "--window", window, int(x), int(y))
        time.sleep(0.12)
        pointer = dict(
            line.split("=", 1)
            for line in xd("getmouselocation", "--shell").splitlines()
        )
        s.require(
            pointer.get("WINDOW") == window,
            "Pointer did not enter the owned test window; use an isolated Xvfb display",
        )

    def cell(position):
        state = run.state()
        columns = state["catalog_scroll"]["columns"]
        width = (state["layout"]["center_width"] - 32 - 12 * (columns - 1)) / columns
        return (
            state["layout"]["sidebar_width"]
            + 16
            + (position % columns) * (width + 12)
            + width / 2,
            90 + 16 + (position // columns) * 216 + 80,
        )

    def hover(position):
        move(*cell(position))
        s.wait_for(
            lambda: run.state()["catalog_hover"] == ids[position], 3, "image hover"
        )

    def context(position):
        hover(position)
        xd("click", 3)
        return s.wait_for(lambda: run.state()["image_context"], 3, "image context menu")

    def action(label):
        menu = run.state()["image_context"]
        item = next(item for item in menu["actions"] if item["label"] == label)
        move(item["x"], item["y"])
        xd("click", 1)
        s.wait_for(lambda: run.state()["image_context"] is None, 3, "menu action")

    def grid():
        c.call("view", {"mode": "grid"})
        move(400, 20)
        time.sleep(0.1)

    def record(name):
        report["checks"].append(name)
        print("PASS " + name, flush=True)

    def expect_full(photo_id, reason):
        state = run.state()
        s.require(
            state["view"] == "loupe"
            and state["filtered_count"] == 12
            and state["review_subset"] is None
            and state["selected_id"] == photo_id,
            reason,
        )

    try:
        run = extended.Run.__new__(extended.Run)
        extended.Run.__init__(run, binary, env, root, 1)
        c = run.client
        window = xd(
            "search", "--all", "--pid", run.process.pid, "--name", "^Shadowroom$"
        ).splitlines()[0]
        focus()
        xd("windowsize", window, 1600, 1000)
        c.call("import", {"path": str(source)})
        s.wait_for(
            lambda: run.state()["total"] == 12 and not run.state()["indexing"],
            20,
            "catalog",
        )
        ids = [
            photo["id"] for photo in c.call("library_list", {"scope": "all"})["photos"]
        ]
        for shortcut in ("Return", "l"):
            grid()
            c.call("select", {"id": ids[0]})
            hover(1)
            key(shortcut)
            expect_full(
                ids[1], f"{shortcut} did not review hovered image with full context"
            )
        record("Enter and L open the hovered singleton with the full collection")

        grid()
        c.call("select", {"id": ids[0]})
        key("Right")
        key("Return")
        expect_full(ids[1], "Enter lost the keyboard-focused singleton")
        record("Keyboard navigation followed by Enter keeps the full collection")

        subset = ids[4:7]
        for shortcut in ("Return", "l"):
            grid()
            c.call("select", {"ids": subset})
            hover(1)
            key(shortcut)
            state = run.state()
            s.require(
                state["filtered_count"] == 3
                and set(state["review_subset"]) == set(subset),
                f"{shortcut} did not restrict Review to the deliberate multi-selection",
            )
            s.require(
                not state["selection"], "Review retained a batch decision selection"
            )
            for _ in range(6):
                key("Right")
                s.require(
                    run.state()["selected_id"] in subset,
                    "Review escaped selected subset",
                )
            grid()
            s.require(
                set(run.state()["selection"]) == set(subset),
                "Catalog lost saved selection",
            )
        record(
            "Multi-selection Enter/L restrict navigation and restore the Catalog selection"
        )

        c.call("select", {"ids": subset})
        key("Return")
        current = run.state()["selected_id"]
        move(600, 260)
        xd("click", 3)
        s.wait_for(
            lambda: run.state()["image_context"], 3, "subset Review context menu"
        )
        action("Show in review")
        expect_full(current, "Context Show in review did not leave the existing subset")
        grid()
        s.require(
            set(run.state()["selection"]) == set(subset),
            "Leaving a Review subset discarded the saved Catalog selection",
        )
        record(
            "Context Show in review expands an existing subset without losing its Catalog selection"
        )

        c.call("select", {"ids": subset})
        hover(2)
        xd("click", "--repeat", 2, "--delay", 90, 1)
        time.sleep(0.2)
        expect_full(
            ids[2], "Double-click restricted the collection or focused the wrong photo"
        )
        record("Double-click always opens full-context Review at the clicked image")

        grid()
        c.call("select", {"ids": subset})
        menu = context(1)
        s.require(
            menu["photo_id"] == ids[1] and set(run.state()["selection"]) == {ids[1]},
            "Right-click on an unrelated image retained a dangerous old batch selection",
        )
        labels = {item["label"] for item in menu["actions"]}
        s.require(
            {
                "Show in folder",
                "Show in review",
                "Survey burst",
                "Move to Trash…",
                "Permanently delete…",
            }
            <= labels,
            "Required image actions missing",
        )
        action("Pick")
        s.wait_for(
            lambda: c.call("photo_get", {"id": ids[1]})["photo"]["flag"] == "Pick",
            3,
            "context pick",
        )
        s.require(
            all(
                c.call("photo_get", {"id": photo_id})["photo"]["flag"] == "None"
                for photo_id in subset
            ),
            "Context action changed an unrelated earlier selection",
        )
        record(
            "Right-click targets the clicked image and provides the requested file/review/burst/delete actions"
        )

        c.call("select", {"ids": ids[:2]})
        menu = context(1)
        s.require(
            menu["selection_count"] == 2,
            "Right-click discarded a batch containing the clicked image",
        )
        action("Reject")
        s.require(
            all(
                c.call("photo_get", {"id": photo_id})["photo"]["flag"] == "Reject"
                for photo_id in ids[:2]
            ),
            "Context action did not apply to the clicked batch",
        )
        context(1)
        key("Down")
        key("Return")
        expect_full(ids[1], "Context Show in review opened a subset")
        grid()
        s.require(
            set(run.state()["selection"]) == set(ids[:2]),
            "Full-context Review lost the saved batch",
        )
        record(
            "Right-click within a batch preserves it; context Review opens all photos and restores selection"
        )

        c.call("view", {"mode": "compare"})
        state = run.state()
        reference, candidate = state["reference_id"], state["selected_id"]
        candidate_before = c.call("photo_get", {"id": candidate})["photo"]["flag"]
        move(450, 260)
        xd("click", 3)
        menu = s.wait_for(
            lambda: run.state()["image_context"], 3, "reference context menu"
        )
        s.require(
            menu["photo_id"] == reference and run.state()["selected_id"] == candidate,
            "Right-clicking a reference replaced the comparison candidate",
        )
        action("Pick")
        s.require(
            c.call("photo_get", {"id": reference})["photo"]["flag"] == "Pick"
            and c.call("photo_get", {"id": candidate})["photo"]["flag"]
            == candidate_before,
            "Reference context action affected the candidate",
        )
        grid()
        record(
            "Comparison reference actions target the reference and preserve the candidate"
        )

        c.call("group", {"mode": "time", "gap_ms": 2000})
        s.wait_for(lambda: run.state()["groups"], 5, "time grouping")
        context(1)
        action("Survey burst")
        s.require(
            run.state()["view"] == "survey" and run.state()["filtered_count"] == 12,
            "Survey context action failed",
        )
        c.call("view", {"mode": "loupe"})
        move(600, 260)
        xd("click", 3)
        s.wait_for(lambda: run.state()["image_context"], 3, "Review image context menu")
        before = c.call("photo_get", {"id": run.state()["selected_id"]})["photo"][
            "flag"
        ]
        key("p")
        s.require(
            c.call("photo_get", {"id": run.state()["selected_id"]})["photo"]["flag"]
            == before,
            "Photo shortcut escaped the open context menu",
        )
        key("Escape")
        s.require(
            run.state()["image_context"] is None, "Escape did not dismiss image actions"
        )
        record(
            "Survey burst and Review right-click work; the menu owns keyboard shortcuts and Escape"
        )

        grid()
        context(0)
        if shutil.which("magick"):
            subprocess.run(
                [
                    "magick",
                    "import",
                    "-window",
                    window,
                    str(root / "image-context.png"),
                ],
                env=env,
                check=True,
                timeout=10,
            )
        move(400, 20)
        xd("click", 1)
        s.wait_for(
            lambda: run.state()["image_context"] is None, 3, "outside-click dismissal"
        )
        record("Clicking outside dismisses the image menu")
        run.durable()
        c.call("quit")
        run.finish()
        for path, expected in originals.items():
            s.require(
                (s.digest(path), path.stat().st_mtime_ns) == expected,
                f"Original changed: {path.name}",
            )
        record("All original photo bytes and modification times remain unchanged")
    except Exception:
        if run and hasattr(run, "client"):
            report["pointer"] = xd("getmouselocation", "--shell")
            report["window_geometry"] = xd("getwindowgeometry", "--shell", window)
            (root / "failure-state.json").write_text(
                json.dumps(run.state(), indent=2) + "\n"
            )
            if shutil.which("magick"):
                subprocess.run(
                    ["magick", "import", "-window", window, str(root / "failure.png")],
                    env=env,
                    timeout=10,
                    check=False,
                )
        raise
    finally:
        if run and hasattr(run, "process"):
            run.abort()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/shadowroom")
    parser.add_argument("--output")
    args = parser.parse_args()
    root = (
        pathlib.Path(args.output).resolve()
        if args.output
        else pathlib.Path(tempfile.mkdtemp(prefix="shadowroom-review-context-"))
    )
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    report = {"result": "RUNNING", "checks": [], "session": str(root)}
    began = time.monotonic()
    try:
        exercise(args, root, report)
        report["result"] = "PASS"
    except Exception as error:  # noqa: BLE001
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
