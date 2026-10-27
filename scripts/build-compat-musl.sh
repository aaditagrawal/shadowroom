#!/usr/bin/env bash
# Static CPU/X11 feasibility build. Build tools are not application dependencies.
set -euo pipefail
compat_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$compat_root"
compat_cargo=$(command -v cargo || true)
[[ -n "$compat_cargo" ]] || compat_cargo="$HOME/.cargo/bin/cargo"
compat_rustc=$(command -v rustc || true)
[[ -n "$compat_rustc" ]] || compat_rustc="$HOME/.cargo/bin/rustc"
[[ -x "$compat_cargo" && -x "$compat_rustc" ]] || { printf '%s\n' 'Rust/Cargo are required.' >&2; exit 1; }
compat_sysroot=$("$compat_rustc" --print sysroot)
compat_host=$("$compat_rustc" -vV | awk '/^host:/ {print $2}')
[[ -d "$compat_sysroot/lib/rustlib/x86_64-unknown-linux-musl/lib" ]] || {
    printf '%s\n' 'Install the target first: rustup target add x86_64-unknown-linux-musl' >&2; exit 1;
}
export SHADOWROOM_COMPAT_LLD="$compat_sysroot/lib/rustlib/$compat_host/bin/rust-lld"
[[ -x "$SHADOWROOM_COMPAT_LLD" ]] || { printf '%s\n' 'The Rust toolchain must include rust-lld.' >&2; exit 1; }
export SHADOWROOM_COMPAT_UVX
SHADOWROOM_COMPAT_UVX=$(command -v uvx)
# Zig supplies musl C headers/compiler for bundled SQLite and BLAKE3. Rust's
# self-contained musl CRT is linked by rust-lld, avoiding duplicate Zig CRTs.
"$SHADOWROOM_COMPAT_UVX" --from ziglang==0.15.2 python-zig version
compat_tools="$compat_root/.local/compat-musl-tools"
mkdir -p "$compat_tools"
cat > "$compat_tools/cc" <<'CC_WRAPPER'
#!/usr/bin/env bash
set -euo pipefail
compat_args=()
for compat_arg in "$@"; do
    # cc-rs detects clang and supplies Rust's four-component target spelling;
    # Zig uses its own three-component spelling, set explicitly below.
    [[ "$compat_arg" == --target=x86_64-unknown-linux-musl ]] && continue
    compat_args+=("$compat_arg")
done
exec "$SHADOWROOM_COMPAT_UVX" --from ziglang==0.15.2 python-zig cc -target x86_64-linux-musl "${compat_args[@]}"
CC_WRAPPER
cat > "$compat_tools/ar" <<'AR_WRAPPER'
#!/bin/sh
exec "$SHADOWROOM_COMPAT_UVX" --from ziglang==0.15.2 python-zig ar "$@"
AR_WRAPPER
cat > "$compat_tools/ld" <<'LD_WRAPPER'
#!/bin/sh
exec "$SHADOWROOM_COMPAT_LLD" -flavor gnu "$@"
LD_WRAPPER
chmod 755 "$compat_tools/cc" "$compat_tools/ar" "$compat_tools/ld"
export CC_x86_64_unknown_linux_musl="$compat_tools/cc"
export AR_x86_64_unknown_linux_musl="$compat_tools/ar"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$compat_tools/ld"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS='-C target-feature=+crt-static -C target-cpu=x86-64 -C linker-flavor=ld'
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
"$compat_cargo" build --locked --release --target x86_64-unknown-linux-musl \
    --manifest-path tools/compat-desktop/Cargo.toml "$@"
printf '%s\n' 'Build complete. Verify the resulting executable with file/readelf; older-kernel compatibility still needs VM/hardware testing.'
