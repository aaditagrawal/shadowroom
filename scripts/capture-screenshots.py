#!/usr/bin/env python3
"""Capture the native Linux app with a private catalog and supplied demo photos."""

import argparse
import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "extended", Path(__file__).with_name("native-extended.py")
)
extended = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(extended)
smoke = extended.smoke


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/shadowroom")
    parser.add_argument(
        "--photos",
        required=True,
        help="Folder of demo photos you can redistribute in screenshots",
    )
    parser.add_argument("--output", default="docs/images")
    args = parser.parse_args()
    binary, photos, output = (
        Path(value).resolve() for value in (args.binary, args.photos, args.output)
    )
    smoke.require(
        binary.is_file() and photos.is_dir(), "Built binary and demo photos required"
    )
    tool = smoke.xdotool_path(None)
    smoke.require(
        tool is not None and shutil.which("magick"), "xdotool and ImageMagick required"
    )
    output.mkdir(parents=True, exist_ok=True)
    session = Path(tempfile.mkdtemp(prefix="shadowroom-screenshots-"))
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env["GPUI_X11_SCALE_FACTOR"] = "1"
    env.update(
        {
            f"XDG_{key.upper()}_HOME": str(session / key)
            for key in ("config", "data", "cache", "state")
        }
    )
    config = session / "config/shadowroom"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(
        json.dumps(
            {
                "auto_discover_drives": False,
                "auto_advance": False,
                "grouping_mode": "off",
                "theme_mode": "manual",
                "interface_font": "Geist Mono",
            }
        )
    )
    tool_env = dict(env)
    libraries = tool.parent.parent / "lib64"
    if libraries.is_dir():
        tool_env["LD_LIBRARY_PATH"] = str(libraries) + (
            ":" + env["LD_LIBRARY_PATH"] if env.get("LD_LIBRARY_PATH") else ""
        )

    def xd(*arguments):
        return subprocess.check_output(
            [str(tool), *map(str, arguments)], env=tool_env, text=True, timeout=8
        ).strip()

    before = {
        p: (smoke.digest(p), p.stat().st_mtime_ns)
        for p in photos.iterdir()
        if p.is_file()
    }
    report = {
        "binary_sha256": smoke.digest(binary),
        "backend": "native GPUI, Linux X11, scale 1",
        "captures": [],
    }
    run = extended.Run.__new__(extended.Run)
    try:
        extended.Run.__init__(run, binary, env, session, 1)
        client = run.client
        client.call("import", {"path": str(photos)})
        smoke.wait_for(
            lambda: run.state()["total"] >= 4 and not run.state()["indexing"],
            30,
            "demo library",
        )
        library = sorted(
            client.call("library_list", {"scope": "all"})["photos"],
            key=lambda p: p["filename"],
        )
        ids = [photo["id"] for photo in library]
        window = smoke.wait_for(
            lambda: xd(
                "search", "--all", "--pid", run.process.pid, "--name", "^Shadowroom$"
            ).splitlines()[0],
            8,
            "owned window",
        )
        smoke.require(
            xd("getwindowpid", window) == str(run.process.pid),
            "Screenshot window must belong to this session",
        )
        xd("windowsize", window, 1440, 900)
        client.call("flag", {"ids": [ids[1], ids[3]], "flag": "pick"})
        client.call("flag", {"ids": [ids[4 % len(ids)]], "flag": "reject"})
        client.call("rate", {"ids": [ids[1]], "rating": 5})
        client.call("rate", {"ids": [ids[3]], "rating": 4})
        client.call("label", {"ids": [ids[1]], "label": "green"})

        def capture(name):
            def settled():
                stats = client.call("cache_stats")
                return stats["queued"] == 0 and stats["active"] == 0

            smoke.wait_for(settled, 30, "settled previews")
            time.sleep(0.8)  # Allow the native renderer to upload completed textures.
            target = output / f"{name}.png"
            subprocess.run(
                ["magick", "import", "-window", window, "-strip", str(target)],
                env=env,
                check=True,
                capture_output=True,
                timeout=12,
            )
            report["captures"].append(
                {
                    "file": target.name,
                    "sha256": smoke.digest(target),
                    "view": run.state()["view"],
                }
            )
            print(target, flush=True)

        client.call("select", {"id": ids[1]})
        client.call("view", {"mode": "loupe"})
        smoke.wait_for(
            lambda: client.call("photo_get", {"id": ids[1]})["preview"],
            25,
            "hero preview",
        )
        capture("review")
        client.call("pin", {"id": ids[0]})
        client.call("select", {"id": ids[5 % len(ids)]})
        client.call("view", {"mode": "compare"})
        capture("compare")
        client.call("view", {"mode": "grid"})
        capture("catalog")
        client.call("select", {"id": ids[1]})
        client.call("view", {"mode": "loupe"})
        client.call(
            "settings_set",
            {"settings": {"theme": "Shadowroom Light", "theme_mode": "manual"}},
        )
        capture("light")
        client.call(
            "settings_set",
            {"settings": {"theme": "Shadowroom Dark", "theme_mode": "manual"}},
        )
        client.call("command", {"name": "toggle_preferences"})
        capture("preferences")
        run.durable()
        client.call("quit")
        run.finish()
        smoke.require(
            all(
                (smoke.digest(p), p.stat().st_mtime_ns) == value
                for p, value in before.items()
            ),
            "Demo original changed",
        )
        report["originals_unchanged"] = True
        (session / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print("Evidence: " + str(session / "report.json"))
    finally:
        run.abort()


if __name__ == "__main__":
    main()
