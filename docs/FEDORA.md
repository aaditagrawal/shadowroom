# Fedora RPM and DNF distribution

Shadowroom's Fedora packaging follows the existing
[typsmthng-desktop COPR workflow](https://github.com/aaditagrawal/typsmthng-desktop/blob/main/docs/fedora-copr.md):
prepare a self-contained source RPM from one committed source revision, then
compile that source RPM without networking in Fedora. The RPM contains both
`shadowroom` and `shadowroom-remote`, desktop integration, controls, fonts, and
dependency/asset license notices. It uses Fedora's native shared libraries.

The initial target is Fedora 44 x86_64. Fedora 45 x86_64 is also an available
COPR chroot and should be validated before advertising support. These are native
Fedora RPMs, separate from the portable glibc 2.28 AppImage and tarball.

## Fedora repository

The [aaditagrawal/shadowroom COPR project](https://copr.fedorainfracloud.org/coprs/aaditagrawal/shadowroom/)
is configured for Fedora 44 x86_64 with binary-build networking disabled. Its
public Builds/Packages pages show the current package version and publication
status. GitHub release publication precedes the COPR compilation: a GitHub
release alone does not mean the Fedora repository build has finished.

After a successful COPR build, install and receive updates with:

```sh
sudo dnf copr enable aaditagrawal/shadowroom
sudo dnf install shadowroom
sudo dnf upgrade shadowroom
```

COPR builds/signs a third-party repository; this is not inclusion in Fedora's
official repositories. Fedora 44 x86_64 is the validated target. Do not assume a
Fedora 45 build is available merely because COPR offers that chroot.

## Build the source RPM

The preparation host needs Git, Cargo, a current stable Rust toolchain, uv,
`rpm-build`, GNU tar, and gzip. On Fedora:

```sh
sudo dnf install git rust cargo uv rpm-build tar gzip
bash scripts/rpm-build-srpm.sh --source-ref HEAD --output "$PWD/build/rpm-sources"
```

The selected revision must already contain the RPM scripts and template. The
script archives that committed revision; local edits and untracked files are
excluded. It refuses an existing output directory. Preparation may download
locked Cargo dependencies. The result contains the source RPM, source/vendor
archives, generated spec, and `SOURCE-PROVENANCE.json` with the source SHA,
lockfile digest, minimum Rust version, and dependency license inventory.

The checked-in `vendor/xattr` patch remains in the application source. Registry
crates use a separate `rpm-vendor` directory and relative Cargo source mapping.
Locked registry inputs use stable archive timestamps so unchanged build-script
inputs can reuse dependency caches across application commits. Source0 application
and path-dependency files retain their source commit timestamps; provenance still
records the selected revision.
The license inventory covers the resolved Linux build/development superset;
it does not claim every listed package is linked into the binaries.

## Rebuild and validate offline

Use a clean Fedora build environment rather than installing build dependencies
into your workstation:

```sh
mock -r fedora-44-x86_64 --rebuild build/rpm-sources/*.src.rpm
```

For CI in an already provisioned Fedora container, disable networking explicitly:

```sh
unshare --net rpmbuild --rebuild build/rpm-sources/*.src.rpm \
  --define "_topdir $PWD/build/rpm"
```

The spec declares the native build dependencies, uses `cargo --frozen` with a
fresh Cargo home, validates the desktop entry and both CLI entry points, and
checks executable build IDs. CI installs the main RPM in a clean, pinned Fedora
base with weak dependencies disabled and checks both CLI entry points. Include
the main `shadowroom-*.x86_64.rpm` and corresponding `.src.rpm` in the GitHub
release. This recipe disables debuginfo and debugsource packages.

For a downloaded, verified GitHub release RPM:

```sh
sudo dnf install ./shadowroom-0.1.3-1.fc44.x86_64.rpm
```

A directly downloaded RPM is installed locally; it does not configure a package
repository or provide automatic release updates. Match the package to its Fedora
target and verify the release checksum. GitHub release RPMs are not claimed to
have a Fedora/COPR signature.

## Release automation and credentials

The [Fedora COPR workflow](../.github/workflows/copr.yml) runs after Native checks
publish a verified version tag. It also accepts an owner's published-release
event or a manual **already-published tag** from the default branch. It reuses the
release's self-contained SRPM rather than rebuilding source preparation against
mutable dependency state. Only Fedora 44 x86_64 is submitted.

The repository uses:

| GitHub repository setting | Value |
| --- | --- |
| Variable `COPR_PROJECT` | `aaditagrawal/shadowroom` |
| Secret `COPR_CONFIG` | Complete COPR API INI configuration, supplied securely |

The project and repository settings are provisioned separately from the build
workflow. [COPR's setup documentation](https://docs.copr.fedorainfracloud.org/user_documentation.html#quick-start)
describes obtaining API configuration. Secrets belong in GitHub repository
secrets, never source files, logs, release assets, or chat. The publisher validates
the Fedora HTTPS endpoint/owner, writes a temporary mode-600 configuration, and
removes it immediately after submission. Pull requests never execute this job or
receive its credentials; implementation is checked out from the default branch.

Before submitting, [copr-publish.py](../scripts/copr-publish.py) verifies:

- A published numeric release tag resolves to a commit on the default branch,
  and Native checks ran against that exact tag/SHA. Both core jobs, Linux,
  macOS, Fedora, source packaging, and GitHub publication must have succeeded.
- The release SRPM matches both `SHA256SUMS` and GitHub's asset SHA-256 digest.
  Its package identity, embedded source commit, Cargo.lock, and vendor provenance
  match the released source. Archive contents are inspected without executing or
  extracting them into the project.
- The COPR project has the validated chroot and disabled build networking.
  Public build history contains no competing build for the same version/release
  and no unidentified active upload.

The publisher uploads the verified local SRPM bytes with no automatic POST retry,
records the build ID, and waits for that build. Existing running or successful
builds for the same EVR are reused; reused successful source archives must match
the release. A failed/cancelled build or conflicting EVR requires operator review
instead of silently creating a duplicate. A timeout does not cancel the build:
rerun **Fedora COPR** for that published tag to resume. Submission-response errors
can still mean server acceptance; inspect the project before retrying.

The sanitized publication receipt is retained as an Actions artifact; credentials
and downloaded source payloads are excluded. The isolated publishing toolchain
pins `copr-cli==2.6`, `copr==2.7`, `rpmfile==2.2.1`, and `zstandard==0.25.0`, using
uv. This follows the existing typsmthng release route while adding checksum,
source-provenance, and duplicate-submission checks.

## Runtime codecs

The RPM uses the system LibRaw and libheif when available, with weak package
recommendations. Vulkan drivers and a desktop-portal backend come from the
operating system. The RPM does not bundle libde265 or the AppImage's codec
payload. HEVC support depends on the system's decoder availability; installing
libheif alone does not promise an HEVC decoder. See [format support](MEDIA.md).

COPR's [software and license requirements](https://docs.copr.fedorainfracloud.org/user_documentation.html#what-i-can-build-in-copr)
apply separately from Fedora's optional packaging guidelines. Keep this native
system-codec build separate from the portable codec distribution when preparing
a public COPR submission.
