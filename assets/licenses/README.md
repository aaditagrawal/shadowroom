# Supplemental dependency notices

These files supplement Cargo crate archives that omit their workspace-level
license texts. The native package scripts copy applicable entries into the
dependency notice directory. No dependency implementation is vendored here.

Each versioned directory's `SOURCES.json` records the full source URLs and
SHA-256 hashes. Published-commit files are preferred. Explicit notes identify
later upstream licenses, a sibling published commit, or standard SPDX text
instead of presenting those as copyright notices from the original archive.
SeaHash's available license declaration and author credit are retained in its
`DECLARATION.txt`. The original crate manifest's SPDX expression remains in the
generated package inventory.

The files are copied verbatim and retain their respective upstream terms. The
source references here are attribution/provenance, not runtime network requests;
packaging runs offline.
