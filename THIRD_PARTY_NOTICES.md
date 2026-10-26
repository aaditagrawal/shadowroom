# Third-party notices

Shadowroom's original code is licensed under Apache License 2.0; see `LICENSE`.
The following independently licensed assets and dependencies retain their
original terms. Their inclusion does not imply endorsement by their authors.

## Fonts

The bundled font files are unmodified upstream releases distributed under the
[SIL Open Font License 1.1](https://openfontlicense.org/).

| Font | Copyright notice | Upstream |
| --- | --- | --- |
| DM Sans | Copyright 2014 The DM Sans Project Authors | https://github.com/googlefonts/dm-fonts |
| DM Mono | Copyright 2020 The DM Mono Project Authors | https://github.com/googlefonts/dm-mono |
| Geist Mono | Copyright 2024 The Geist Project Authors | https://github.com/vercel/geist-font |

Full copyright and license texts are retained in `assets/fonts/<family>/OFL.txt`
in the source tree and `licenses/fonts/<family>/OFL.txt` beside this notice in
distribution packages. Font files are bundled alongside those original licenses.
`assets/fonts/README.md` records download sources and `SHA256SUMS` records the
bundled originals. These fonts are not relicensed under Shadowroom's license.

## Codicons

Shadowroom uses SVG artwork from [Visual Studio Code Codicons](https://github.com/microsoft/vscode-codicons),
by Microsoft and contributors, under the
[Creative Commons Attribution 4.0 International Public License](https://creativecommons.org/licenses/by/4.0/).
The top-level Codicons SVG files retain upstream geometry. Shadowroom's
`assets/icons/shadowroom/trash-restore.svg` adapts Codicons `trash.svg`: it retains
the lid, handle and bin outline and replaces the two slats with an up arrow.
That adapted artwork remains under CC BY 4.0. The app also selects, scales and
colors icons when rendering. Other artwork identified as original in
`assets/icons/README.md` is under Shadowroom's Apache-2.0 license. No Microsoft
trademarks or endorsement are granted. The full Codicons license is
`assets/icons/LICENSE` in source and
`licenses/Codicons-CC-BY-4.0.txt` beside this notice in packages. The upstream
[legal notice](https://github.com/microsoft/vscode-codicons#legal-notices)
distinguishes its CC BY 4.0 artwork/content from its MIT-licensed program code;
Shadowroom bundles the SVG artwork. Packages retain the icon provenance table
and adapted SVG under `licenses/icons/`.

## Vendored xattr compatibility patch

`vendor/xattr` contains xattr 0.2.3, Copyright (c) 2015 Steven Allen, licensed
under MIT or Apache License 2.0. Shadowroom changes the missing-attribute errno
alias on Linux and Android from ENOATTR to ENODATA, preserving the other platform
implementations. Both original licenses and `SHADOWROOM-PATCH.md` accompany the
vendored source and are copied to `licenses/xattr/` in packages.

## Compiled dependencies

Rust dependencies, including GPUI, and their bundled native components retain
their respective upstream licenses. Packaging creates `licenses/dependencies/`
with available upstream LICENSE, LICENCE, COPYING, COPYRIGHT and NOTICE files,
including nested native-component notices, from the locally resolved Cargo
sources. `licenses/dependencies.json` records package versions, SPDX expressions,
upstream repositories and the collected file paths. It includes the resolved
build/development dependency superset as well as runtime dependencies, without
claiming every listed package is linked into the executable.

For crate archives that omit workspace-level licenses, supplemental texts are
retained in `assets/licenses/dependencies/` and copied with the package. Each
`SOURCES.json` records URLs, SHA-256 hashes and provenance, distinguishing exact
published commits from later upstream license texts or standard SPDX texts.
These preserve available upstream attribution without downloading anything
during packaging. SeaHash's archive omits its standalone copyright/license file
and its upstream rejects unauthenticated retrieval; its entry explicitly retains
the published MIT declaration and authors alongside the standard MIT text.
Some older Objective-C helper archives likewise omit standalone texts; their
entries preserve the published authors/license declarations and clearly identify
the accompanying standard MIT or Apache-2.0 text.

The inventory explicitly records packages without a separate notice file;
declared license expressions alone are not substituted for upstream copyright
notices. `Cargo.lock`, copied into packages, identifies the dependency versions.
System shared libraries remain supplied by the operating system; the package's
`shared-libraries.txt` records their dynamic loader references.
