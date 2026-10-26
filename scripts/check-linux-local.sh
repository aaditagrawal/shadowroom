#!/usr/bin/env bash
# Use development headers extracted in a user-owned directory without modifying
# system packages. Default: check the desktop app; pass cargo arguments to build.
set -euo pipefail

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
build_root=${SHADOWROOM_SYSROOT:-"$HOME/.local/share/shadowroom-build"}
build_lib="$build_root/usr/lib64"

if [[ -d "$build_lib/pkgconfig" ]]; then
    export PKG_CONFIG_PATH="$build_lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export C_INCLUDE_PATH="$build_root/usr/include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
    export CPLUS_INCLUDE_PATH="$build_root/usr/include${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}"
    export LIBRARY_PATH="$build_lib${LIBRARY_PATH:+:$LIBRARY_PATH}"

    # Development RPMs contain linker symlinks, while runtimes are installed in
    # /usr/lib64. Repair only links to the exact installed runtime filename. Never
    # substitute a different ABI version or copy runtime libraries into the app.
    for link in "$build_lib"/*.so; do
        [[ -L "$link" ]] || continue
        target=$(readlink -- "$link")
        runtime="/usr/lib64/$(basename -- "$target")"
        if [[ -e "$runtime" && "$target" != "$runtime" ]]; then
            ln -sfn -- "$runtime" "$link"
        fi
    done
fi

if [[ -d /home/linuxbrew/.linuxbrew/lib/pkgconfig ]]; then
    export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:+$PKG_CONFIG_PATH:}/home/linuxbrew/.linuxbrew/lib/pkgconfig"
fi

if command -v cargo >/dev/null 2>&1; then
    cargo_bin=$(command -v cargo)
elif [[ -x "$HOME/.cargo/bin/cargo" ]]; then
    cargo_bin="$HOME/.cargo/bin/cargo"
else
    printf '%s\n' 'cargo is required; install a Rust toolchain or add it to PATH.' >&2
    exit 1
fi

cd -- "$project_root"
if [[ $# -eq 0 ]]; then
    exec "$cargo_bin" check --features desktop
else
    exec "$cargo_bin" "$@"
fi
