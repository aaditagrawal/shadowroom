# Bundled fonts

Downloaded as original, unmodified font files from the [Google Fonts repository](https://github.com/google/fonts) on 2026-10-17. Each family directory includes its upstream SIL Open Font License 1.1, including author/copyright notices. Redistribution and embedding are permitted under those licenses; retain the notices with redistributed app builds.

- [DM Sans upstream](https://github.com/google/fonts/tree/main/ofl/dmsans): `DMSans[opsz,wght].ttf`, family name **DM Sans**. Variable upright font, optical size and weight axes.
- [Geist Mono upstream](https://github.com/google/fonts/tree/main/ofl/geistmono): `GeistMono[wght].ttf`, family name **Geist Mono**. Variable upright weight axis.
- [DM Mono upstream](https://github.com/google/fonts/tree/main/ofl/dmmono): `DMMono-Regular.ttf` and `DMMono-Medium.ttf`, family name **DM Mono**.

`SHA256SUMS` records the bytes bundled in this checkout, including licenses. Load each `.ttf` through GPUI's font registration API at startup; the licenses do not need to be loaded as fonts.
