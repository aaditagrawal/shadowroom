#!/usr/bin/env bash
# Native .app bundle with an ad-hoc signature by default; no notarization claim.
set -euo pipefail

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output_root="$project_root/dist"
profile=release
skip_build=0
runtime_shaders=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) skip_build=1; shift ;;
        --debug) profile=debug; shift ;;
        --runtime-shaders) runtime_shaders=1; shift ;;
        --output) output_root=${2:?--output requires a directory}; shift 2 ;;
        -h|--help) printf '%s\n' 'Usage: scripts/package-macos.sh [--no-build] [--debug] [--runtime-shaders] [--output DIR]'; exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
[[ $(uname -s) == Darwin ]] || { printf '%s\n' 'Run this script natively on macOS.' >&2; exit 1; }
if command -v cargo >/dev/null 2>&1; then cargo_bin=$(command -v cargo)
else cargo_bin="${HOME}/.cargo/bin/cargo"; fi
[[ -x "$cargo_bin" ]] || { printf '%s\n' 'A Rust toolchain is required.' >&2; exit 1; }
if [[ $skip_build -eq 0 && $runtime_shaders -eq 0 ]]; then
    for tool in metal metallib; do
        xcrun --sdk macosx --find "$tool" >/dev/null 2>&1 || {
            printf '%s\n' "GPUI 0.2.2 requires Xcode with the Metal compiler ($tool missing)." 'Select a complete Xcode installation using DEVELOPER_DIR; Command Line Tools alone are insufficient.' >&2; exit 1;
        }
    done
fi
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-12.0}
build_features=desktop
if [[ $runtime_shaders -eq 1 ]]; then build_features=desktop,runtime-shaders; fi
cd -- "$project_root"
if [[ $skip_build -eq 0 ]]; then
    if [[ $profile == release ]]; then
        LCMS2_STATIC=1 "$cargo_bin" build --locked --release --features "$build_features" --bin shadowroom
    else
        LCMS2_STATIC=1 "$cargo_bin" build --locked --features "$build_features" --bin shadowroom
    fi
fi
metadata=$("$cargo_bin" metadata --format-version 1 --no-deps --locked)
target_root=$(printf '%s' "$metadata" | uv run --no-project python -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
binary="$target_root/$profile/shadowroom"
[[ -x "$binary" ]] || { printf 'Binary missing: %s\n' "$binary" >&2; exit 1; }
version=$(printf '%s' "$metadata" | uv run --no-project python -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="shadowroom"))')
architecture=$(uname -m)
package="Shadowroom-$version-macos-$architecture"
mkdir -p -- "$output_root"
output_root=$(cd -- "$output_root" && pwd)
[[ ! -e "$output_root/$package" && ! -e "$output_root/$package.zip" ]] || { printf 'Output already exists under %s; choose a new --output directory.\n' "$output_root" >&2; exit 1; }
staging=$(mktemp -d "$output_root/.shadowroom-macos.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
bundle="$staging/$package/Shadowroom.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$staging/Shadowroom.iconset"
install -m 755 "$binary" "$bundle/Contents/MacOS/shadowroom"
cp packaging/macos/Info.plist "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :LSMinimumSystemVersion $MACOSX_DEPLOYMENT_TARGET" "$bundle/Contents/Info.plist"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" packaging/shadowroom.png --out "$staging/Shadowroom.iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size*2))
    sips -z "$double" "$double" packaging/shadowroom.png --out "$staging/Shadowroom.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$staging/Shadowroom.iconset" -o "$bundle/Contents/Resources/Shadowroom.icns"
cp -R assets/fonts assets/controls docs "$bundle/Contents/Resources/"
cp README.md CONTRIBUTING.md "$bundle/Contents/Resources/"
uv run --no-project python scripts/package-notices.py "$bundle/Contents/Resources" --cargo "$cargo_bin" --features "$build_features"
# Reject Homebrew/local dynamic paths: such a bundle would fail on another Mac.
otool -L "$bundle/Contents/MacOS/shadowroom" > "$bundle/Contents/Resources/shared-libraries.txt"
if tail -n +2 "$bundle/Contents/Resources/shared-libraries.txt" | grep -E '/opt/homebrew/|/usr/local/|/Users/'; then
    printf '%s\n' 'Non-system dynamic dependency detected. Rebuild with LCMS2_STATIC=1 or bundle dependencies explicitly.' >&2; exit 1
fi
plutil -lint "$bundle/Contents/Info.plist"
identity=${SHADOWROOM_CODESIGN_IDENTITY:--}
codesign --force --sign "$identity" --timestamp=none "$bundle"
codesign --verify --deep --strict "$bundle"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$staging/$package.zip"
mv "$staging/$package" "$output_root/$package"
mv "$staging/$package.zip" "$output_root/$package.zip"
printf 'Created %s\n' "$output_root/$package.zip"
