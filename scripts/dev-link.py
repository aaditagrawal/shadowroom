#!/usr/bin/env python3
"""Build or run Shadowroom with a pinned, optional linker on Linux x86_64."""

import argparse
import fcntl
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Linker:
    directory: str
    executable: str
    url: str
    sha256: str


LINKERS = {
    "wild": Linker(
        directory="wild-linker-0.11.0-x86_64-unknown-linux-gnu",
        executable="wild",
        url=(
            "https://github.com/wild-linker/wild/releases/download/0.11.0/"
            "wild-linker-0.11.0-x86_64-unknown-linux-gnu.tar.gz"
        ),
        sha256="25012a78de164686f552b7703b5d0223adab4829c79cd1fb351108221d61c7d8",
    ),
    "mold": Linker(
        directory="mold-3.0.0-x86_64-linux",
        executable="bin/mold",
        url=(
            "https://github.com/rui314/mold/releases/download/v3.0.0/"
            "mold-3.0.0-x86_64-linux.tar.gz"
        ),
        sha256="6c90d4a474c7c0409dfb575be03a5345878ac14fdba18de8b40fa58c60121189",
    ),
}
PROJECT_ROOT = Path(__file__).resolve().parents[1]


def tools_root() -> Path:
    override = os.environ.get("SHADOWROOM_BUILD_TOOLS")
    if override:
        return Path(override).expanduser().resolve()
    data_home = os.environ.get("XDG_DATA_HOME")
    data_root = (
        Path(data_home).expanduser() if data_home else Path.home() / ".local/share"
    )
    return (data_root / "shadowroom-build/tools").resolve()


def require_executable(path: Path) -> None:
    if not path.is_file() or not os.access(path, os.X_OK):
        raise RuntimeError(f"Missing executable: {path}")


def add_adapter(directory: Path, linker: Linker) -> Path:
    require_executable(directory / linker.executable)
    adapter_directory = directory / "ld-bin"
    adapter_directory.mkdir(exist_ok=True)
    adapter = adapter_directory / "ld"
    target = Path("..") / linker.executable
    if adapter.is_symlink():
        if os.readlink(adapter) != str(target):
            raise RuntimeError(
                f"Existing linker adapter has an unexpected target: {adapter}"
            )
    elif adapter.exists():
        raise RuntimeError(f"Refusing to replace an existing file: {adapter}")
    else:
        adapter.symlink_to(target)
    return adapter


def setup(name: str, root: Path) -> None:
    linker = LINKERS[name]
    root.mkdir(parents=True, exist_ok=True)
    directory = root / linker.directory
    with (root / f".{linker.directory}.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if directory.exists():
            add_adapter(directory, linker)
            print(f"Using existing {name}: {directory}", file=sys.stderr)
            return
        curl = shutil.which("curl")
        if curl is None:
            raise RuntimeError(
                "curl is required to download the pinned linker archive."
            )
        with tempfile.TemporaryDirectory(prefix=f".{name}-", dir=root) as temporary:
            staging = Path(temporary)
            archive_path = staging / "linker.tar.gz"
            subprocess.run(
                [
                    curl,
                    "-fsSL",
                    "--retry",
                    "2",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--output",
                    str(archive_path),
                    linker.url,
                ],
                check=True,
            )
            with archive_path.open("rb") as archive_file:
                digest = hashlib.file_digest(archive_file, "sha256").hexdigest()
            if digest != linker.sha256:
                raise RuntimeError(
                    f"SHA-256 mismatch for {name}; refusing to install it."
                )
            extracted = staging / "extracted"
            extracted.mkdir()
            with tarfile.open(archive_path, "r:gz") as archive:
                archive.extractall(extracted, filter="data")
            contents = list(extracted.iterdir())
            if (
                len(contents) != 1
                or contents[0].name != linker.directory
                or not contents[0].is_dir()
            ):
                raise RuntimeError(
                    f"Unexpected top-level directory in the {name} archive."
                )
            add_adapter(contents[0], linker)
            contents[0].rename(directory)
        print(f"Installed {name}: {directory}", file=sys.stderr)


def linker_adapter(name: str, root: Path) -> Path:
    linker = LINKERS[name]
    directory = root / linker.directory
    adapter = directory / "ld-bin/ld"
    try:
        require_executable(directory / linker.executable)
        require_executable(adapter)
        if adapter.resolve() != (directory / linker.executable).resolve():
            raise RuntimeError(f"Unexpected linker adapter: {adapter}")
    except RuntimeError as error:
        raise RuntimeError(
            f"{error}\nRun: uv run --no-project python scripts/dev-link.py "
            f"--linker {name} setup"
        ) from error
    return adapter


def build(
    runner: str, profile: str, adapter: Path, arguments: list[str]
) -> tuple[int, Path | None]:
    command = [
        str(PROJECT_ROOT / "scripts/check-linux-local.sh"),
        "--runner",
        runner,
        "rustc",
        "--locked",
        "--profile",
        profile,
        "--bin",
        "shadowroom",
        "--message-format=json-render-diagnostics",
        *arguments,
        "--",
        "-C",
        "linker-features=-lld",
        "-C",
        f"link-arg=-B{adapter.parent}/",
    ]
    executable = None
    with subprocess.Popen(
        command,
        cwd=PROJECT_ROOT,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
    ) as process:
        assert process.stdout is not None
        for line in process.stdout:
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="", file=sys.stderr, flush=True)
                continue
            if not isinstance(event, dict):
                print(line, end="", file=sys.stderr, flush=True)
                continue
            if event.get("reason") == "compiler-message":
                message = event.get("message")
                if isinstance(message, dict) and isinstance(
                    message.get("rendered"), str
                ):
                    print(message["rendered"], end="", file=sys.stderr, flush=True)
            elif event.get("reason") == "compiler-artifact":
                target = event.get("target")
                path = event.get("executable")
                if (
                    isinstance(target, dict)
                    and target.get("name") == "shadowroom"
                    and "bin" in target.get("kind", [])
                    and isinstance(path, str)
                    and path
                ):
                    executable = Path(path)
        status = process.wait()
    if executable is not None and not executable.is_absolute():
        executable = PROJECT_ROOT / executable
    return (status if status >= 0 else 128 - status), executable


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", choices=("cargo", "mbx"), default="cargo")
    parser.add_argument("--profile", default="dev-fast")
    parser.add_argument("--linker", choices=tuple(LINKERS), default="wild")
    parser.add_argument("task", choices=("setup", "build", "run"))
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine().lower() not in {
        "x86_64",
        "amd64",
    }:
        parser.error("Optional local linkers are supported only on Linux x86_64.")
    if not args.profile or args.profile.startswith("-"):
        parser.error("--profile requires a Cargo profile name.")
    if args.task == "setup":
        if args.arguments:
            parser.error("setup takes no Cargo or application arguments.")
        setup(args.linker, tools_root())
        return 0
    cargo_arguments = args.arguments
    application_arguments = []
    if args.task == "run" and "--" in cargo_arguments:
        boundary = cargo_arguments.index("--")
        application_arguments = cargo_arguments[boundary + 1 :]
        cargo_arguments = cargo_arguments[:boundary]
    adapter = linker_adapter(args.linker, tools_root())
    status, executable = build(args.runner, args.profile, adapter, cargo_arguments)
    if status:
        return status
    if executable is None:
        raise RuntimeError(
            "The build succeeded without reporting the Shadowroom executable."
        )
    require_executable(executable)
    if args.task == "run":
        os.execv(executable, [str(executable), *application_arguments])
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as error:
        raise SystemExit(
            error.returncode if error.returncode >= 0 else 128 - error.returncode
        )
    except (OSError, RuntimeError, tarfile.TarError) as error:
        print(f"dev-link: {error}", file=sys.stderr)
        raise SystemExit(1) from error
    except KeyboardInterrupt:
        raise SystemExit(130) from None
