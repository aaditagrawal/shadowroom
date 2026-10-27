# Trash, deletion, and exact duplicates

**Delete** defaults to the operating system's Trash. Change **Preferences →
Culling → Trash destination** to **In-app** for the original restorable catalog
behavior. Applying this preference does not move existing files or convert
previous in-app Trash decisions. Ratings, flags, labels, and explicit XMP export
continue to use the local catalog and recovery model; changing Trash mode does
not enable automatic metadata writes.

| Action | Effect | Recovery |
| --- | --- | --- |
| Delete with OS Trash | Move confirmed local photo files out of their source folders to system Trash | Restore with the OS file manager |
| Delete with In-app | Hide photo groups in the catalog; originals, companions, and sidecars stay in place | Restore from Shadowroom's Trash or decision Undo |
| Shift+Delete | Permanently remove the confirmed local files, bypassing either Trash mode | No Shadowroom Undo or Trash recovery |

## Confirmation and file scope

Delete/Backspace, Shift+Delete, and the image context menu first open a
confirmation; they do not immediately change files. The confirmation captures the
selection and displays the photo count, physical-file count, source locations,
and destination/recovery route. **Cancel is selected by default. Enter cancels;
Tab then Enter confirms. Escape also cancels.**

Physical operations include the original, a paired JPEG/HEIF companion, and
recognized adjacent sidecars. Sidecar matching is case-insensitive and accepts
the media stem or full filename followed by `.xmp`, `.aae`, `.thm`, `.wav`,
`.mp3`, `.log`, or `.lrv`. For example, `IMG_1234.CR2`, `IMG_1234.JPG`,
`IMG_1234.xmp`, and `IMG_1234.CR2.xmp` are included in one plan. Shared paths are
counted once. Unrelated neighboring files and directories are excluded. In-app
Trash only counts the catalog's media members because it does not discover or
move sidecars.

A stem-based sidecar shared with same-stem media outside the explicit selection
is retained. For example, deleting only `IMG_1234.CR2` leaves `IMG_1234.xmp` when
an unselected `IMG_1234.JPG` still uses that stem; `IMG_1234.CR2.xmp` remains
specific to the selected RAW. The confirmation lists retained shared sidecars
separately from files that will move or be deleted.

The plan captures file and parent-folder identities, checks the entire plan
before changing files, then checks each member again before its operation. A
changed or missing file cancels the operation instead of silently deleting a
replacement. Symbolic-link photo/sidecar entries are refused. Source-folder
aliases are resolved before confirmation.

OS Trash availability depends on the platform and drive. Linux uses GIO (GLib)
and reports an error if that support is unavailable. **Failure never falls back to permanent
deletion.** Physical file operations run sequentially, with sidecars and
companions before originals, and stop at the first failure. Earlier operations
may already have succeeded: the UI reports completed/failed counts and errors,
while `workspace_state.deletion.report` records completed, failed, and
not-attempted files. This is not an atomic filesystem transaction. Restore any
already-trashed files through the OS file manager; permanent deletion cannot be
rolled back.

After restoring files from OS Trash, refresh or reopen their folder to index
them again. Restoring the files does not restore catalog decisions removed with
their original photo record. Export decisions explicitly before deletion if you
need a portable copy of that work; in-app Trash retains those decisions in place.

The catalog reconciles only files actually removed. A catalog reconciliation
failure remains visible and calls for a folder refresh; it does not undo completed
filesystem work. Physical deletion clears decision Undo history after files
change, preventing an old snapshot from appearing to restore absent files. The
app waits for an executing operation before quitting.

Physical deletion is local-only. Remote photos cannot be deleted through these
actions. Indexing, copy imports, exports, recovery, unsaved decisions, or an
active duplicate scan can require finishing first. MCP's `trash` tool remains
explicitly **app-only**, regardless of the UI destination preference; there is
no MCP command to confirm or execute physical deletion. Finish confirmations in
the app.

## Durable in-app Trash decisions

`Photo`, `PhotoDecision`, and workspace history `Decision` carry `trashed: bool`. Old serialized decisions default it to false. Opening an older catalog adds an independent SQLite `trashed` column with a false default; metadata blobs and IDs are retained. Metadata reindexing and standalone-JPEG to RAW+companion promotion do not overwrite the Trash column.

In-app Trash changes use the existing full-decision batch, revision acknowledgement, retry, undo/redo, and recovery journal path. Accepted changes can be pending until their SQLite transaction commits; an acceptance response does not claim durability. Restoring keeps the previous rating, pick/reject flag, and color label. The Trash filter includes only in-app trashed photographs; it is not a browser for the OS Trash. All, Picks, Unreviewed, and Rejected exclude in-app trashed photographs. Grouping input and new picks/XMP export plans exclude them. Already accepted exports use their captured input snapshot.

The complete `library_list` scope `all` is an inspection API and includes all catalog rows, including Trash. Its default visible scope follows the current filter. `photo_get` can inspect a trashed ID directly.

## Explicit exact scan

Scanning starts only after a user or agent requests it. It does not run automatically during drive discovery, import, or ordinary culling, and it never automatically trashes anything. Omitting the ID selection scans nontrashed catalog assets; explicit IDs constrain the snapshot. Only one scan can be active; another request must wait or cancel the first.

The worker:

1. Reads current filesystem metadata and partitions assets by source byte lengths. Unique lengths require no source-content reads.
2. Samples at most three 16 KiB regions per file, from its beginning, middle, and end. Samples only eliminate candidates; a sample match is not a duplicate result.
3. Streams candidate files through BLAKE3 with one reusable 1 MiB buffer. Matching full digests identify byte-exact media matches. A RAW+JPEG/HEIF asset must match both its primary and companion bytes; it is not compared as a standalone companion image.
4. Rechecks source fingerprints after reading and before returning groups. A final independent catalog query also verifies current ID, primary path, companion path, and Trash state. An ID promoted to a different logical asset during scanning is omitted rather than mislabeled by a stale snapshot.

Different RAW/JPEG/HEIF representations of one exposure are not exact duplicates merely because they are paired or visually alike. Ratings, flags, labels, XMP, and AAE sidecars are not media-byte identity inputs. Review the matches before choosing a Trash action. The desktop uses the configured destination and confirms it; the MCP `trash` tool always leaves originals and sidecars in place.

Results are deterministic, ordered by smallest ID, with IDs sorted inside each group. Output is capped at 1,000 groups and 10,000 total IDs. A whole group exceeding the remaining limit is omitted and `truncated` becomes true. Errors retain at most 64 messages, with an `errors_omitted` counter. A completed scan can have source errors or omitted changed assets; inspect those fields rather than interpreting completion as proof every source was readable. The desktop clears prior result IDs and group lookup as soon as another scan is admitted, including when that rescan is later cancelled. It captures primary/companion path, indexed mtime, and size at admission; a changed completed result becomes `stale` and clears the duplicate collection. Reimporting a RAW that promotes an already-scanned JPEG ID therefore cannot leave its former standalone duplicate set visible.

## Cache, progress, and shutdown

Full and sample digests live in the app's local SQLite `exact_hashes` table, never next to originals. Cache keys use the source path and current fingerprint: byte length and nanosecond modification time, plus device/inode/change-time on Unix. File creation time supplements the fingerprint on other platforms. A warm scan still checks filesystem metadata but can read zero source-content bytes. `cache_hits` counts reused full-file hashes, so a paired asset can contribute two hits.

The scan opens a separate local SQLite connection. No catalog mutex or SQLite transaction spans source reads. Cache writes use brief batches of up to 32 records; lock contention can fail the rebuildable scan instead of holding a long transaction ahead of decisions. One cooperative background reader yields between files and sleeps briefly between streamed chunks; this is not an OS scheduling-priority guarantee.

Progress has `id`, `phase` (`running`, `completed`, `cancelled`, `failed`), `stage` (`sizing`, `sampling`, `verifying`), `total`, `candidates`, `processed`, `bytes_read`, `cache_hits`, `groups`, `errors`, `errors_omitted`, and `truncated`. The desktop additionally exposes `queued` before worker progress and `stale` after invalidating completed results. `total` is the selected nontrashed snapshot count; `candidates` counts assets with colliding size signatures; `processed` counts those candidates resolved or rejected by the later stages. Stream progress updates approximately every 100 ms and replaces a single pending snapshot rather than building an unbounded queue.

Cancellation is checked between source files, sample regions, and streaming chunks. A blocked operating-system filesystem operation must return before that check can run. Shutdown cancels and joins the worker on the catalog actor, then performs the final flush/acknowledgement, so the normal shutdown barrier does not leave cache writes running after it completes.

## Verification

Core tests cover schema migration, Trash/undo/restore, paired-file identity, cache invalidation, bounded errors, writer contention, and shutdown cancellation.

Run the real desktop acceptance harness separately from Cargo builds:

```sh
env -u WAYLAND_DISPLAY uv run --no-project python scripts/native-features.py
```

The harness owns private XDG directories, checks its own endpoint PID, launches/restarts only its own app, and records binary provenance plus JSON-RPC transcripts. It checks full Trash decision durability and undo/redo, live-filter/export exclusions, invalid requests, exact groups and sample-collision rejection, persisted warm scans, explicit subsets, duplicate collection navigation, streamed-scan cancellation/quit, and original SHA256/mtime preservation. Cancellation sources are valid BMPs with sparse zero trailers, not a camera-drive performance workload. Verification hashes add separate read traffic and are not counted as worker `bytes_read`.

Optional real HEIF coverage:

```sh
env -u WAYLAND_DISPLAY uv run --no-project python scripts/native-features.py \
  --heif-fixture /path/to/real.heic --heif-dimensions 1280x854
```

The fixture is copied into the private dataset. This checks standalone metadata, extension-neutral RAW+HEIF pairing, and byte-preserving companion export. The RAW partner is a synthetic pairing fixture, not a claim of RAW decoding quality. Successful preview decoding depends on an available HEIF codec; a missing codec must remain visible rather than being reported as a decoded preview.

