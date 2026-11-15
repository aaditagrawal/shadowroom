# Importing a shoot

Insert a mounted card, or choose **Import from Card/Folder…** from the command palette (Ctrl/Cmd+O). The Import pane scans originals read-only. Device imports walk DCIM, PRIVATE and MP_ROOT; a folder import walks the selected folder tree. Scanning lists files and reads bounded EXIF headers; it does not decode full RAWs or hash whole videos. Visible thumbnails use the regular asynchronous media pipeline, including sensor fallback when a RAW lacks a paired or supported embedded preview. JPEG XL, Classic TIFF/BigTIFF, HEIC/HEIF/HIF and recognized RAW extensions use the format limits in [Media](MEDIA.md). Videos have filename/size tiles, a Video badge, and a THM preview when available; playback and video culling are outside this feature.

The timeline groups files by day and session, initially using a 60-minute gap. Each day row ("Sun 26 Oct 2026 · 12 photos") has a checkbox at its left edge that checks or unchecks the whole day; the session rows indented under it ("15:00 · 6 photos") check or uncheck that session. Clicking either also scrolls the grid to its first photo. The header segment **All 612 | New 598** shows every photo or only those not yet imported; **Check all** includes every shown photo (hidden ones keep their state) and **Uncheck all** leaves out everything. Previously imported files start unchecked and dimmed with a chip such as "Imported 12 Oct" (or "Marked imported"); their tooltip shows the import date and destination. Photos this job has copied and verified show a **Verified** check at full strength. A RAW and its JPEG/HEIF companion share a checkbox, sequence number and destination stem. XMP, AAE, THM, WAV, MP3, LOG and LRV companions travel with their group. A standalone LRV is a video.

**Split sessions after 60 min** (the dropdown above the timeline: 15, 30, 60 or
120 minutes) means neighboring capture times within 60 minutes belong to the
same session; a larger gap starts another. Day/session checkboxes show a dash
when only some members are checked.

In the grid, the *current* tile (accent ring) and the *checked* tiles (neutral
outline, filled checkbox) are separate. A plain click or an arrow key only moves
the current tile, so looking at photos never changes what is imported. The
checkbox, Space and Ctrl/Cmd-click include or leave out one photo;
Shift-click and Shift+Arrow give the range from the anchor the anchor's state
(checked or unchecked), and shrinking the range restores the photos that left
it. The grid and timeline have draggable scrollbars for reaching distant rows
directly.

The right-hand column is grouped into Job, Destination, Naming, Options and More. **Job code** comes first (press J to jump to it) with up to five recent codes as chips. Choose a library folder and optional backup folders (added with **+ Add backup folder…**, or typed and separated by semicolons); long paths are shortened in the middle, keeping the root and the last folders. Once planned, each destination shows what the import needs and the space left ("Needs 4.1 GB · 412 GB free on Photos-SSD (408 GB after)") with a bar for the import's share, red when there is not enough space; a backup row has a × to remove it. Under Naming, the Preset dropdown opens below its field and lists Original names, Date folders, Date+job, Date_time_sequence and saved custom presets with their templates; it shows Custom once a template is edited. Save current as a preset from the same menu (named after the job code, or “Custom”). The Example box under the templates shows a real selected file's destination. **Naming tokens** below it is folded until opened (it opens by itself while a template field is being edited): hover a token for its meaning and click it to insert it at the caret of the subfolder or filename template (the last one edited). Options holds Import (All photos, Picks, Everything except rejects, Rated photos with a minimum star rating), Videos (Include/Skip) and the name-collision policy (Add suffix/Skip). Dropdowns support Up/Down, Enter and Escape. Save as defaults also remembers destinations and the job-code MRU. **Mark selected as imported…** records the checked photos without copying them; it asks first, with the count, because marked photos start unchecked the next time the card is scanned. Fields accept typing, Backspace, paste, Ctrl/Cmd+A to replace the field, Tab and Enter.

The footer holds one filled button, **Import 612 photos · 48 GB**, with its own icon. It is disabled, with the reason in its tooltip, while the scan runs, nothing is checked, the plan is being checked or a problem is shown above it. Planning runs automatically a moment after any change (Refresh Import Plan in the command palette re-checks it). Next to the button the footer says where the photos go ("→ Library/2026/2026-10-26_Iceland · +1 backup"), and one line states the guarantees: copies are read back and checked, nothing on the card is changed, existing files are never overwritten.

The Import view is fully keyboard operable: arrows move the current tile, Space checks or unchecks it, Tab moves into the fields (Job code first) and between them, J jumps to Job code, Enter leaves a field, and **Ctrl/Cmd+Return starts the import** from anywhere in the view. Escape in a field only leaves the field; a second Escape closes Import. The command palette has Start Import, Pause or Resume Import, Cancel Import and Refresh Import Plan. While Import is open no view tab is underlined, and the source drive's sidebar row is marked as the current location.

For **shoot → import → cull**, select the new shoot, start Import, and wait for all destinations to verify. While it runs the footer and the status bar read "Copying 37 of 612 · 8.4 of 48 GB · 142 MB/s · about 4 min left" over a bar weighted by bytes, so one large video never stalls it; skipped and failed counts follow in the warning and error colours. Speed is a moving average of one-second windows and time left counts only time spent copying, so a pause does not distort either; "estimating…" shows for the first seconds. Pause/Resume and Cancel remain available with the pane closed. Cancel asks once (a second click within four seconds cancels; Cancel Import in the palette cancels at once). Afterwards the footer keeps the outcome, such as "Cancelled · 120 of 612 photos imported and verified · 492 still selected" with **Import remaining**, or "Imported 612 photos · verified · 14 already in the library (identical, skipped)". Committed destination groups appear in Grid scoped to the imported folder, ready for picks and ratings; finalization failures can still leave the job marked failed. Open Import History for durable job results and inspect the manifest for exact destination paths and BLAKE3 digests.

For **shoot → cull → import**, choose **Cull first**. Shadowroom indexes the card in place and stores decisions in its own catalog. Rate/pick/reject normally, reopen Show Import Panel, then choose All photos, Picks, Everything except rejects or Rated photos (with a minimum rating) from the Import dropdown. Planning snapshots saved source decisions; destination rows inherit those decisions when their group commits. The source files and source sidecars are never written. Use Import Picks from This Card to select the Picks filter quickly. Decisions made after a job starts belong to the next import snapshot.

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

Each source file is read once by a sequential reader. Bounded 1 MiB chunks fan out to independent primary/backup writers through four-chunk queues. Every destination uses an exclusive destination-local staging file; the worker hashes the source stream, preserves mtime, and rereads every primary and backup to compare BLAKE3 and byte size. Destination verification runs concurrently. Linux uses `renameat2(RENAME_NOREPLACE)`, macOS uses `renamex_np(RENAME_EXCL)`, and unsupported kernels use an exclusive hard-link fallback. Publication checks that each new final name refers to the verified staging file; an existing file is verified by content. All output files and destination directories are synced before the catalog commit. Existing files, directories and symlinks are never replaced.

The producer stops at the initial source-size bound plus one detection byte;
growth is an error before extra bytes are queued to any writer. Destination
verification is bounded by its expected size plus one byte and opens regular
files (rejecting terminal symlinks and special files on Unix). Bounded queues
therefore also have a finite per-file input, even if another process keeps
appending to the original.

The group commits only after every member at every destination verifies. A bounded commit worker flushes at 64 groups, 256 open outputs, a 500 ms collection deadline, or pause/completion. File syncs overlap, each destination directory syncs once per batch, and history records, destination photo decisions and persistent counters share one SQLite transaction. A copy/verify/publish failure removes newly created entries from that group; a sync/database failure rolls back all groups in the affected batch. Existing identical files remain. Cleanup checks file identities and reports failures instead of deleting a path replaced by someone else. Verified groups already handed to the commit worker survive cancellation if their commit succeeds. Failed batches discard later queued groups before sequence counters are reloaded, so later names use only committed counters. Retry verifies recorded destinations and skips them; changed or missing backups are repaired through the normal verified group path.

Capacity is checked before creating destinations. Required bytes are aggregated by filesystem, so primary and backup on the same disk count twice; an additional 1 MiB margin covers metadata. `FsStats` and `CopyHooks` are injectable for capacity/corruption tests.

A uniquely named `shadowroom-ingest-<job-id>.json` manifest records status, counts, errors, exact file paths, fingerprints, hashes, primary/backup outputs and pre-existing-file flags. Finalization first checkpoints a non-success **finalizing** history row with no finished timestamp. The proposed manifest is exclusively created, synced, reread and verified, with directory sync and file-identity checks. Only then can the final history write mark the job completed. While history is finalizing, the proposed manifest is provisional.

If manifest verification or the final history write fails, the job reports failure and retains committed groups. Bounded compensation attempts record failed history and replace only the job's own manifest with an exclusively created, verified failed outcome. A foreign replacement is never adopted, overwritten or deleted. Manifest creation, cleanup and history-write refusals are reported explicitly; no retry loop can guarantee persistence when storage keeps rejecting writes. The returned manifest pointer is cleared when no certified artifact can be retained. Check both terminal progress and history rather than treating a briefly visible manifest as completion.

Before accepting commands at startup, the ingest worker repairs abandoned **running/finalizing** history rows to failed, reconstructing committed counts from `ingest_log`. Committed media, decisions and sequence counters remain intact; unfinished groups are not inferred successful. Repair failures are surfaced. This repairs catalog history, not every leftover manifest or staging file, and requires writable storage.

Group atomicity is an application commit/rollback guarantee: filesystems do not atomically rename multiple independent filenames. Another process can briefly observe a group during publication. A power loss/process kill during publication may leave staging files or a partial unpublished group; there is no crash journal that automatically removes those artifacts. They are not certified successful. Retrying checks names and hashes without overwriting them. This limitation also applies across separate backup filesystems.

## Seen tracking and preferences

`ingest_log` stores one group record, source volume identity (Linux UUID/label when available; label fallback elsewhere), relative source path, original name, size, nanosecond mtime, capture time/provenance, full content BLAKE3, all outputs, job and import time. Matching never requires the old mount prefix. Lookup uses a known content hash first, then exact filename/size/capture time, then filename/size/mtime with a difference no greater than 24 hours and exactly a multiple of 15 minutes. A known mismatching hash does not fall back to a weaker match. Stat-only scans normally lack a hash, so metadata matches are best-effort; copy/collision verification always checks full content. Mark as imported records explicit selected groups without copying and without consuming sequence counters.

Preferences → Import exposes card insertion behavior as a choice group (Ask by default, Import automatically, Cull in place, Ignore, each described under the group), the hour the day begins (any hour in one click), opt-in automatic ejection (Off/On) and, while a card is scanned, whether discovery offers that named card (Offer/Ignore). It also shows the library path and naming templates, changed in Import. Deny suppresses automatic discovery; manual import remains available. The pane edits saved library, templates, backup roots, videos, collision policy, time gap and job codes. Settings keys are `ingest_on_insert`, `ingest_library_root`, `ingest_folder_template`, `ingest_file_template`, `ingest_backup_roots`, `ingest_job_codes`, `ingest_day_start_hour`, `ingest_collision`, `ingest_time_group_minutes`, `eject_after_ingest`, `ingest_include_videos`, `ingest_volumes`, and `ingest_presets` (preset name → `[folder_template,file_template]`). Existing settings gain serde defaults without losing unknown fields.

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
