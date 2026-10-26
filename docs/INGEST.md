# Importing a shoot

Insert a mounted card, or choose **Import from Card/Folder…** from the command palette (Ctrl/Cmd+O). The Import pane scans originals read-only. Device imports walk DCIM, PRIVATE and MP_ROOT; a folder import walks the selected folder tree. Scanning lists files and reads bounded EXIF headers; it does not decode full RAWs or hash whole videos. Visible thumbnails use the regular asynchronous media pipeline, including sensor fallback when a RAW lacks a paired or supported embedded preview. JPEG XL, Classic TIFF/BigTIFF, HEIC/HEIF/HIF and recognized RAW extensions use the format limits in [Media](MEDIA.md). Videos have filename/size tiles, a Video badge, and a THM preview when available; playback and video culling are outside this feature.

The timeline groups files by month, day and time, initially using a 60-minute gap. Click a time group to select/deselect it; its day control selects/deselects that day's groups. All/None and Show all/New only control the selection. Previously imported files start unchecked and dimmed; their tooltip shows the import date and destination. A RAW and its JPEG/HEIF companion share a checkbox, sequence number and destination stem. XMP, AAE, THM, WAV, MP3, LOG and LRV companions travel with their group. A standalone LRV is a video.

**Gap: 60 min** means neighboring capture times within 60 minutes belong to the
same time section; a larger gap starts another section. Its tooltip explains the
control. Day/time checkboxes show a dash when only some members are selected.
Shift+Arrow and Shift-click extend a range from the selection anchor, and
Ctrl/Cmd-click toggles a single tile. The grid and timeline have draggable
scrollbars for reaching distant rows directly.

Choose a library root, optional backup roots, folder and filename templates, and a job code. The destination column shows a real selected file's example path and projected free space. Backup paths can be added with the picker or entered separated by semicolons. The preset menu includes Original names, Date folders, Date+job and Date_time_sequence. Save a custom preset under the job code (or “Custom”); Save as defaults also remembers destinations and the job-code MRU. Fields accept typing, Backspace, paste, Ctrl/Cmd+A to replace the field, Tab and Enter. Template edits refresh the example after a short debounce; stale plans cannot enable Start.

For **shoot → import → cull**, select the new shoot, start Import, and wait for all destinations to verify. The status bar reports copying/verifying/indexing, groups, throughput and ETA. Pause/Resume and Cancel remain available with the pane closed. Committed destination groups appear in Grid scoped to the imported folder, ready for picks and ratings; finalization failures can still leave the job marked failed. Open Import History for durable job results and inspect the manifest for exact destination paths and BLAKE3 digests.

For **shoot → cull → import**, choose **Cull first**. Shadowroom indexes the card in place and stores decisions in its own catalog. Rate/pick/reject normally, reopen Show Import Panel, then choose All, Picks, Except rejects or Rated ≥ N. Planning snapshots saved source decisions; destination rows inherit those decisions when their group commits. The source files and source sidecars are never written. Use Import Picks from This Card to select the Picks filter quickly. Decisions made after a job starts belong to the next import snapshot.

## Templates

Default folder: `{YYYY}/{YYYY}-{MM}-{DD}{_job}`. Default filename: `{original}`. The filename template generates a **stem**; the original media extension is appended automatically. Sidecars retain both `name.xmp` and `name.ARW.xmp` conventions.

| Tokens | Meaning |
| --- | --- |
| `{YYYY}`, `{YY}`, `{MM}`, `{MMM}`, `{DD}` | Capture year, short year, month number, English short month, day |
| `{hh}`, `{mm}`, `{ss}`, `{subsec}` | Camera wall-clock hour, minute, second, three-digit milliseconds |
| `{date}`, `{time}` | `YYYY-MM-DD`, `HHMMSS` |
| `{import:YYYY-MM-DD}` | Import date in UTC; YYYY, YY, MM and DD can be combined with literal separators |
| `{original}` | Original filename without its extension |
| `{original_ext}`, `{ext}` | Original extension preserving case; lowercase extension |
| `{camera}`, `{make}`, `{serial}`, `{lens}`, `{iso}` | Available bounded EXIF metadata; missing values expand to empty text |
| `{job}`, `{_job}` | Job code; optional job code prefixed by `_` |
| `{seq_session:4}` | Successful new group number in this execution, padded to four digits |
| `{seq_day:3}` | Persistent group count for the capture day, padded to three digits |
| `{seq_stored:5}` | Persistent catalog-wide successful group count, padded to five digits |
| `{letter}` | `a`…`z`, `aa`… for consecutive groups captured in the same second |

Sequence widths are optional, or 1–18 when specified. The day starts at 03:00 by default: a 02:30 capture counts toward the previous day. Explicit EXIF offsets restore the camera's own wall clock for naming. Offsetless EXIF remains wall time; comparing EXIF with mtime infers UTC/local device clock behavior within two minutes. Unknown capture times use mtime with a visible warning. Inferred local clocks also guide mtime-only naming; inference never fabricates an EXIF offset.

Names use a conservative portable policy: illegal/control characters and whitespace become separators, repeated separators collapse, leading/trailing separators/dots are removed, reserved DOS names are escaped, and components are capped at 180 UTF-8 bytes before their media/sidecar suffix. Absolute templates, traversal and unknown tokens are rejected. A collision on any media file, sidecar or backup applies a common `-1`, `-2`, … suffix to the whole group, or skips the group under Skip policy. Identical existing files are verified and recorded without replacement or advancing sequence counters.

## Backup and verification

Each source file is read once by a sequential reader. Bounded 1 MiB chunks fan out to independent primary/backup writers through four-chunk queues. Every destination uses an exclusive destination-local staging file; the worker hashes the source stream, preserves mtime, syncs each output, and rereads every primary and backup to compare BLAKE3 and byte size. Linux uses `renameat2(RENAME_NOREPLACE)`, macOS uses `renamex_np(RENAME_EXCL)`, and unsupported kernels use an exclusive hard-link fallback. Final directory entries are verified again and their directories synced. Existing files, directories and symlinks are never replaced.

The producer stops at the initial source-size bound plus one detection byte;
growth is an error before extra bytes are queued to any writer. Destination
verification is bounded by its expected size plus one byte and opens regular
files (rejecting terminal symlinks and special files on Unix). Bounded queues
therefore also have a finite per-file input, even if another process keeps
appending to the original.

The group commits only after every member at every destination verifies. The history record, destination photo decisions and persistent counters share one SQLite transaction. A copy/verify/publish/database failure removes newly created entries from that group, including earlier verified members; existing identical files remain. Cleanup checks file identities and reports failures instead of deleting a path replaced by someone else. Completed groups survive cancellation. Retry verifies recorded destinations and skips them; changed or missing backups are repaired through the normal verified group path.

Capacity is checked before creating destinations. Required bytes are aggregated by filesystem, so primary and backup on the same disk count twice; an additional 1 MiB margin covers metadata. `FsStats` and `CopyHooks` are injectable for capacity/corruption tests.

A uniquely named `shadowroom-ingest-<job-id>.json` manifest records status, counts, errors, exact file paths, fingerprints, hashes, primary/backup outputs and pre-existing-file flags. Finalization first checkpoints a non-success **finalizing** history row with no finished timestamp. The proposed manifest is exclusively created, synced, reread and verified, with directory sync and file-identity checks. Only then can the final history write mark the job completed. While history is finalizing, the proposed manifest is provisional.

If manifest verification or the final history write fails, the job reports failure and retains committed groups. Bounded compensation attempts record failed history and replace only the job's own manifest with an exclusively created, verified failed outcome. A foreign replacement is never adopted, overwritten or deleted. Manifest creation, cleanup and history-write refusals are reported explicitly; no retry loop can guarantee persistence when storage keeps rejecting writes. The returned manifest pointer is cleared when no certified artifact can be retained. Check both terminal progress and history rather than treating a briefly visible manifest as completion.

Before accepting commands at startup, the ingest worker repairs abandoned **running/finalizing** history rows to failed, reconstructing committed counts from `ingest_log`. Committed media, decisions and sequence counters remain intact; unfinished groups are not inferred successful. Repair failures are surfaced. This repairs catalog history, not every leftover manifest or staging file, and requires writable storage.

Group atomicity is an application commit/rollback guarantee: filesystems do not atomically rename multiple independent filenames. Another process can briefly observe a group during publication. A power loss/process kill during publication may leave staging files or a partial unpublished group; there is no crash journal that automatically removes those artifacts. They are not certified successful. Retrying checks names and hashes without overwriting them. This limitation also applies across separate backup filesystems.

## Seen tracking and preferences

`ingest_log` stores one group record, source volume identity (Linux UUID/label when available; label fallback elsewhere), relative source path, original name, size, nanosecond mtime, capture time/provenance, full content BLAKE3, all outputs, job and import time. Matching never requires the old mount prefix. Lookup uses a known content hash first, then exact filename/size/capture time, then filename/size/mtime with a difference no greater than 24 hours and exactly a multiple of 15 minutes. A known mismatching hash does not fall back to a weaker match. Stat-only scans normally lack a hash, so metadata matches are best-effort; copy/collision verification always checks full content. Mark as imported records explicit selected groups without copying and without consuming sequence counters.

Preferences → Import exposes card insertion behavior (Ask by default, Auto, Index in place, Ignore), day start, opt-in automatic ejection and remembered per-volume allow/deny. Deny suppresses automatic discovery; manual import remains available. The pane edits saved library, templates, backup roots, videos, collision policy, time gap and job codes. Settings keys are `ingest_on_insert`, `ingest_library_root`, `ingest_folder_template`, `ingest_file_template`, `ingest_backup_roots`, `ingest_job_codes`, `ingest_day_start_hour`, `ingest_collision`, `ingest_time_group_minutes`, `eject_after_ingest`, `ingest_include_videos`, `ingest_volumes`, and `ingest_presets` (preset name → `[folder_template,file_template]`). Existing settings gain serde defaults without losing unknown fields.

Eject is explicit unless `eject_after_ingest` was enabled. Linux uses `gio mount -e`; macOS uses `diskutil eject`. Only a discovered removable volume's mount root is eligible. A normal folder, including a test temp directory, returns unsupported without invoking either command. Busy/unavailable volume-manager failures are shown. No delete-from-card command exists.

## MCP tools and a folder test

All disk work is asynchronous. Scan, plan and history tools acknowledge queueing so GPUI never waits for filesystem or SQLite work; poll `ingest_status` for `scanning:false`, a non-null ready plan, progress and errors. `ingest_history` requests durable history; its result arrives in `ingest_status.history`. Scan item IDs are independent of catalog photo IDs. Status returns up to 100 scan items by default; `offset`/`limit` page them (limit ≤ 1000). Planning uses saved defaults plus the supplied options. Empty folder templates are supported.

These are exact MCP `tools/call` payloads; use an existing **test-owned** source directory, and replace the example `/tmp/shadowroom-ingest-e2e` tree with one created under the checkout's `target/test-tmp` when /tmp capacity is limited. Do not start with default destinations during a test.

```json
{"name":"ingest_scan","arguments":{"path":"/tmp/shadowroom-ingest-e2e/card"}}
{"name":"ingest_status","arguments":{"offset":0,"limit":100}}
{"name":"ingest_plan","arguments":{"options":{"library_root":"/tmp/shadowroom-ingest-e2e/library","folder_template":"{YYYY}/{date}{_job}","file_template":"{date}_{time}_{seq_stored:5}","backup_roots":["/tmp/shadowroom-ingest-e2e/backup"],"job_code":"verification","selection":"all","include_videos":true,"collision":"suffix","day_start_hour":3}}}
{"name":"ingest_status","arguments":{}}
{"name":"ingest_start","arguments":{}}
{"name":"ingest_status","arguments":{}}
{"name":"ingest_history","arguments":{"limit":100}}
{"name":"ingest_status","arguments":{}}
{"name":"eject","arguments":{"path":"/tmp/shadowroom-ingest-e2e/card"}}
```

Poll after scan and plan before advancing; finalizing is not success. Poll after Start until `progress.phase` is `completed`, `failed`, or `cancelled`. Check `progress.destinations`, manifest hashes, and `history.records[].files[].destinations` to verify both outputs. Eject in this test must set `error` to an unsupported-folder message. Extra controls:

```json
{"name":"ingest_pause","arguments":{}}
{"name":"ingest_resume","arguments":{}}
{"name":"ingest_cancel","arguments":{}}
{"name":"ingest_mark_imported","arguments":{"ids":[0]}}
{"name":"ingest_plan","arguments":{"options":{"library_root":"/tmp/shadowroom-ingest-e2e/library","folder_template":"{date}","file_template":"{original}","selection":"picks","backup_roots":["/tmp/shadowroom-ingest-e2e/backup"]}}}
```

The legacy `import {path}` MCP tool remains index-in-place for compatibility and cull-first automation. Use it, then `library_list`/rating/flag tools, wait for saved decisions, and call `ingest_plan` with a cull selection. Ingest has no remote/PTP/MTP transport and no video playback. Preview disk-cache keys include source paths, so destination previews regenerate lazily when an existing key cannot be reused.
