# Release process

Shadowroom uses the version in `Cargo.toml` for native package names. Prepare a
release candidate locally, review its README/screenshots/measurements, then publish
the approved revision. CI runs checks and retains package artifacts; it does not
publish a GitHub Release automatically.

## Validate the source

Run the checks in [CONTRIBUTING.md](../CONTRIBUTING.md), then the applicable native
smoke scripts. Test both the desktop and `shadowroom-remote` from the final package.
Verify import/copy and decision recovery against disposable sources. Check that
original SHA-256 hashes and modification times remain unchanged.

Inspect native formats with known reference fixtures and required codecs. A passing
core suite does not establish native codec fidelity, Wayland behavior, hardware
controller input, or macOS notarization. Record the systems actually tested.

## Prepare artifacts

1. Build packages using [BUILD.md](BUILD.md), retaining dependency notices and any
   optional codec sources/licenses/provenance.
2. Run the packaged binary's `--help` and the native smoke harness with `--binary`
   pointing at its launcher. For an AppImage, also run `native-appimage-smoke.py`.
3. Regenerate [performance data](PERFORMANCE.md) from the final release binary and
   capture current screenshots using `scripts/capture-screenshots.py` with demo
   photos cleared for redistribution. Update [image credits](images/README.md).
4. Generate SHA-256 checksums for the archives. Inspect archive contents for local
   profiles, machine addresses, private photographs, build logs, and experiments.
   `uv run --no-project python scripts/package-source.py` creates a source tarball
   and checksum from an explicit public-file list, excluding build/local artifacts.
5. On macOS, apply the intended distribution identity and notarization process
   before describing an artifact as signed/notarized for distribution.

## Publish after review

Commit only the source tree, documentation, benchmark JSON, and documentation
images. Keep `target/`, `.local/`, `dist/`, catalogs, and codec build work out of Git.
Retain `Cargo.lock`, `vendor/xattr`, and all attribution files.

After the repository owner approves the candidate, create the GitHub repository
if needed, push the reviewed source, and let CI validate that exact revision.
Create a version tag and GitHub Release with the native archives, their SHA-256
checksums, and release notes describing supported platforms and known limits.
Use actual successful CI/native results in those notes.
