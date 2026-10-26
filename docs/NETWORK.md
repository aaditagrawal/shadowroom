# Shadowroom remote photo access

The headless agent serves metadata and display previews over SSH. The desktop retains source identities, decisions and preview caches locally, with Network sidebar and MCP host controls. See [Media](MEDIA.md) for supported formats and rendering limits.

## Architecture and operating instructions

The GUI opens one SSH child per host, authenticating and encrypting through the user's existing OpenSSH configuration:

```text
ssh -o BatchMode=yes -o ConnectTimeout=10 -T [extra SSH args] -- HOST REMOTE_COMMAND
```

The remote command is a trusted, user-configured shell command, normally `shadowroom-remote serve --stdio`. It must produce no shell banners or other stdout output. stderr remains a separate inherited diagnostic stream. There are no listening ports, remote passwords, credential storage, mounts, or original-file downloads in the normal preview path. Extra SSH arguments are trusted configuration, not data from a photo or remote response.

Build and manual use:

```bash
export CARGO_BUILD_JOBS=4
cargo build --release --no-default-features --bin shadowroom-remote
./target/release/shadowroom-remote --version
# Agent configuration: discovered external drives plus repeated explicit roots.
shadowroom-remote serve --stdio --state-dir /internal/agent-state --root /Volumes/Photos --decode-budget-mb 2048
# Exact allowlist: discovery is disabled.
shadowroom-remote serve --stdio --no-discover --root /tmp/test/photos --state-dir /tmp/test/state
# The whole remote command is passed as one string. Quote shell paths within it.
./target/release/shadowroom-remote probe --ssh darkstar \
  --remote-cmd '/tmp/test/shadowroom-remote serve --stdio --no-discover --root /tmp/test/photos --state-dir /tmp/test/state' \
  --index /tmp/test/photos --fetch-thumbs 3 --out /tmp/test-output --cache-dir /tmp/test-client-cache
```

The agent's shared decode budget defaults to **2 GiB**. `serve --decode-budget-mb N` accepts **64–65536 MiB**; it controls media admission and wire-encoding reservations, separately from `--cache-budget-mb` disk storage. Zoom prefers a paired JPEG/HEIF or embedded JPEG. When neither exists, a typical 24 MP sensor RAW fallback estimate is about 1.2 GiB; smaller budgets can reject it explicitly. Admission is conservative, not a hard native allocator/RSS cap. The client desktop defaults to automatic memory sizing (a quarter of physical RAM, bounded to 512 MiB–4 GiB), with manual limits and separate GPU costs.

The default state directory is `directories::ProjectDirs::from("app", "shadowroom", "shadowroom-remote").data_local_dir()` (normally `$XDG_DATA_HOME/shadowroom-remote` or `~/.local/share/shadowroom-remote` on Linux, an Application Support directory on macOS). It contains a stable random device id, an OS advisory session lock, per-root SQLite catalogs, revision manifests, the existing media cache and a wire-format JPEG cache. State is rejected before creation if it is inside any served photo root. Put it on internal storage. The source tree receives no files, decisions, sidecars, or cache writes.

One running stdio agent may use a state directory at a time. The advisory lock releases on process exit/crash; its file need not be deleted. Multiple concurrent GUI connections to the same state directory are rejected explicitly. A future multi-client daemon would need transactional shared revision storage; silently racing JSON snapshots would be incorrect.

`RemoteHost::connect(spec, cache_dir, timeout)` supports SSH or `ConnectionSpec::Local` for child-process tests. Clones share a session. Use a few caller threads to fetch previews in parallel. All methods block their caller, so invoke them on service/background workers. `from_io` accepts a custom blocking reader/writer (the tests use an in-process Unix socket pair). Custom transport owners must shut down their underlying transport when disconnecting; duplicated full-duplex socket handles do not become write-half-closed just by dropping one handle.

`request`/`request_with_timeout` expose multiplexed streams and request ids. `PendingRequest::cancel()` sends a control message; dropping an unfinished request also cancels it. `index` has a progress callback. For large libraries use `photos_stream` to apply bounded batches to a local catalog; `photos` collects results in memory for small callers/probes. `stat` and bounded `read_range` provide the fallback building blocks without adding SFTP.

## Protocol version 1

All integer prefixes are unsigned little-endian:

```text
u32 body_len
  u32 json_len
  json_len bytes of compact UTF-8 JSON
  payload_len bytes of binary payload
```

`body_len` counts the inner length prefix, JSON and payload. The envelope is:

```json
{"id":17,"done":true,"payload_len":0,"message":{"type":"ping"}}
```

Request ids are nonzero and unique while outstanding. Requests always have `done:true`, with no payload. Responses echo the id; streaming responses have `done:false` until exactly one terminal response. Frames for different ids may interleave. Binary preview/range data is never base64 encoded. JSON variants use a `type` discriminator with snake_case names. `PreviewKind` retains the existing library representation: `"Thumbnail"`, `"Fit"`, `"Full"`.

Limits: 16 MiB body, 256 KiB JSON, at most `16 MiB - 256 KiB - 4` binary bytes, 1 MiB range length, 32 outstanding application requests, four agent workers, and an eight-frame mailbox per client request. A stalled stream gets up to one second of reader backpressure before being disconnected; it cannot silently return a successful partial sync. Drain callbacks promptly and dispatch database work in reasonably small batches. The bounded client writer queue avoids blocking the caller on SSH stdin. Partial reads are supported. Oversized, inconsistent, malformed and truncated frames are errors; EOF is clean only between frames. An invalid transport closes the session and fails pending requests.

Messages and terminal responses:

| Request | Responses |
| --- | --- |
| `hello {protocol_version, app_version}` | `hello {protocol_version, app_version, device_id, hostname, os, arch, capabilities}`; `version_mismatch` on incompatible protocol |
| `list_roots` | `roots {roots:[{id, volume_id, identity_kind, path, label}]}` |
| `index {root}` | streamed `progress`, optional nonterminal `error {code:"index_warning", message}`, terminal `indexed {progress, revision, cancelled}` |
| `photos {root, since_revision}` | streamed `photo {key, metadata, revision}`, `deleted {relative_path}`, terminal `synced {revision}` |
| `preview {key, kind, max_bytes}` | `preview {width, height, mime:"image/jpeg", rendered_raw, embedded_raw, rendering_note}` plus JPEG payload |
| `read_range {root, path, offset, len}` | `range` plus at most `len` bytes; EOF may return fewer |
| `stat {root, path}` | `stat {size_bytes, modified_at, is_dir}` |
| `cancel {request_id}` | `cancelled {request_id}` acknowledgement on the cancellation message's own id |
| `ping` | `pong` |

Hello is required before other operations. Matching protocol versions interoperate regardless of app version; app version is diagnostic. A mismatched hello does not negotiate a session. General terminal errors carry a code and message (`request_failed`, `cancelled`, `busy`, `hello_required`, etc.). Cancellation acknowledgement means the flag was set if the target was still active; it does not promise to retract bytes already sent. Indexing normally returns a cancelled summary; queued work may instead fail with a terminal `cancelled` error. Media decodes already running are not forcibly interrupted; stale responses are discarded and the bounded media workers may finish populating useful caches.

Root names are opaque ids in requests, not arbitrary absolute paths. `probe --index` accepts a reported id or path and resolves it to an id. Linux volume identity uses the containing mount and `/dev/disk/by-uuid`, then `/dev/disk/by-label` where accessible. macOS asks `diskutil info -plist` for VolumeUUID/MountPoint. The fallback is a mount path. `identity_kind` makes this distinction explicit. Root ids hash the volume identity plus the root's location within that filesystem, including the Linux mountinfo subtree for bind mounts. Ambiguous duplicate root identities are rejected explicitly. The device id namespaces identities across machines. A path fallback cannot distinguish a replacement disk mounted at the same location; the GUI should expose that limitation and offer a metadata refresh after drive replacement. Labels are weaker than UUIDs and can collide.

## Revision and photo identity contract

An indexed photo has this source identity:

```text
(device_id, root_id, volume_id, relative_path)
```

A `PhotoKey` adds `size_bytes`, `modified_at` (milliseconds since Unix epoch), and companion relative path/size/mtime. A `PhotoRecord` includes that key, the existing `model::Photo` metadata, and its last changed revision. `Photo.path`, `folder` and `companion_path` in remote metadata are root-relative. The agent's numeric catalog id is not a GUI catalog id. Do not interpret any of these paths as local paths.

Indexing reuses `Indexer`'s incremental fingerprint checks in a catalog owned by the agent. After a complete scan, the agent compares its catalog against the prior manifest, advances the revision only if records changed, and persists a new manifest atomically. Removed files generate persistent tombstones after error-free scans; scans with per-file errors retain unseen records to avoid false deletions. Cancelled or failed scans do not publish a revision. Original-file metadata is refreshed only by index, not by `photos`, so polling `photos` never scans a drive. Revision zero requests a full snapshot. A revision ahead of the agent is an explicit error: reset to zero. Apply all records/tombstones and the terminal revision in one local transaction; keep the previous cursor if the stream fails. A different device id means different state, so start with revision zero.

`dhash` is optional. Preview generation saves the media pipeline hash through `Catalog::set_dhash`; a subsequent sync publishes it. After indexing, clients also use the optional `analyze` capability to generate thumbnails/hashes only for records lacking them, warm the agent cache, then sync the new revision. This completes remote burst grouping without transferring every preview. Agents without that capability skip the pass.

The index operation streams progress; photo records are a separate `photos` stream after its terminal response. This keeps revision publication atomic and lets the client recover a disconnected sync without repeating a scan. This is a deliberate adaptation of the proposed combined index/records message.

## Preview caching and drive I/O

The agent uses the shared `MediaPipeline`: 384px thumbnails, 2560px Fit, native format/color handling and version-9 local media caches. All tiers prefer a paired JPEG/HEIF, then an embedded RAW JPEG, with LibRaw sensor fallback if no supported preview exists. **Full uses that selected preview at its native resolution**. Its dimensions can therefore differ from the RAW capture dimensions; provenance identifies the actual source rather than calling a camera preview a sensor render.

Pixels are already oriented sRGB and travel as **JPEG quality 85**, even for TIFF/JXL/HEIF or RAW sources. Notes identify JPEG wire encoding, actual transmitted dimensions and source provenance, including LibRaw/white-balance details. Thumbnail/Fit requests downsize a larger satisfying image to their requested tier. **Full is never resized to fit the wire budget**: exceeding `max_bytes` or the roughly 16 MiB framing ceiling fails explicitly, without changing quality or dimensions. The exact binary payload limit is listed above; encoding stops before growing beyond it. There is no chunked Full transport.

New agents advertise `preview_jpeg_first_v4`. Responses and both encoded-JPEG caches retain `rendered_raw`, `embedded_raw` and `rendering_note`; clients verify JPEG dimensions and reject conflicting RAW flags. Older agents advertising `preview_provenance_v2` retain their sensor-only RAW Full contract, which clients still enforce. Legacy replies without provenance remain readable but carry an explicit Full-provenance-unverified note, also retained offline.

Client/server representation revision 4 invalidates earlier preview representations independently of protocol version 1. The server uses `jpeg-v4`; earlier version directories are obsolete. Client keys include representation/protocol versions, device identity, the entire photo/companion fingerprint and kind. JPEG/checksum metadata is published atomically; corrupt/partial entries are misses. Cached payloads and metadata use bounded reads of regular files; on Unix, final-component symlinks and special files are rejected. Both cached and newly received previews must have matching JPEG dimensions, bounded pixels/notes, and consistent RAW provenance. A matching byte checksum alone does not validate metadata. Warm fetches consult the client cache before connecting and transfer zero protocol bytes. `RemoteHost::offline_preview` reopens persisted JPEGs and provenance after restart; disconnect retains them. The GUI retains metadata to enumerate offline photos.

Metadata indexing and first preview generation can read a source separately because the existing public indexer and media APIs do not share source bytes. Incremental scans avoid repeated metadata reads for unchanged originals, and warmed previews avoid original content reads. Canonicalization/stat calls still touch filesystem metadata. Do not promise exactly one physical drive read for the first index plus preview. RAW compressed-byte and image pixel safety limits remain those of the existing media pipeline.

The media cache has its existing budget. The client JPEG cache defaults to a 2 GiB byte LRU, configurable with the desktop `remote_cache_budget_mb` preference (MCP `settings_set`). The extra agent JPEG cache defaults to a 4 GiB byte LRU, configurable with `serve --cache-budget-mb N`. Zero disables persistent JPEG retention. Cache recency persists via modification times; indexing a cache directory happens once per cache instance, with subsequent updates proportional to the touched/evicted entries. Eviction includes JSON sidecars, can remove rebuildable representations, and never removes catalog metadata, decisions, or originals. A cached higher-resolution JPEG can satisfy an offline lower-resolution request.

## Security, lifecycle and limits

Only configured/discovered roots are served. All request paths must be relative, reject `..` and absolute/prefix components, reject every symlink component (including in-root symlinks), canonicalize, and pass a component-aware root prefix check. Indexing uses the existing scanner, which ignores symlink entries. Photo preview keys must exactly match an indexed manifest record and volume id. The agent checks primary and selected preview-source size/mtime before its JPEG cache lookup, including the companion for Full when paired. Changed sources require re-indexing. A client cache hit can still serve the indexed revision without contacting the agent until metadata is refreshed. No protocol request writes originals or exports sidecars.

The SSH account and the remote filesystem owner are trusted. Canonicalization plus `open` is not an atomic capability-style filesystem traversal: a hostile local process can race a directory/symlink replacement between checks and reads. Hard links and mounts inside a root are also part of that root's access boundary. Strong confinement against concurrent hostile filesystem mutation needs OS-specific `openat`/`openat2` descriptor traversal (and a descriptor-based decode API) or an OS sandbox. This implementation prevents static traversal and symlink escapes; it is not a sandbox against the SSH user. Do not expose an untrusted shared writable directory under this threat model.

Roots/mount identities are a connection-time snapshot. Reconnect when mounts change. Removing a drive may make reads fail; cached previews still work locally. Source changes preserving both size and millisecond mtime are outside the existing indexer's fingerprint guarantees. Non-UTF-8 paths are unsupported by the JSON/catalog path representation. Revision manifests are whole JSON snapshots, with retained tombstones; very large libraries need transactional/chunked storage and tombstone compaction. After an index with per-file errors, inspect the returned error count before presenting the result as a fully verified library.

Request timeouts are overall deadlines, including writer-queue delay; progress does not reset them. Use an appropriate long index timeout or `request_with_timeout`. On timeout/drop the client sends best-effort cancel. Request operations never reconnect or replay automatically: call `reconnect(attempts)` (at most six attempts, 100/200/400/800/1600ms backoff) and decide whether to retry. Lifecycle calls should have a single owner in the service; fetching requests may run concurrently. Disconnect closes stdin and gives the process a short grace period before killing/reaping the local SSH child. Agent EOF cancels queued/running work and joins protocol workers. An already running media decoder can continue until its library cancellation boundary/process exit.

SSH bootstrap is deferred: first query `ssh HOST shadowroom-remote --version`, verify protocol/OS/arch, and allow deployment only of an explicitly available matching trusted binary. Upload to a versioned temporary path in user-writable internal storage, verify a digest, chmod, and atomically rename. Never fetch/execute an arbitrary URL, alter SSH config, install system packages, or overwrite a running binary in place. The user-facing connection setup should display a concrete target path and chosen binary. This phase only supports manually supplied remote commands.

## Desktop setup and operation

1. Build/copy the matching `shadowroom-remote` binary to the remote machine yourself. Configure key-based SSH and test your existing alias outside the app. Automatic bootstrap/upload is not implemented.
2. In Shadowroom's command palette choose **Add host…**, enter the SSH alias or `user@host`, and optionally replace the command. Tab switches fields, clipboard paste works, Enter adds/connects. The default command is `shadowroom-remote serve --stdio`.
3. The background connection checks protocol compatibility through `hello`, reporting agent/app version, device id, OS and architecture through `remote_status`. A failed check leaves a readable diagnostic. SSH stderr is also available in the launching terminal.
4. Expand/use the **Network** sidebar host and root rows. Root tooltips show the reported path, volume id and identity quality. Click a root to scope the library. Use **Index: host / root** in the palette for the first scan or to refresh filesystem metadata; **Refresh: host / root** syncs an already-indexed revision without rescanning the drive.
5. Palette **Connect**, **Disconnect**, and **Cancel** commands manage each host. The status bar shows connection/index activity, scan counts, and offline host counts. Root selection and local decisions survive disconnection. Offline cache misses display **Unavailable offline**, including when a thumbnail is available but Fit/Full is missing.

The host form is deliberately a small SSH alias/command form. Display names, extra SSH arguments, explicit local child transport for tests, removal, and cache budgets are available through MCP. Host editing currently means remove/add. Removal retains catalog rows, decisions and cached previews. Startup loads hosts and roots offline; connection is explicit. Reconnect after mounts change, and re-index to refresh a drive using a path/label identity. A changed device id stops reconnection and asks for an explicit remove/add association; it never merges old and new devices by hostname.

## Implemented desktop architecture

### Source identity and migration (`model.rs`, `catalog.rs`, `grouping.rs`)

`Photo.source` is a serialized `PhotoSource`, with `Local` as its backward-compatible default and `Remote` carrying device/root/volume identity, relative source/companion paths, fingerprints, and availability. Local paths remain in `Photo.path`, avoiding a redundant local path copy in the enum. Remote `Photo.path` and companion paths remain relative metadata, never local filesystem locations.

The in-place catalog migration adds nullable `source_device`, `source_root`, `source_volume`, `source_relative` columns and a composite unique partial index. Old rows, ids, decisions and JSON are preserved without rewriting. The existing unique SQL `path` column remains for local incremental-index performance; remote rows use a deterministic namespaced storage identity in that SQL column. Their serialized `Photo.path` stays relative. Independent rating/flag/label/Trash columns remain authoritative, and sync SQL never updates them.

`remote_cursors` stores terminal per-device/root revisions; `remote_stage` stores bounded streamed metadata batches. Each batch stages in one transaction. The final transaction publishes all metadata and tombstones plus the cursor together. Failed/cancelled streams discard their staging token and preserve the previous cursor. Full snapshots mark missing old rows unavailable. Tombstones retain ids, decisions and cached bytes. Orphaned staging from a process crash is invisible; it is never applied on restart.

Folder scopes and temporal/visual grouping include device/root identity. Remote photos use the same decision, undo, smart-collection and view logic as local photos. Agent dhashes are retained where supplied; remote libraries are excluded from the background whole-library hash scan so opening an offline or large network catalog does not transfer every preview.

All desktop media calls use source-aware Photo methods. Local index upserts, duplicate scanning, original exports, XMP exports and Reveal-in-Files reject remote sources with a clear explanation before opening any original or creating export output. This boundary also protects direct catalog/export/duplicate callers. Normal preview access never downloads original files, writes remote decisions, creates remote sidecars or invokes `read_range` as a fallback.

### Connection actors (`network.rs`, `service.rs`)

`hosts.json` beside the desktop catalog is written atomically and separately from photo metadata. It stores config, last hello/root identities, reported revisions and diagnostics. Each host has one bounded-command background actor that owns connect/disconnect/reconnect, exponential retry backoff, indexing and revision sync. The catalog actor remains free to commit local culling decisions while a host indexes. Progress is throttled; reliable terminal state/error updates remain observable. Indexing automatically syncs after the agent publishes its terminal revision. Operations are cancelled through protocol request ids and never automatically replayed after a disconnect.

`IoCommand` exposes `ConnectRemote`, `DisconnectRemote`, `IndexRemote`, `SyncRemote`, `CancelRemote`, and the `Remote(RemoteCommand)` configuration/control envelope. `IoEvent` exposes host state, provider attachment and removal. GPUI consumes snapshots through existing nonblocking slots; library/grouping rebuilds happen on catalog changes, never each frame. The existing incremental grouping cache, counters, palette cache and automation wake-ups remain in use.

### Preview provider (`media.rs`)

Remote Photo requests carry the complete source fingerprint through the existing bounded two-worker priority queue, pending-request deduplication, source serialization, memory admission, display reservations and texture pipeline. Fit/Full outrank thumbnails; selected and neighboring previews are enqueued ahead of viewport work, with Full read-ahead while zoomed. Selection/folder/viewport changes invalidate generations, cancel remote protocol requests, and discard stale results. A returned JPEG is decoded as already-oriented sRGB, preserving agent provenance before deriving smaller tiers. Full geometry comes from the actual JPEG, including paired/embedded previews; nonsensor Fit/Thumbnail geometry can use catalog dimensions until Full arrives. A remote source never reaches the local-file decoder or local original cache fallback.

The shared provider reads the persistent client JPEG cache before considering a live connection; a larger cached kind can satisfy a smaller request. Disconnected/unavailable uncached kinds produce an explicit offline error. Disk quota changes propagate to the shared client cache used by all host actors. Cache directories and filesystem operations are accessed only by background workers; UI media accessors retain their `try_lock` behavior.

## Desktop MCP tools and scripted verification

The existing authenticated desktop MCP bridge exposes these additional tools:

| Tool | Arguments | Result/behavior |
| --- | --- | --- |
| `remote_host_add` | `id`, `ssh_alias`, optional `display_name`, `remote_command`, `extra_ssh_args` | Persist and asynchronously connect/check hello |
| `remote_host_remove` | `host` | Disconnect/remove config, keep local library |
| `remote_hosts` | none | All host snapshots |
| `remote_status` | optional `host` | State, hello, roots, revision/progress/error |
| `remote_connect` / `remote_disconnect` | `host` | Explicit connection lifecycle |
| `remote_cancel` | `host` | Cancel active index/sync; retain prior cursor |
| `remote_roots` | `host` | Reported roots/volumes plus library scope strings |
| `remote_index` | `host`, `root` | Scan root and sync published metadata |
| `remote_sync` | `host`, `root`, optional `full` | Sync indexed metadata without filesystem scanning |

Mutations return acceptance, not completion. Poll `remote_status` for `connected` and a terminal root revision, then `library_list`. Errors are available both in status and in the UI diagnostic. Agent state and photo roots must be **different directories**, with agent state outside every photo root.

Example MCP JSON-RPC calls (substitute actual binary/state/photo paths first):

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"remote_host_add","arguments":{"id":"darkstar","display_name":"Darkstar","ssh_alias":"darkstar","remote_command":"<path>/shadowroom-remote serve --stdio --state-dir <agent-state-dir> --no-discover --root <photo-root-dir>"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"remote_connect","arguments":{"host":"darkstar"}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"remote_status","arguments":{"host":"darkstar"}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"remote_roots","arguments":{"host":"darkstar"}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"remote_index","arguments":{"host":"darkstar","root":"<id from remote_roots>"}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"filter","arguments":{"filter":"all","folder":"<scope from remote_roots>"}}}
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"library_list","arguments":{"scope":"filtered","offset":0,"limit":20}}}
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"select","arguments":{"index":0}}}
{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"remote_disconnect","arguments":{"host":"darkstar"}}}
```

To test without SSH, `remote_host_add` also accepts explicit `local_program` and `local_args` (trusted local child execution), for example the built `target/release/shadowroom-remote` and `["serve","--stdio","--no-discover","--state-dir","/absolute/agent-state","--root","/absolute/test-photos"]`. No local transport is selected implicitly.

The host form supports a blinking caret, arrows, Home/End, Shift selection, Delete/Backspace and Ctrl/Cmd+A/C/X/V. Tab switches fields.

Known limits: bootstrap/scp remains deferred and no upload occurs; mount changes require reconnect; use `remote_sync` with `full:true` to recover from a revision-ahead error (the cursor changes only after a successful full snapshot); source fingerprint guarantees remain size + millisecond mtime. Cache LRU counts published JPEG/JSON files, not transient atomic-write scratch files or OS filesystem overhead. MCP `photo_get` exposes the currently available `preview` with tier/dimensions/provenance and a separate `full_error`, so an available Fit does not hide a failed Full request.

## Verification

Use the remote round-trip and desktop-network tests for framing, identity,
cancellation, cache/offline behavior and source routing. Paired and embedded JPEG
Full can be tested without native RAW development. Testing the sensor fallback
requires LibRaw plus an actual sensor fixture. Check Full dimensions, source/wire notes, cache hits
after agent restart, offline provenance and explicit budget/payload failures.

GUI, real SSH and native-codec fidelity require separate native checks. See [release checks](RELEASING.md) and [performance measurements](PERFORMANCE.md).
