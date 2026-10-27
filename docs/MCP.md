# MCP control of the native workspace

Shadowroom exposes its running desktop through a genuine MCP stdio server. The agent's server subprocess is started with `shadowroom --mcp`; it connects to the existing app rather than opening a second workspace. The desktop remains the authority for selection, decisions, view state, imports, exports, preferences, and graceful quit.

## Connect an MCP client

Start the native Shadowroom app normally, then configure your MCP client:

```json
{
  "mcpServers": {
    "shadowroom": {
      "command": "shadowroom",
      "args": ["--mcp"]
    }
  }
}
```

`shadowroom` must resolve to the installed binary in that client's PATH. A local source build can instead use the absolute path to `target/release/shadowroom`. For a macOS app installed in Applications, use `/Applications/Shadowroom.app/Contents/MacOS/shadowroom` as the command. Pass `--mcp` directly to the executable, not to `open -a`.

Initialization and tool discovery succeed even when the desktop is not running. Calls that need the app then return an actionable MCP tool error asking you to open it. Start the app and retry the call; the bridge rereads the endpoint manifest each time, so restarting the desktop does not require restarting an already initialized MCP session.

The stdio process writes **only newline-delimited JSON-RPC messages to stdout**. Diagnostics belong on stderr. Closing stdin terminates that subprocess; this does not quit the desktop. The `quit` tool explicitly requests desktop shutdown.

## Tools

IDs refer to catalog photo IDs. Arrays accept at most 1,000 IDs per explicit call. Omitted IDs for decision tools mean the app's current selection. `library_list` returns a bounded page; retrieve additional pages by increasing `offset`.

| Tool | Arguments | Behavior |
| --- | --- | --- |
| `workspace_state` | `{}` | Current selection, view, inspector tab, overlays, filters, groups, job state, pending saves, and errors |
| `library_list` | `offset?`, `limit?`, `scope?` | Defaults: offset 0, limit 200, scope `filtered`; maximum limit 1,000; scope can be `all` |
| `photo_get` | `id` | Full metadata and decisions for one photo |
| `select` | `id?`, `ids?`, `index?`, `extend?` | Select by catalog IDs or zero-based filtered index; at least one selector is required |
| `rate` | `rating`, `ids?` | Set 0–5 stars; zero clears stars |
| `flag` | `flag`, `ids?` | Set `none`, `pick`, or `reject` |
| `label` | `label`, `ids?` | Set `none`, `red`, `yellow`, `green`, `blue`, or `purple` |
| `trash` | `trashed`, `ids?` | Always restorable app-only Trash, independent of UI Trash preference; false restores. Never moves/deletes files; inspect decision persistence. |
| `duplicates` | `action`, `ids?` | `scan`, `cancel`, or `status`; omitted scan IDs mean all nontrashed catalog photos. IDs are valid only for scan. |
| `undo` / `redo` | `{}` | Restore the app's decision history |
| `view` | `mode`, `panel?` | `grid`, `loupe`, `compare`, or `survey`; optional photo panel `review` or `info` |
| `zoom` | `mode`, optional `percent` | `fit`, `full` (100%), `toggle`, `in`, `out`, or `custom` with `percent` (12.5–800); prefers paired/embedded JPEGs |
| `pin` | `id?` | Pin the specified/current image as comparison reference |
| `filter` | `filter?`, `duplicates_only?`, `min_rating?`, `folder?`, `clear_folder?` | Filter is `all`, `picks`, `unrated`, `rejects`, or `trash`; stars 0–5. Exact duplicate review is optional. |
| `group` | `mode`, `gap_ms?`, `threshold?` | Modes `off`, `time`, `visual`, `burst`; gap 1–3,600,000 ms; dHash threshold 0–64 |
| `settings_get` | `{}` | Read current preferences |
| `settings_set` | `settings` | Apply a partial validated preferences object |
| `import` | `path` | Enqueue read-only folder/whole-drive indexing |
| `cancel_import` | `{}` | Cancel active/queued imports, preserving indexed records |
| `export_xmp` | `destination`, `ids?` | Enqueue selected-photo sidecar export |
| `export_picks` | `destination` | Enqueue copying picked originals and companions |
| `theme_import` | `path` | Import compatible Zed theme JSON |
| `cache_stats` | `{}` | Read queue, cache, decode, and preview-source file-read counters |
| `command` | `name` | Run one allowlisted workspace action below |
| `quit` | `{}` | Request graceful quit with pending-decision handling |

`command.name` is one of `next`, `previous`, `next_group`, `previous_group`, `next_unreviewed`, `previous_unreviewed`, `resolve_burst`, `toggle_histogram`, `toggle_inspector`, `toggle_sidebar`, `toggle_auto_advance`, `toggle_focus`, `select_all`, `refresh`, `retry_saves`, `retry_preview`, `toggle_histogram_log`, `toggle_compare_sync`, `show_duplicates`, `scan_duplicates_in_view`, `toggle_palette`, `toggle_preferences`, `open_folder_picker`, `open_theme_picker`, `show_appearance`, `show_rating_filter`, `reset_layout`, `resize_catalog`, `resize_inspector`, `close_overlay`, `getting_started`, `toggle_focus_peaking`, `show_full_folder`, or `reset_decision`. `show_appearance` opens Preferences directly to Appearance. `getting_started` opens the help guide, and `show_full_folder` leaves a selected Review subset. `reset_decision` clears stars and pick/reject together on the selected Catalog photos or the active Review/Compare photo, preserving labels and never auto-advancing; Undo restores the reset as one decision. Commands never accept shell text, executable names, or arbitrary code.

`workspace_state.duplicate_scan` reports job identity, phase, stage, counts, bytes read, cache hits, bounded exact-match groups and errors. Scans can be queued, running, completed, cancelled, failed, or stale. A new scan clears the old displayed results. A known primary/companion path or indexed source-revision change invalidates completed matches and marks them stale until rescanned. `scan_duplicates_in_view` follows the folder-scoped UI action and does nothing when it contains no active photos; `duplicates(action=scan)` with omitted IDs deliberately scans the catalog. `library_list(scope=all)` includes Trash for explicit inspection and restoration.

Cache counters describe the preview pipeline, not physical drive operations or
indexing header reads. Telemetry is sampled without blocking the UI; retry a
zero/contended sample before drawing conclusions about memory or activity.
`cache_stats.recent_errors` retains the last 32 background failures, with source
path/revision, requested preview tier, timestamp, and a bounded message. A null
snapshot means the diagnostic lock was busy; retry. The history is session-local
and survives successful retries, so it is evidence of earlier failures rather
than the current availability of a photograph.

`settings_set.settings` accepts the application's known preferences, including `interface_font`, `theme`, `theme_mode`, `theme_light`, `theme_dark`, `grouping_mode`, `burst_gap_ms`, `visual_threshold`, `auto_advance`, `auto_discover_drives`, `cache_budget_mb`, `memory_budget_mb`, `show_getting_started`, `trash_mode`, `last_photo_id`, `view_mode`, `show_inspector`, `min_rating`, and `filter`. Memory accepts **0 for Auto** or 64–65,536 MiB; local disk cache accepts 128–1,048,576 MiB. Auto uses a quarter of physical RAM bounded to 512 MiB–4 GiB. `theme_mode` is `manual`, `system`, or `zed`; the light/dark theme names select palettes for automatic modes. `trash_mode` is `system` (OS Trash, default) or `in_app`; changing it does not move files or change the app-only MCP `trash` tool. Unknown fields and invalid enum/range/type values produce tool errors before the UI receives a request. The explicit `settings_set` tool commits immediately; the Preferences dialog keeps a separate draft until Apply.

Opening Review or Compare from Catalog constrains navigation only when two or
more photos are selected. A singleton keeps the current collection available.
The batch-edit selection is cleared on entry, so a decision affects the current
frame. Automation uses its explicit selection, independently of mouse hover.
`workspace_state.review_subset` exposes an active subset. Returning to Grid
restores the Catalog selection; `command(name=show_full_folder)` releases the
subset. The UI's double-click and Show in review context action always open the
full current collection.

The UI defaults to OS Trash and offers optional in-app Trash in Culling
preferences. For compatibility, the MCP `trash` tool **always changes only the
catalog decision**; it never uses OS Trash or permanent deletion. Physical
deletion and its confirmation are available only in the app, and remote photos
cannot be physically deleted. There is no allowlisted `command` to confirm or
execute deletion. `workspace_state.deletion` exposes whether a confirmation or
operation is open, its frozen photo/file counts, sources, destination, mode, and
the last per-file outcome report. While it is open, mutations are refused; close
or finish it in the app before continuing. OS-trashed files must be restored
through the OS file manager, and physical deletion cannot be undone by MCP
`undo`.

Preferences **Apply** commits and keeps the pane open; **Apply & Close** commits
and closes it. Cancel/Escape only discards unapplied changes. These UI semantics
do not change the explicit, immediate `settings_set` behavior or XMP export
policy.

Import/export acceptance is asynchronous. Read `workspace_state` until the job completes and pending saves are clear; an accepted job is not a claim that files are already indexed or copied. Decision tools follow the desktop's save-status model. Tool timeouts are not a rollback guarantee: inspect workspace state before retrying non-idempotent actions such as navigation, undo, or export.

Successful tool calls preserve the app's full JSON result in both a serialized JSON text block and `structuredContent`. Object results are returned directly. A scalar, array, or null result is preserved in the text block and wrapped as `{"value": ...}` in `structuredContent`, which MCP requires to be an object. App errors use `isError: true`; malformed protocol requests, unknown tools, and unsupported protocol methods use JSON-RPC errors.

## Example conversation on stdio

Send one complete JSON object per line:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"example-agent","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"library_list","arguments":{"limit":100,"scope":"all"}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"workspace_state","arguments":{}}}
```

Use returned photo IDs in `select`, then call decision tools or provide an explicit ID array. Inspect the reported save state before ending a session that changes decisions.

## Local authentication and resource bounds

The desktop binds **127.0.0.1 on an ephemeral port** and generates a fresh 256-bit random token on every launch. A per-user manifest, `shadowroom/automation.json` under `directories::BaseDirs::config_dir()`, contains the address, token, process ID, and format version. On Linux this is normally `~/.config/shadowroom/automation.json`; on macOS it follows the user's Library configuration location. Unix manifests are created with mode **0600**. The stdio bridge rejects symlink/insecure manifests and non-loopback addresses. Do not copy the manifest or expose its token to other users.

Every IPC request authenticates before dispatch. The TCP endpoint accepts only the same known method names and validated arguments as MCP. There are four connection workers, at most 16 queued sockets, and at most 64 queued app requests. Each socket has read/write limits and a two-second absolute input-frame deadline, including clients that trickle individual bytes; incomplete requests cannot occupy a worker indefinitely. Input lines are capped at 1 MiB and desktop responses at 8 MiB; use smaller library pages if metadata exceeds the response bound. The app acknowledgment deadline is 20 seconds. Each queued request includes a unique request ID and an Instant deadline; the UI rejects expired requests before dispatch, so a mutation left in the queue cannot execute after its deadline.

One manifest identifies one active desktop endpoint per user. A private persistent `automation.lock` file holds a crash-released operating-system file lock for the server lifetime. A second live endpoint is refused. Manifest publication syncs a temporary private file, atomically renames it, and syncs the directory; readers never observe a partially written new manifest. A malformed private manifest from an interrupted older launch is preserved under `automation.invalid-<id>.json` and replaced under the lock. Insecure, symlink, or foreign-owner descriptors are refused. A manifest whose socket explicitly refuses connections is stale and can be replaced on the next launch. Normal server drop removes only its own token-matching manifest and stops accepting connections; it does not unlink a replacement process's manifest. A crash can leave a stale manifest behind. No background daemon, internet listener, browser bridge, raw HID interface, shell access, or arbitrary file-read/write tool is installed.

## Integration and checks

`start_server() -> Result<AutomationServer>` supplies `server.requests: Receiver<AutomationRequest>`. The app retains the server guard, polls requests on its UI thread, validates state-dependent conditions such as photo IDs, invokes the same commands used by the UI, and sends exactly one result through each request's reply channel:

```rust
// Integration shape; the desktop implements the stateful handler.
while let Ok(request) = server.requests.try_recv() {
    if request.is_expired() {
        let _ = request.reply.send(Err("Request expired before dispatch".into()));
        continue;
    }
    let result: Result<serde_json::Value, String> = handle_app_action(
        &request.method,
        request.params,
    );
    let _ = request.reply.send(result);
}
```

The desktop must check `request.is_expired()` immediately before dispatch. `request.request_id` identifies the command in diagnostics and acknowledgments. `run_mcp_stdio()` implements the subprocess side. The bridge module itself does not mutate photographs, spawn tools, or implement a second catalog. Backend handlers remain responsible for app-state validation, export destination restrictions, original-read-only guarantees, and save/quit acknowledgment semantics.

```bash
cargo test --locked --no-default-features automation::tests
```

Tests exercise protocol/version negotiation, notifications, JSON-only output, parse-error handling, complete nested results, input bounds and command allowlisting, real authenticated loopback dispatch against a fake app handler, owner-only manifest mode, second-instance rejection, unavailable-app errors, token-aware cleanup, partial-manifest recovery, absolute slow-trickle deadlines, and expired authenticated requests rejected by a fake handler. This validates the protocol/bridge independently; native UI integration must also be exercised against the running app.

The protocol follows the official [MCP lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle), [stdio transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports), and [tool protocol](https://modelcontextprotocol.io/specification/2025-11-25/server/tools). Supported negotiated versions are `2025-11-25`, `2025-06-18`, `2025-03-26`, and `2024-11-05`; a different requested version receives the newest supported version as allowed by negotiation. `ping` is supported before and after initialization; tools are available after initialization. No unimplemented resources, prompts, subscriptions, or task capabilities are advertised.

`command` with `name: "show_rating_filter"` opens or closes the rating menu. It refuses while an import, host, preferences, or command dialog is open. `workspace_state.overlays` reports the visible rating-filter, palette and preferences state. Use `filter.min_rating` to set a filter directly; repeated settings are idempotent.

On Linux, `open_folder_picker` opens the built-in photo-folder browser and `open_theme_picker` opens its JSON-file mode for an active preference draft. `workspace_state.folder_picker` reports the current directory, bounded entries, loading/error state and selection; it is null when closed. Filesystem reads run in background tasks. Escape or `close_overlay` cancels.
