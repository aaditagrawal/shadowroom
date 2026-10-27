#!/usr/bin/env python3
"""Exercise the built-in picker with XTEST and a private native app profile."""

import argparse
import importlib.util
import json
import os
import subprocess
import tempfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "extended", Path(__file__).with_name("native-extended.py")
)
ext = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ext)
s = ext.smoke


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/shadowroom")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    root = (
        args.output.resolve()
        if args.output
        else Path(tempfile.mkdtemp(prefix="shadowroom-path-picker-"))
    )
    if args.output:
        root.mkdir(parents=True, exist_ok=False)
    binary = Path(args.binary).resolve()
    tool = s.xdotool_path(None)
    s.require(
        tool is not None and os.environ.get("DISPLAY"),
        "Private X11 DISPLAY and xdotool required",
    )
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    for key in ("config", "data", "cache", "state"):
        env[f"XDG_{key.upper()}_HOME"] = str(root / key)
    config = root / "config/shadowroom"
    config.mkdir(parents=True)
    folders = root / "folders"
    child = folders / "photo folder"
    nested = child / "nested"
    nested.mkdir(parents=True)
    photo = child / "photo.bmp"
    s.bmp(photo, 80, 50, 7)
    original = (s.digest(photo), photo.stat().st_mtime_ns)
    theme = root / "picker-theme.json"
    theme.write_text(
        json.dumps(
            {
                "name": "Picker family",
                "author": "test",
                "themes": [
                    {
                        "name": "Picker theme",
                        "appearance": "dark",
                        "style": {"background": "#102030"},
                    }
                ],
            }
        )
    )
    (config / "settings.json").write_text(
        json.dumps(
            {
                "auto_discover_drives": False,
                "show_getting_started": False,
                "ingest_library_root": str(folders),
                "memory_budget_mb": 128,
            }
        )
    )
    tenv = env.copy()
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tenv["LD_LIBRARY_PATH"] = str(libraries) + (
            ":" + env["LD_LIBRARY_PATH"] if env.get("LD_LIBRARY_PATH") else ""
        )
    run = ext.Run.__new__(ext.Run)
    checks = []

    def xd(*arguments):
        return subprocess.check_output(
            [str(tool), *map(str, arguments)], env=tenv, text=True, timeout=8
        ).strip()

    def picker():
        value = run.state().get("folder_picker")
        return value if value and not value["loading"] else None

    def opened(path):
        value = picker()
        return (
            value
            if value
            and value["directory"] == str(path.resolve())
            and not value["error"]
            else None
        )

    def enter_path(path, key="Return"):
        xd("key", "--clearmodifiers", "ctrl+l", "ctrl+a")
        xd("type", "--clearmodifiers", "--delay", "1", str(path))
        xd("key", "--clearmodifiers", key)

    try:
        ext.Run.__init__(run, binary, env, root, 1)
        window = s.wait_for(
            lambda: xd(
                "search", "--onlyvisible", "--pid", run.process.pid
            ).splitlines()[0],
            10,
            "own X11 window",
        )
        xd("windowfocus", "--sync", window)
        run.client.call("import", {"path": str(child)})
        s.wait_for(
            lambda: run.state()["total"] == 1 and not run.state()["indexing"],
            10,
            "fixture photo indexed",
        )
        run.client.call("command", {"name": "open_folder_picker"})
        value = s.wait_for(lambda: opened(folders), 10, "built-in folder picker")
        s.require(
            any(entry["path"] == str(child) for entry in value["entries"]),
            "Folder listing omitted fixture",
        )
        enter_path(child)
        s.wait_for(lambda: opened(child), 10, "path with spaces")
        # Modal input must consume photo rating shortcuts instead of touching a catalog.
        xd("key", "--clearmodifiers", "ctrl+l", "ctrl+a")
        xd("type", "--clearmodifiers", str(root / "missing"))
        xd("key", "--clearmodifiers", "Return")
        s.wait_for(
            lambda: value if (value := picker()) and value["error"] else None,
            10,
            "unavailable path error",
        )
        s.require(
            run.state()["folder_picker"]["directory"] == str(child),
            "Failed navigation changed current directory",
        )
        enter_path(child)
        s.wait_for(lambda: opened(child), 10, "recovered navigation")
        xd("key", "--clearmodifiers", "Tab", "Return")
        s.wait_for(lambda: opened(nested), 10, "keyboard entry navigation")
        subprocess.run(
            ["magick", "import", "-window", window, str(root / "picker.png")],
            env=env,
            check=True,
            timeout=10,
        )
        xd("key", "--clearmodifiers", "Escape")
        s.wait_for(
            lambda: run.state()["folder_picker"] is None, 5, "picker cancellation"
        )
        checks.append(
            "background listing, paths with spaces, recoverable errors, keyboard navigation and cancellation"
        )
        photo_state = run.client.call("library_list", {"scope": "all", "limit": 1})[
            "photos"
        ][0]
        s.require(
            photo_state["rating"] == 0 and photo_state["flag"] == "None",
            "Picker input changed photo decisions",
        )

        run.client.call("command", {"name": "open_folder_picker"})
        s.wait_for(picker, 10, "folder picker reopen")
        enter_path(child, "ctrl+Return")
        s.wait_for(
            lambda: run.state()["folder_picker"] is None, 10, "folder confirmation"
        )
        s.wait_for(
            lambda: run.client.call("ingest_status").get("source") == str(child),
            10,
            "selected folder reaches ingest",
        )
        run.client.call("command", {"name": "close_overlay"})
        checks.append("confirmed folder reaches existing ingest workflow")

        run.client.call("command", {"name": "toggle_preferences"})
        run.client.call("command", {"name": "open_theme_picker"})
        s.wait_for(picker, 10, "theme picker")
        enter_path(theme, "ctrl+Return")
        s.wait_for(lambda: run.state()["folder_picker"] is None, 10, "theme selection")
        s.require(
            run.state()["overlays"]["preferences"], "Theme selection closed preferences"
        )
        run.client.call("command", {"name": "close_overlay"})
        s.require(
            (s.digest(photo), photo.stat().st_mtime_ns) == original,
            "Picker changed source photo",
        )
        checks.append(
            "theme selection stays in preference draft; original bytes and timestamps unchanged"
        )
        run.client.call("quit")
        run.finish()
        report = {"result": "PASS", "binary_sha256": s.digest(binary), "checks": checks}
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        for check in checks:
            print("PASS " + check)
        print(root)
    finally:
        if hasattr(run, "process") and run.process.poll() is None:
            run.abort()


if __name__ == "__main__":
    main()
