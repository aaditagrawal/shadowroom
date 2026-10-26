#!/usr/bin/env bash
# Build the committed, vendored SRPM offline, then verify local RPM installation.
set -euo pipefail
project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$project_root"
engine=${CONTAINER_ENGINE:-docker}
image=${SHADOWROOM_RPM_BUILD_IMAGE:-shadowroom-release-fedora44:ci}
runtime_image=$(awk '$1 == "FROM" { print $2; exit }' packaging/rpm/Containerfile)
[[ $runtime_image == *@sha256:* ]] || { printf '%s\n' 'Fedora runtime base must be pinned by digest.' >&2; exit 1; }
engine_options=()
if [[ $(basename "$engine") == podman ]]; then engine_options+=(--userns=keep-id); fi
rust_sysroot=$(rustc --print sysroot)
mkdir -p .local/ci-rpm/target .local/ci-rpm/home dist
"$engine" run --rm --network=none "${engine_options[@]}" --security-opt label=disable \
    --user "$(id -u):$(id -g)" -v "$project_root/.local/ci-rpm:/output" \
    -v "$rust_sysroot:/opt/rust:ro" \
    -e HOME=/output/home -e SHADOWROOM_RPM_TARGET_DIR=/output/target \
    -e PATH=/opt/rust/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    -e CARGO_INCREMENTAL=0 "$image" bash -euo pipefail -c '
        rpmbuild --rebuild /output/sources/*.src.rpm \
            --define "_topdir /output/rpm" --define "_smp_build_ncpus 2"
    '
cp .local/ci-rpm/sources/*.src.rpm dist/
for package in .local/ci-rpm/rpm/RPMS/x86_64/shadowroom-[0-9]*.x86_64.rpm; do
    [[ -f "$package" ]] || { printf '%s\n' 'Main RPM was not built.' >&2; exit 1; }
    cp "$package" dist/
done
# DNF resolves runtime dependencies in the clean, pinned Fedora base so installed
# build dependencies cannot mask missing Requires. This does not use COPR.
"$engine" run --rm --security-opt label=disable -v "$project_root/dist:/artifacts:ro" \
    "$runtime_image" bash -euo pipefail -c '
        dnf -y --setopt=install_weak_deps=False install /artifacts/shadowroom-[0-9]*.x86_64.rpm
        shadowroom --help
        shadowroom-remote --version
        rpm -q --requires shadowroom
    '
uv run --no-project python scripts/release-checksums.py dist
