#!/usr/bin/env bash
# Assemble an existing verified Linux directory package; no downloads or builds.
set -euo pipefail
package_root=
runtime=
output=
runtime_sha256=
runtime_metadata=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --package) package_root=${2:?--package needs a directory}; shift 2 ;;
        --runtime) runtime=${2:?--runtime needs an AppImage runtime}; shift 2 ;;
        --output) output=${2:?--output needs a filename}; shift 2 ;;
        --runtime-sha256) runtime_sha256=${2:?--runtime-sha256 needs a digest}; shift 2 ;;
        --runtime-metadata) runtime_metadata=${2:?--runtime-metadata needs a directory}; shift 2 ;;
        *) printf '%s\n' 'Usage: package-appimage.sh --package DIR --runtime FILE --output FILE.AppImage [--runtime-sha256 SHA256] [--runtime-metadata DIR]' >&2; exit 2 ;;
    esac
done
[[ -n "$package_root" && -n "$runtime" && -n "$output" ]] || { printf '%s\n' 'Package, runtime and output are required.' >&2; exit 2; }
package_root=$(realpath -- "$package_root")
runtime=$(realpath -- "$runtime")
[[ -x "$package_root/bin/shadowroom" && -x "$package_root/bin/shadowroom-remote" && -f "$runtime" ]] || { printf '%s\n' 'Built app, remote agent and AppImage runtime required.' >&2; exit 1; }
if [[ -n "$runtime_sha256" ]]; then
    printf '%s  %s\n' "$runtime_sha256" "$runtime" | sha256sum --check --status
fi
[[ ! -e "$output" ]] || { printf 'Output already exists: %s\n' "$output" >&2; exit 1; }
mkdir -p -- "$(dirname -- "$output")"
output=$(realpath -m -- "$output")
staging=$(mktemp -d "$(dirname -- "$output")/.appimage.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
appdir="$staging/AppDir"
mkdir -p "$appdir/usr"
cp -a -- "$package_root/." "$appdir/usr/"
if [[ -n "$runtime_metadata" ]]; then
    notices="$appdir/usr/share/doc/shadowroom/appimage-runtime"
    mkdir -p "$notices"
    for name in appimage-runtime-LICENSE appimage-runtime-source.tar.gz release-inputs.json; do
        install -m 644 "$runtime_metadata/$name" "$notices/$name"
    done
fi
desktop=app.shadowroom.Shadowroom.desktop
cp -- "$package_root/share/applications/$desktop" "$appdir/$desktop"
cp -- "$package_root/share/icons/hicolor/scalable/apps/app.shadowroom.Shadowroom.svg" "$appdir/app.shadowroom.Shadowroom.svg"
ln -s app.shadowroom.Shadowroom.svg "$appdir/.DirIcon"
cat > "$appdir/AppRun" <<'APP_RUN'
#!/usr/bin/env bash
set -euo pipefail
here=${APPDIR:-$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)}
exec "$here/usr/bin/shadowroom" "$@"
APP_RUN
chmod 755 "$appdir/AppRun"
time_options=()
if [[ -n "${SOURCE_DATE_EPOCH:-}" ]]; then
    time_options=(-mkfs-time "$SOURCE_DATE_EPOCH" -all-time "$SOURCE_DATE_EPOCH")
fi
# Older squashfs-tools reject an epoch in both the environment and flags.
# Pass it explicitly to set every inode time, then remove only the redundant env.
env -u SOURCE_DATE_EPOCH mksquashfs "$appdir" "$staging/payload" -noappend -comp zstd -root-owned -no-progress \
    -processors "${CARGO_BUILD_JOBS:-2}" "${time_options[@]}" > "$staging/squashfs.log"
cat -- "$runtime" "$staging/payload" > "$staging/app.AppImage"
chmod 755 "$staging/app.AppImage"
mv -n -- "$staging/app.AppImage" "$output"
[[ ! -e "$staging/app.AppImage" ]] || { printf '%s\n' 'Output appeared during assembly; no file replaced.' >&2; exit 1; }
sha256sum -- "$output"
