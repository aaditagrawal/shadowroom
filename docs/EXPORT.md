# Export behavior and verification

Original media and existing source sidecars are opened read-only. Export destinations must be outside the selected photographs' source directories and every registered imported root, including paths resolved through existing symlinks. Preparing an `ExportPlan` snapshots decisions and source paths using catalog metadata; destination filesystem checks and copying run only when the background export worker executes it.

## Copy picked groups

`export_picks` exports all photographs flagged Pick in the captured catalog state. The `ids` argument of `prepare_export` selects generated XMP exports; it does not limit picks exports. A picked RAW with a JPEG companion is one export group.

Each group includes its primary photograph, its registered companion, and existing adjacent sidecars matching either the photograph stem (`capture.xmp`, `capture.AAE`) or full filename (`capture.nef.xmp`, `capture.jpg.xmp`). XMP/AAE extension matching is case-insensitive. Directory sidecar listings are cached during an export rather than rescanning a large source directory for every photo. Shared stem sidecars associated with both RAW and JPEG are copied once.

The worker reserves **every** target group member using exclusive `create_new` opens before copying. If any target already exists, it removes only its own tentative reservations and retries with a common suffix. For example, if `capture.jpg` already exists at the destination, a RAW/JPEG pair plus sidecars becomes:

```text
capture-1.nef
capture-1.jpg
capture-1.xmp
capture-1.nef.xmp
capture-1.jpg.xmp
capture-1.AAE
```

A collision on any one sidecar also renames the whole group. Existing files, directories and symlinks are never overwritten. Repeated exports create fresh suffixes and fresh manifests. Unusual input groups with duplicate case-insensitive extensions/sidecar suffixes receive `.alternate-N` disambiguators, with exact source mappings in the manifest.

The group reservation is coordinated through exclusive file creation and rollback; a filesystem does not provide a single atomic operation covering multiple filenames. Other processes can observe empty reservations or a group in progress. The completed manifest identifies groups that finished verification.

## Verification and failure handling

Each member is copied with a fixed 64 KiB buffer while computing a BLAKE3 digest. Reads are limited to the initial source size plus one detection byte, so a continually growing source cannot prolong the copy indefinitely. Growth fails before excess bytes reach the output. The output is synced, then reopened for a bounded byte-count and digest comparison. Open-handle and current-path metadata must still match the initial identity, size and modification time; Unix also checks change time. Source and verification opens reject terminal symlinks and special files on Unix. These checks detect ordinary concurrent changes; they are not an atomic filesystem snapshot or a sandbox against ancestor-directory races. The source is read once; verification adds a destination read, without decoding or modifying the photograph.

If a source/companion/sidecar cannot be read, writing or syncing fails, or verification fails, the worker removes all newly reserved outputs from that group. Files verified earlier in the failed group are marked rolled back. Completed independent groups remain. Cleanup verifies the reserved file identity before deletion on Unix, so a path replaced by another process is left untouched and reported as a cleanup failure. Cleanup failures are explicit in the report.

Every operation creates a uniquely named `shadowroom-picks-manifest.json` (then `-1`, `-2`, and so on). It starts with a small `running` record, then is atomically replaced with the final report. The final schema includes completed file/group counts, failed group count, and each group's original path, chosen basename, status and error. Each file records its source, destination, role, outcome, copied bytes, BLAKE3 hash, whether output remains, and any failure. The returned count includes verified media and existing sidecar files, excluding the manifest.

When some groups fail, the API returns an error containing the completed verified-file count and manifest path. Read that manifest to distinguish completed groups, rolled-back files, unattempted members and cleanup failures. The worker continues independent groups to provide a complete partial-success report. A process interruption can leave a `running` manifest and incomplete reserved outputs; those are not certified complete, and a retry creates new filenames. If final manifest persistence itself fails, the error identifies its path and the completed-file count.

Rust callers may downcast the returned `anyhow::Error` to `catalog::ExportFailure` to inspect `completed`, `manifest_path` and `message` without parsing human-readable text. Existing export APIs continue returning `Result<usize>`. Tracked background jobs report this partial completion count in their reliable failure event.

## Generate decision XMP

XMP export writes new decision sidecars to a separate destination; it does not replace existing source editing sidecars. Existing destination names receive unique suffixes. The uniquely named `shadowroom-xmp-manifest.json` remains an array mapping each source photograph and catalog ID to the actual generated sidecar, outcome and checksum. This mapping is necessary when different source folders contain the same basename.

Each sidecar records `xmp:Rating`, `xmp:Label`, `shadowroom:Flag` and `shadowroom:PriorRating`. Rejected photographs use the conventional `xmp:Rating="-1"`, while `shadowroom:PriorRating` preserves their original zero-to-five stars. Consumers that understand only standard XMP may ignore the custom Shadowroom fields; the exported data still retains both flag and star decisions. Generated files are synced and read back for verification. Failures leave completed sidecars and record failed entries in the manifest, and the API reports the completed count plus manifest path.

## Tests

Catalog tests exercise a real RAW/JPEG/Adobe-XMP/darktable-XMP/AAE group with a companion filename collision, verify coordinated suffixes and every manifest checksum, repeat the export without overwriting, detect destination byte corruption, roll back a primary copied before a missing companion fails, retain a successful independent group, preserve unrelated destination/source bytes, map flattened XMP collisions, and preserve rejected photographs' previous star ratings.
