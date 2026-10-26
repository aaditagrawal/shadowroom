# Local xattr 0.2.3 compatibility patch

This is the original crates.io xattr 0.2.3 source used by GPUI 0.2.2's dependency tree, with its original MIT and Apache 2.0 licenses. Shadowroom's `[patch.crates-io]` entry selects this copy without changing its public API or GPUI.

The only source change is in `src/sys/mod.rs`: the crate's internal `ENOATTR` constant resolves to `libc::ENODATA` on Linux and Android. macOS, FreeBSD and NetBSD continue using their real `libc::ENOATTR`. Unsupported-platform behavior is unchanged.

Linux and Android do not define ENOATTR in their system headers; the Rust libc crate previously provided a deprecated compatibility alias and removed it in current releases. The breakage and intended replacement are documented in [rust-lang/libc issue #1356](https://github.com/rust-lang/libc/issues/1356), completed by [libc pull request #5401](https://github.com/rust-lang/libc/pull/5401). Current [upstream xattr](https://github.com/Stebalien/xattr/blob/master/src/sys/linux_macos.rs) already distinguishes Linux's missing-attribute ENODATA from macOS's ENOATTR. Pinning an old libc globally would conflict with newer transitive dependencies' minimum versions.

Remove this patch after GPUI's dependency tree upgrades away from the affected xattr release. No registry cache or system library is modified.
