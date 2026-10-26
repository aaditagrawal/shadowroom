#!/usr/bin/env bash
# Build verified upstream archives inside packaging/linux/Containerfile.
set -euo pipefail
[[ $# == 3 ]] || { printf '%s\n' 'Usage: build-release-codecs.sh INPUTS WORK OUTPUT' >&2; exit 2; }
inputs=$(realpath -- "$1")
work=$(realpath -m -- "$2")
output=$(realpath -m -- "$3")
[[ ! -e "$work" && ! -e "$output" ]] || { printf '%s\n' 'Codec work/output directories must be new.' >&2; exit 1; }
mkdir -p "$work" "$output/lib" "$output/sources" "$output/licenses"
project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# Verify again at the build boundary, including when the inputs came from a cache.
python3 - "$inputs" <<'PY'
import hashlib, json, pathlib, sys
root = pathlib.Path(sys.argv[1])
for item in json.loads((root / 'release-inputs.json').read_text())['inputs']:
    if item['kind'] == 'codecs':
        with (root / item['name']).open('rb') as source:
            actual = hashlib.sha256(source.read()).hexdigest()
        if actual != item['sha256']:
            raise SystemExit('Codec source checksum mismatch: ' + item['name'])
PY
for archive in LibRaw-0.22.2.tar.gz libde265-1.0.18.tar.gz libheif-1.23.5.tar.gz; do
    tar -xzf "$inputs/$archive" -C "$work"
    cp "$inputs/$archive" "$output/sources/"
done
stage="$work/stage"
jobs=${CARGO_BUILD_JOBS:-2}
# Preserve the oldest available ABI for LibRaw; only the thread-safe shared object.
mkdir "$work/raw-build"
(
    cd "$work/raw-build"
    "$work/LibRaw-0.22.2/configure" --prefix="$stage" --libdir="$stage/lib" \
        --enable-shared --disable-static --disable-examples --disable-openmp \
        --disable-jpeg --disable-lcms --enable-zlib \
        CFLAGS='-O2 -g0 -fPIC -march=x86-64 -mtune=generic' \
        CXXFLAGS='-O2 -g0 -fPIC -march=x86-64 -mtune=generic'
    make -j"$jobs" lib/libraw_r.la
    make lib_LTLIBRARIES=lib/libraw_r.la install-libLTLIBRARIES
)
# libheif needs a newer C++ compiler, while the resulting shared objects must
# still pass the EL8 loader check performed by the release workflow.
set +u
source /opt/rh/gcc-toolset-15/enable
set -u
cmake -S "$work/libde265-1.0.18" -B "$work/de265-build" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$stage" -DCMAKE_INSTALL_LIBDIR=lib \
    -DENABLE_SDL=OFF -DENABLE_DECODER=OFF -DENABLE_ENCODER=OFF -DBUILD_SHARED_LIBS=ON
cmake --build "$work/de265-build" --parallel "$jobs"
cmake --install "$work/de265-build"
PKG_CONFIG_PATH="$stage/lib/pkgconfig" cmake -S "$work/libheif-1.23.5" -B "$work/heif-build" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$stage" -DCMAKE_INSTALL_LIBDIR=lib \
    -DCMAKE_PREFIX_PATH="$stage" -DBUILD_SHARED_LIBS=ON \
    -DENABLE_PLUGIN_LOADING=OFF -DWITH_LIBDE265=ON -DWITH_LIBDE265_PLUGIN=OFF \
    -DWITH_UNCOMPRESSED_CODEC=ON -DWITH_HEADER_COMPRESSION=OFF -DCMAKE_DISABLE_FIND_PACKAGE_Brotli=TRUE \
    -DWITH_EXAMPLES=OFF -DWITH_GDK_PIXBUF=OFF -DBUILD_TESTING=OFF -DWITH_FUZZERS=OFF \
    -DBUILD_DOCUMENTATION=OFF -DBUILD_DEVELOPMENT_TOOLS=OFF \
    -DWITH_X265=OFF -DWITH_AOM_ENCODER=OFF -DWITH_AOM_DECODER=OFF -DWITH_DAV1D=OFF \
    -DWITH_RAV1E=OFF -DWITH_SvtEnc=OFF -DWITH_JPEG_DECODER=OFF -DWITH_JPEG_ENCODER=OFF \
    -DWITH_OpenJPEG_DECODER=OFF -DWITH_OpenJPEG_ENCODER=OFF -DWITH_FFMPEG_DECODER=OFF \
    -DWITH_KVAZAAR=OFF -DWITH_OPENJPH_ENCODER=OFF -DWITH_VVDEC=OFF -DWITH_VVENC=OFF \
    -DWITH_UVG266=OFF -DWITH_X264=OFF -DWITH_OpenH264_DECODER=OFF \
    -DWITH_LIBSHARPYUV=OFF -DWITH_WEBCODECS=OFF \
    -DCMAKE_INSTALL_RPATH='$ORIGIN' -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON
cmake --build "$work/heif-build" --parallel "$jobs"
cmake --install "$work/heif-build"
cp -a "$stage/lib/"*.so* "$output/lib/"
for spec in 'LibRaw-0.22.2 libraw' 'libde265-1.0.18 libde265' 'libheif-1.23.5 libheif'; do
    read -r directory library <<< "$spec"
    mkdir "$output/licenses/$library"
    find "$work/$directory" -maxdepth 1 -type f \
        \( -name 'LICENSE*' -o -name 'COPYING*' -o -name 'COPYRIGHT*' -o -name 'AUTHORS*' -o -name '*NOTICE*' \) \
        -exec cp -t "$output/licenses/$library" {} +
done
cp "$project_root/scripts/build-release-codecs.sh" "$output/sources/"
cp "$project_root/packaging/linux/Containerfile" "$output/sources/Containerfile"
cp "$inputs/release-inputs.json" "$output/sources/"
cp /release-build-rpms.txt "$output/sources/build-rpms.txt"
python3 - "$output" <<'PY'
import hashlib, json, pathlib, platform, sys
root = pathlib.Path(sys.argv[1])
records = {}
for path in sorted(root.rglob('*')):
    if path.is_file() and not path.is_symlink():
        records[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
(root / 'provenance.json').write_text(json.dumps({
    'schema': 1, 'libraries': {'libraw': '0.22.2', 'libheif': '1.23.5', 'libde265': '1.0.18'},
    'source_modifications': 'none', 'build': platform.platform(),
    'features': {'raw_sensor_fallback': True, 'hevc_decoder': 'libde265', 'uncompressed_heif': True,
                 'external_heif_plugins': False, 'external_heif_encoders': False,
                 'libraw_jpeg_lossy_dng': False, 'libraw_optional_camera_sdks': False},
    'rebuild': 'sources/build-release-codecs.sh with sources/Containerfile and verified source archives',
    'sha256': records,
}, indent=2) + '\n')
PY
