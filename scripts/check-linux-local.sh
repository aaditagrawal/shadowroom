#!/usr/bin/env bash
# Use development headers extracted in a user-owned directory without modifying
# system packages. Default: check the desktop app; pass cargo arguments to build.
set -euo pipefail

runner=cargo
if [[ ${1:-} == --help ]]; then
    printf '%s\n' 'Usage: scripts/check-linux-local.sh [--runner cargo|mbx|blaze] [cargo arguments...]'
    exit 0
fi
if [[ ${1:-} == --runner ]]; then
    runner=${2:-}
    case "$runner" in
        cargo|mbx|blaze) shift 2 ;;
        *) printf '%s\n' '--runner requires cargo, mbx, or blaze.' >&2; exit 2 ;;
    esac
fi

resolve_tool() {
    local name=$1 directory
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return
    fi
    for directory in "$HOME/.local/bin" "$HOME/.cargo/bin" "$HOME/.bun/bin"; do
        if [[ -x "$directory/$name" ]]; then
            printf '%s\n' "$directory/$name"
            return
        fi
    done
    printf '%s is required; install it or add it to PATH.\n' "$name" >&2
    return 1
}

# MBX invokes Cargo itself; keep the selected Rust installation discoverable.
cargo_bin=$(resolve_tool cargo)
export PATH="$(dirname -- "$cargo_bin"):$PATH"
runner_bin=$(resolve_tool "$runner")

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

cd -- "$project_root"
if [[ $# -eq 0 ]]; then
    exec "$runner_bin" check --features desktop
else
    exec "$runner_bin" "$@"
fi
