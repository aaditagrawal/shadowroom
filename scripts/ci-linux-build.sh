#!/usr/bin/env bash
# CI restores/saves these host cache paths, which are mounted into the EL8 build.
set -euo pipefail
project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$project_root"
engine=${CONTAINER_ENGINE:-docker}
image=${SHADOWROOM_BUILD_IMAGE:-shadowroom-release-el8:ci}
engine_options=()
if [[ $(basename "$engine") == podman ]]; then engine_options+=(--userns=keep-id); fi
rust_sysroot=$(rustc --print sysroot)
cargo_cache=${CARGO_HOME:-"$HOME/.cargo"}
mkdir -p "$cargo_cache" .local/ci-linux/target .local/ci-linux/home
cargo_cache=$(cd "$cargo_cache" && pwd)
[[ -f .local/release-inputs/release-inputs.json ]] || {
    printf '%s\n' 'Run fetch-release-inputs.py before building.' >&2; exit 1;
}
"$engine" run --rm "${engine_options[@]}" --security-opt label=disable --user "$(id -u):$(id -g)" \
    -v "$project_root:/src:ro" -v "$rust_sysroot:/opt/rust:ro" \
    -v "$cargo_cache:/cargo" -v "$project_root/.local/ci-linux:/output" \
    -v "$project_root/.local/release-inputs:/inputs:ro" \
    -w /src -e PATH=/opt/rust/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    -e HOME=/output/home -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/output/target -e LCMS2_STATIC=1 \
    -e CARGO_INCREMENTAL=0 -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
    -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_PROFILE_TEST_DEBUG=0 -e RUST_TEST_THREADS=2 \
    "$image" bash -euo pipefail -c '
        cargo build --locked --release --features desktop --bins
        cargo test --locked --bin shadowroom
        if [[ ! -f /output/codecs/provenance.json ]]; then
            rm -rf /output/codec-work /output/codecs
            scripts/build-release-codecs.sh /inputs /output/codec-work /output/codecs
            rm -rf /output/codec-work
        fi
        # No compiler toolset environment: the EL8 system loaders must work.
        for library in /output/codecs/lib/*.so.*; do
            [[ ! -L "$library" ]] || continue
            dependencies=$(LD_LIBRARY_PATH=/output/codecs/lib ldd "$library")
            printf "%s\n" "$dependencies"
            if [[ "$dependencies" == *"not found"* ]]; then exit 1; fi
        done
        LD_LIBRARY_PATH=/output/codecs/lib LD_BIND_NOW=1 python3 -c \
          "import ctypes; ctypes.CDLL(\"/output/codecs/lib/libraw_r.so\"); ctypes.CDLL(\"/output/codecs/lib/libheif.so.1\")"
        /output/target/release/shadowroom --help
        /output/target/release/shadowroom-remote --version
    '
