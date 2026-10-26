#!/usr/bin/env bash
# Called only by the successful tag workflow; no builds or arbitrary downloads.
set -euo pipefail
project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$project_root"
: "${RELEASE_TAG:?Release tag is required}"
uv run --no-project python scripts/release-metadata.py
notes="docs/releases/$RELEASE_TAG.md"
[[ -f "$notes" ]] || { printf 'Release notes missing: %s\n' "$notes" >&2; exit 1; }
for pattern in 'dist/*-linux-x86_64.tar.gz' 'dist/*-x86_64.AppImage' \
    'dist/*-macos-arm64.zip' 'dist/*-source.tar.gz'; do
    compgen -G "$pattern" >/dev/null || { printf 'Missing release artifact: %s\n' "$pattern" >&2; exit 1; }
done
(
    cd dist
    for checksum in *.sha256; do sha256sum --check "$checksum"; done
)
uv run --no-project python scripts/release-checksums.py dist
# Resume interrupted drafts, but never silently replace already public bytes.
if state=$(gh release view "$RELEASE_TAG" --json isDraft --jq .isDraft 2>/dev/null); then
    [[ "$state" == true ]] || { printf '%s\n' 'Release is already published; refusing to replace its assets.' >&2; exit 1; }
else
    gh release create "$RELEASE_TAG" --verify-tag --draft --prerelease \
        --title "Shadowroom $RELEASE_TAG" --notes-file "$notes"
fi
assets=()
for path in dist/*.tar.gz dist/*.zip dist/*.AppImage dist/*.rpm dist/*.sha256 dist/SHA256SUMS; do
    [[ ! -f "$path" ]] || assets+=("$path")
done
gh release upload "$RELEASE_TAG" "${assets[@]}" --clobber
gh release edit "$RELEASE_TAG" --draft=false --prerelease --notes-file "$notes"
