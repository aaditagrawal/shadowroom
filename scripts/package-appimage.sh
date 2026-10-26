#!/usr/bin/env bash
# Assemble an existing verified Linux directory package; no downloads or builds.
set -euo pipefail
package_root=
runtime=
output=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --package) package_root=${2:?--package needs a directory}; shift 2 ;;
        --runtime) runtime=${2:?--runtime needs an AppImage runtime}; shift 2 ;;
        --output) output=${2:?--output needs a filename}; shift 2 ;;
        *) printf '%s\n' 'Usage: package-appimage.sh --package DIR --runtime FILE --output FILE.AppImage' >&2; exit 2 ;;
    esac
done
[[ -n "$package_root" && -n "$runtime" && -n "$output" ]] || { printf '%s\n' 'Package, runtime and output are required.' >&2; exit 2; }
package_root=$(realpath -- "$package_root")
runtime=$(realpath -- "$runtime")
[[ -x "$package_root/bin/shadowroom" && -x "$package_root/bin/shadowroom-remote" && -f "$runtime" ]] || { printf '%s\n' 'Built app, remote agent and AppImage runtime required.' >&2; exit 1; }
[[ ! -e "$output" ]] || { printf 'Output already exists: %s\n' "$output" >&2; exit 1; }
mkdir -p -- "$(dirname -- "$output")"
output=$(realpath -m -- "$output")
staging=$(mktemp -d "$(dirname -- "$output")/.appimage.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
appdir="$staging/AppDir"
mkdir -p "$appdir/usr"
cp -a -- "$package_root/." "$appdir/usr/"
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
mksquashfs "$appdir" "$staging/payload" -noappend -comp zstd -root-owned -no-progress -processors 4 > "$staging/squashfs.log"
cat -- "$runtime" "$staging/payload" > "$staging/app.AppImage"
chmod 755 "$staging/app.AppImage"
mv -n -- "$staging/app.AppImage" "$output"
[[ ! -e "$staging/app.AppImage" ]] || { printf '%s\n' 'Output appeared during assembly; no file replaced.' >&2; exit 1; }
sha256sum -- "$output"
