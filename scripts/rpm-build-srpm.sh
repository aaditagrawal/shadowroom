#!/usr/bin/env bash
# Resolve/download Rust sources here; the resulting RPM builds fully offline.
set -euo pipefail

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output="$project_root/build/rpm-sources"
source_ref=HEAD
rpm_release=1
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output) output=${2:?--output needs a directory}; shift 2 ;;
        --source-ref) source_ref=${2:?--source-ref needs a commit or tag}; shift 2 ;;
        --release) rpm_release=${2:?--release needs a positive integer}; shift 2 ;;
        -h|--help) printf '%s\n' 'Usage: scripts/rpm-build-srpm.sh [--output DIR] [--source-ref REF] [--release N]'; exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
for tool in git uv rpmbuild tar gzip; do
    command -v "$tool" >/dev/null || { printf 'Missing source RPM tool: %s\n' "$tool" >&2; exit 1; }
done
if command -v cargo >/dev/null 2>&1; then cargo_bin=$(command -v cargo)
else cargo_bin="$HOME/.cargo/bin/cargo"; fi
[[ -x "$cargo_bin" ]] || { printf '%s\n' 'Cargo is required.' >&2; exit 1; }
[[ "$rpm_release" =~ ^[1-9][0-9]*$ ]] || { printf '%s\n' 'Release must be a positive integer.' >&2; exit 2; }
source_sha=$(git -C "$project_root" rev-parse --verify "$source_ref^{commit}")
source_epoch=$(git -C "$project_root" show -s --format=%ct "$source_sha")
[[ ! -e "$output" ]] || { printf 'Refusing existing output directory: %s\n' "$output" >&2; exit 1; }
mkdir -p -- "$output"
output=$(cd -- "$output" && pwd)
# Registry sources can exceed a gigabyte; stage on the output filesystem rather
# than assuming the user's /tmp quota can accommodate a complete Cargo vendor.
work=$(mktemp -d "$output/.work.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/source"
git -C "$project_root" archive "$source_sha" | tar -xf - -C "$work/source"
[[ -f "$work/source/packaging/rpm/shadowroom.spec.in" ]] || { printf '%s\n' 'The selected commit does not contain RPM packaging. Commit the packaging first.' >&2; exit 1; }
mkdir -p "$work/source/.cargo"
(
    cd -- "$work/source"
    "$cargo_bin" vendor --locked --versioned-dirs rpm-vendor > .cargo/config.toml
    "$cargo_bin" metadata --offline --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu --features desktop > "$work/metadata.json"
    uv run --no-project python scripts/package-notices.py "$work/source/rpm-notices" --cargo "$cargo_bin"
)
version=$(uv run --no-project python "$work/source/scripts/rpm-prepare.py" \
    --source "$work/source" --metadata "$work/metadata.json" --output "$work/generated" \
    --source-sha "$source_sha" --source-epoch "$source_epoch" --release "$rpm_release")
cp "$work/generated/SOURCE-PROVENANCE.json" "$work/source/rpm-notices/"
git -C "$project_root" archive --prefix="shadowroom-$version/" "$source_sha" | gzip -n > "$output/shadowroom-$version.tar.gz"
# Keep the checked-in vendor/xattr patch in Source0 and registry crates separate.
# Locked registry crates are immutable, including files watched by build scripts.
# Giving them each application commit's epoch reruns those scripts and rebuilds
# their dependents after an otherwise cacheable documentation-only commit.
# Only registry inputs get the fixed epoch; Source0/path sources retain ordinary
# source-commit invalidation and notices/provenance still identify this commit.
uv run --no-project python - "$work/source/rpm-vendor" <<'PY'
import os
import sys
from pathlib import Path

root = Path(sys.argv[1])
epoch_ns = 315532800 * 1_000_000_000  # 1980-01-01 UTC.
for path in (root, *root.rglob("*")):
    metadata = path.lstat()
    os.utime(path, ns=(metadata.st_atime_ns, epoch_ns), follow_symlinks=False)
PY
# Clamp generated config/notices to the commit epoch, retaining older immutable
# registry timestamps instead of overriding them with the application's epoch.
tar --sort=name --mtime="@$source_epoch" --clamp-mtime --owner=0 --group=0 --numeric-owner -czf "$output/shadowroom-$version-vendor.tar.gz" \
    -C "$work/source" rpm-vendor .cargo/config.toml rpm-notices
cp "$work/generated/shadowroom.spec" "$output/shadowroom.spec"
rpmbuild -bs "$output/shadowroom.spec" \
    --undefine dist \
    --define "_sourcedir $output" --define "_srcrpmdir $output" \
    --define "_builddir $work/rpmbuild"
cp "$work/generated/SOURCE-PROVENANCE.json" "$output/"
printf 'Created source RPM from %s in %s\n' "$source_sha" "$output"
