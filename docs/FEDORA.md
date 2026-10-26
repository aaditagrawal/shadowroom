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

## Current publication state

As checked on October 27, 2026, the existing
[aaditagrawal/typsmthng-desktop COPR project](https://copr.fedorainfracloud.org/coprs/aaditagrawal/typsmthng-desktop/)
publishes the RPM named `typsmthng` for Fedora 44 x86_64. Its successful
[build 11207495](https://copr.fedorainfracloud.org/coprs/build/11207495/)
contains version 0.1.11-1. Its GitHub release workflow prepares an immutable
source RPM and submits it after the release passes verification.

A Shadowroom COPR project has not been created. The Shadowroom GitHub repository
also has no `COPR_PROJECT` variable or `COPR_CONFIG` secret configured. The
typsmthng repository has both, but GitHub does not reveal stored secret values;
its encrypted `COPR_CONFIG` cannot be copied through the repository API. Obtain
or securely provision a COPR API configuration separately when publication is
authorized. Do not put it in source files, logs, release assets, or chat.

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
sudo dnf install ./shadowroom-0.1.1-1.fc44.x86_64.rpm
```

A directly downloaded RPM is installed locally; it does not configure a package
repository or provide automatic release updates. Match the package to its Fedora
target and verify the release checksum. GitHub release RPMs are not claimed to
have a Fedora/COPR signature.

## Configure COPR when publication is authorized

Follow [COPR's setup documentation](https://docs.copr.fedorainfracloud.org/user_documentation.html#quick-start)
to use the owner's Fedora account and API configuration. The following commands
describe future setup; preparation scripts do not execute them:

```sh
copr-cli create shadowroom --chroot fedora-44-x86_64
copr-cli build aaditagrawal/shadowroom build/rpm-sources/shadowroom-*.src.rpm
```

Create the project with networking disabled for binary RPM builds. Start with
the chroot validated by CI; enable additional chroots after testing. A future
release submission job can use:

| GitHub repository setting | Value |
| --- | --- |
| Variable `COPR_PROJECT` | `aaditagrawal/shadowroom` |
| Secret `COPR_CONFIG` | Complete COPR API INI configuration, supplied securely |

Submission should run only after verified publication of the exact same source
commit, and remain disabled when the configuration is absent. Write credentials
to a temporary mode-600 file, remove it after submission, and do not expose them
to pull-request jobs. COPR builds and signs its repository from submitted SRPMs;
this is a third-party repository rather than inclusion in Fedora's official repos.

After a successful future COPR build, installation and updates become:

```sh
sudo dnf copr enable aaditagrawal/shadowroom
sudo dnf install shadowroom
sudo dnf upgrade shadowroom
```

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
