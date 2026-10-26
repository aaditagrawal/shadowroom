#!/usr/bin/env bash
# Build a native directory package; optional HEVC decoder is scoped to its launcher.
set -euo pipefail

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output_root="$project_root/dist"
profile=release
skip_build=0
heif_runtime=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) skip_build=1; shift ;;
        --debug) profile=debug; shift ;;
        --output) output_root=${2:?--output requires a directory}; shift 2 ;;
        --heif-runtime) heif_runtime=${2:?--heif-runtime requires a prepared runtime directory}; shift 2 ;;
        -h|--help) printf '%s\n' 'Usage: scripts/package-linux.sh [--no-build] [--debug] [--output DIR] [--heif-runtime DIR]'; exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
[[ $(uname -s) == Linux ]] || { printf '%s\n' 'Run this script natively on Linux.' >&2; exit 1; }
if command -v cargo >/dev/null 2>&1; then cargo_bin=$(command -v cargo)
else cargo_bin="${HOME}/.cargo/bin/cargo"; fi
[[ -x "$cargo_bin" ]] || { printf '%s\n' 'A Rust toolchain is required.' >&2; exit 1; }
cd -- "$project_root"
if [[ $skip_build -eq 0 ]]; then
    if [[ $profile == release ]]; then
        LCMS2_STATIC=1 "$cargo_bin" build --locked --release --features desktop --bins
    else
        LCMS2_STATIC=1 "$cargo_bin" build --locked --features desktop --bins
    fi
fi
# Respect Cargo's configured target directory (including relative CARGO_TARGET_DIR).
metadata=$("$cargo_bin" metadata --format-version 1 --no-deps --locked)
target_root=$(printf '%s' "$metadata" | uv run --no-project python -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
binary="$target_root/$profile/shadowroom"
[[ -x "$binary" ]] || { printf 'Binary missing: %s\n' "$binary" >&2; exit 1; }
remote_binary="$target_root/$profile/shadowroom-remote"
[[ -x "$remote_binary" ]] || { printf 'Remote agent missing: %s\n' "$remote_binary" >&2; exit 1; }
version=$(printf '%s' "$metadata" | uv run --no-project python -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="shadowroom"))')
architecture=$(uname -m)
package="shadowroom-$version-linux-$architecture"
mkdir -p -- "$output_root"
output_root=$(cd -- "$output_root" && pwd)
[[ ! -e "$output_root/$package" && ! -e "$output_root/$package.tar.gz" ]] || { printf 'Output already exists under %s; choose a new --output directory.\n' "$output_root" >&2; exit 1; }
staging=$(mktemp -d "$output_root/.shadowroom-linux.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
root="$staging/$package"
mkdir -p -- "$root/bin" "$root/share/applications" "$root/share/icons/hicolor/scalable/apps" "$root/share/icons/hicolor/512x512/apps" "$root/share/doc/shadowroom" "$root/share/shadowroom"
install -m 755 "$binary" "$root/bin/shadowroom"
install -m 755 "$remote_binary" "$root/bin/shadowroom-remote"
native_binary="$root/bin/shadowroom"
native_remote="$root/bin/shadowroom-remote"
if [[ -n "$heif_runtime" ]]; then
    # Keep the complete supplied license/source/provenance payload alongside the
    # two decoder objects. The app never installs codecs into system directories.
    heif_runtime=$(cd -- "$heif_runtime" && pwd)
    for required in lib/libde265.so.0 provenance.json licenses/libde265/COPYING licenses/libheif/COPYING; do
        [[ -f "$heif_runtime/$required" ]] || { printf 'Missing HEIF runtime file: %s\n' "$required" >&2; exit 1; }
    done
    [[ -f "$heif_runtime/lib/libheif.so.1" || -f "$heif_runtime/plugins/libheif-libde265.so" ]] || { printf '%s\n' 'HEIF runtime needs bundled libheif or a compatible decoder plugin.' >&2; exit 1; }
    # Native upstream-source builds and the older prepared SourceRPM payloads
    # both retain corresponding sources and the instructions used to build them.
    for source_name in libde265 libheif; do
        [[ -d "$heif_runtime/sources" && -n $(find "$heif_runtime/sources" -maxdepth 1 \
            \( -name "$source_name-*.tar.gz" -o -name "$source_name-*.src.rpm" -o -name "$source_name-freeworld-*.src.rpm" \) -print -quit) ]] \
            || { printf 'Codec runtime requires corresponding %s source.\n' "$source_name" >&2; exit 1; }
    done
    if [[ -f "$heif_runtime/lib/libraw_r.so" ]]; then
        for required in licenses/libraw/LICENSE.LGPL licenses/libraw/LICENSE.CDDL; do
            [[ -f "$heif_runtime/$required" ]] || { printf 'Missing LibRaw license/source: %s\n' "$required" >&2; exit 1; }
        done
        [[ -n $(find "$heif_runtime/sources" -maxdepth 1 -name 'LibRaw-*.tar.gz' -print -quit) ]] || { printf '%s\n' 'LibRaw runtime requires its corresponding source archive.' >&2; exit 1; }
    fi
    mkdir -p "$root/lib" "$root/libexec"
    cp -a -- "$heif_runtime" "$root/lib/shadowroom-codecs"
    mv -- "$root/bin/shadowroom" "$root/libexec/shadowroom-native"
    mv -- "$root/bin/shadowroom-remote" "$root/libexec/shadowroom-remote-native"
    native_binary="$root/libexec/shadowroom-native"
    native_remote="$root/libexec/shadowroom-remote-native"
    cat > "$root/bin/shadowroom" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
package_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
codec_root="$package_root/lib/shadowroom-codecs"
if [[ -d "$codec_root/plugins" ]]; then
    export LIBHEIF_PLUGIN_PATH="$codec_root/plugins${LIBHEIF_PLUGIN_PATH:+:$LIBHEIF_PLUGIN_PATH}"
fi
export LD_LIBRARY_PATH="$codec_root/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$package_root/libexec/shadowroom-native" "$@"
LAUNCHER
    chmod 755 "$root/bin/shadowroom"
    sed 's@libexec/shadowroom-native@libexec/shadowroom-remote-native@' "$root/bin/shadowroom" > "$root/bin/shadowroom-remote"
    chmod 755 "$root/bin/shadowroom-remote"
fi
install -m 644 packaging/linux/app.shadowroom.Shadowroom.desktop "$root/share/applications/"
install -m 644 packaging/shadowroom.svg "$root/share/icons/hicolor/scalable/apps/app.shadowroom.Shadowroom.svg"
install -m 644 packaging/shadowroom-512.png "$root/share/icons/hicolor/512x512/apps/app.shadowroom.Shadowroom.png"
cp -R assets/controls assets/fonts "$root/share/shadowroom/"
cp -R docs "$root/share/doc/shadowroom/"
cp README.md CONTRIBUTING.md "$root/share/doc/shadowroom/"
uv run --no-project python scripts/package-notices.py "$root/share/doc/shadowroom" --cargo "$cargo_bin"
# Record loader dependencies for review instead of claiming a self-contained binary.
ldd "$native_binary" > "$root/share/doc/shadowroom/shared-libraries.txt"
ldd "$native_remote" > "$root/share/doc/shadowroom/remote-shared-libraries.txt"
if [[ -n "$heif_runtime" ]]; then
    for codec in "$root/lib/shadowroom-codecs/lib"/*.so.*; do
        [[ -f "$codec" && ! -L "$codec" ]] || continue
        dependencies="$root/share/doc/shadowroom/$(basename -- "$codec")-libraries.txt"
        LD_LIBRARY_PATH="$root/lib/shadowroom-codecs/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ldd "$codec" > "$dependencies"
        if grep -q 'not found' "$dependencies"; then
            cat "$dependencies" >&2; exit 1
        fi
    done
fi
if [[ -n "$heif_runtime" && -f "$root/lib/shadowroom-codecs/plugins/libheif-libde265.so" ]]; then
    LD_LIBRARY_PATH="$root/lib/shadowroom-codecs/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ldd "$root/lib/shadowroom-codecs/plugins/libheif-libde265.so" > "$root/share/doc/shadowroom/optional-hevc-libraries.txt"
    if grep -q 'not found' "$root/share/doc/shadowroom/optional-hevc-libraries.txt"; then
        cat "$root/share/doc/shadowroom/optional-hevc-libraries.txt" >&2; exit 1
    fi
fi
if grep -q 'not found' "$root/share/doc/shadowroom/shared-libraries.txt" "$root/share/doc/shadowroom/remote-shared-libraries.txt"; then
    cat "$root/share/doc/shadowroom/shared-libraries.txt" >&2
    printf '%s\n' 'Missing runtime libraries; package was not created.' >&2; exit 1
fi
cat > "$root/install.sh" <<'INSTALL'
#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
prefix=${1:-"${HOME}/.local"}
mkdir -p "$prefix/bin" "$prefix/share/applications" "$prefix/share/icons/hicolor/scalable/apps" "$prefix/share/icons/hicolor/512x512/apps" "$prefix/share/doc/shadowroom" "$prefix/share/shadowroom"
install -m 755 "$root/bin/shadowroom" "$prefix/bin/shadowroom"
install -m 755 "$root/bin/shadowroom-remote" "$prefix/bin/shadowroom-remote"
if [[ -d "$root/lib/shadowroom-codecs" ]]; then
    mkdir -p "$prefix/lib" "$prefix/libexec"
    cp -a "$root/lib/shadowroom-codecs" "$prefix/lib/"
    install -m 755 "$root/libexec/shadowroom-native" "$prefix/libexec/shadowroom-native"
    install -m 755 "$root/libexec/shadowroom-remote-native" "$prefix/libexec/shadowroom-remote-native"
fi
# Desktop paths can contain spaces; escape reserved Exec-string characters.
python3 - "$root/share/applications/app.shadowroom.Shadowroom.desktop" "$prefix/bin/shadowroom" "$prefix/share/applications/app.shadowroom.Shadowroom.desktop" <<'PY'
from pathlib import Path
import sys
text=Path(sys.argv[1]).read_text()
# Percent is a desktop field-code escape, and quotes/backslashes have two parse layers.
if any(c in sys.argv[2] for c in '=\n\r\t'):
    raise SystemExit('Install prefix must not contain =, newline, tab or carriage return')
encoded=[]
for char in sys.argv[2]:
    if char=='%': encoded.append('%%')
    elif char=='\\': encoded.append('\\'*4)
    elif char in '"$`': encoded.append('\\'*2+char)
    else: encoded.append(char)
exe=''.join(encoded)
text=text.replace('Exec=shadowroom', 'Exec="'+exe+'"')
Path(sys.argv[3]).write_text(text)
PY
cp "$root/share/icons/hicolor/scalable/apps/"* "$prefix/share/icons/hicolor/scalable/apps/"
cp "$root/share/icons/hicolor/512x512/apps/"* "$prefix/share/icons/hicolor/512x512/apps/"
cp -R "$root/share/doc/shadowroom/." "$prefix/share/doc/shadowroom/"
cp -R "$root/share/shadowroom/." "$prefix/share/shadowroom/"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" || true
printf 'Installed to %s. Launch Shadowroom from the app menu or run %s/bin/shadowroom.\n' "$prefix" "$prefix"
INSTALL
chmod 755 "$root/install.sh"
tar -czf "$staging/$package.tar.gz" -C "$staging" "$package"
mv -- "$root" "$output_root/$package"
mv -- "$staging/$package.tar.gz" "$output_root/$package.tar.gz"
printf 'Created %s\n' "$output_root/$package.tar.gz"
