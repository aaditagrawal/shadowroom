# Background catalog service

`service::start_io(data_dir, initial_settings, initial_import)` returns four handles:

```rust
(
    crossbeam_channel::Sender<IoCommand>,
    crossbeam_channel::Receiver<IoEvent>,
    PhotoSlot, // Arc<Mutex<Option<Vec<Photo>>>>
    GroupSlot, // Arc<Mutex<Option<Vec<Vec<i64>>>>>
)
```

The module has no GPUI dependency. The desktop owns optimistic local interaction and polls worker results. SQLite operations, photograph sorting, grouping, drive discovery and export filesystem work run on background threads.

## UI consumption

Drain lightweight events first. `Saved` resolves one successful `Decisions` command, independent of how many photographs it contained. `SaveFailed(message)` resolves one failed command and carries the error. Both are reliable; decrement the pending-save count for either. Acknowledged commits use the catalog's durable decision policy.

Consume at most one photo slot and one grouping slot each UI poll using `try_lock`, taking the owned value only if the lock is available. A new publication **replaces** an unconsumed snapshot; the service never queues full-library vectors in events. Photographs are already sorted by capture timestamp (mtime fallback), path and ID. The sort comparator borrows paths instead of cloning keys. Do not sort again on the UI thread.

While optimistic decisions remain pending, defer photo-slot consumption. A photograph snapshot can have been produced before a UI decision reached the writer. The writer invalidates any outstanding snapshot before emitting the corresponding acknowledgement; draining acknowledgements before consuming a snapshot therefore avoids reverting a local decision. On a failed decision, the next catalog publication restores persisted state and the UI should display the failure.

Index progress publishes at most once per second, plus one final publication after each finished/cancelled import. Explicit refresh requests are coalesced. Decision updates invalidate stale snapshots and request a later fresh publication instead of immediately issuing a full-library query for every keypress.

## Work isolation and bounds

The catalog actor accepts up to 1,024 queued commands in FIFO order. It processes finite command/event batches so settings and index progress cannot starve. Callers can use `try_send` for optional hash/refresh commands; user decisions require reliable delivery or a visible failure.

Exports have a separate thread with one active export and one queued export. The actor captures an `ExportPlan` using catalog metadata before dispatch. The plan owns its decision/source snapshot. Destination validation, directory creation, original copying, XMP writing and output syncs occur on the export worker without holding a catalog connection. A full export queue reports an error rather than blocking the decision writer. Settings, ratings, flags, labels and explicit Flush barriers continue while an export is slow. Shutdown completion waits for accepted exports to finish. Only metadata snapshot preparation remains on the actor.

Automation callers can submit `ExportTracked { id, destination, picks, ids }` with their job ID. Reliable `ExportJob { id, state, completed, error }` events report `queued`, `running`, then `completed` or `failed`. The queued event precedes dispatch, so a fast export cannot regress from completed back to queued. A full queue or preparation error emits failed with zero completed files; a partial export reads the machine-readable `catalog::ExportFailure` count and includes the manifest path in its error. Copy-picks counts include verified media and existing sidecars, excluding manifests. The original `Export(destination, picks, ids)` remains available for UI callers without explicit job tracking. Job events describe this live service session; callers retain their job registry and final outcomes.

Grouping has its own thread, a single replaceable input snapshot and a single replaceable output. Inputs contain only the photo fields grouping uses; duplicate future hash updates coalesce by photo ID. A 350 ms debounce groups hashes/settings changes together. A revision check discards obsolete computation results. `IoCommand::Settings` detects changes to grouping mode, burst gap and visual distance; `Regroup` explicitly requests the current configuration. Grouping off produces an empty group list. The UI builds its ID-to-group lookup from the resulting ID lists.

Drive discovery runs independently and replaces one latest drive list every five seconds. Its filesystem enumeration cannot monopolize the decision writer. Automatic imports are queued only for newly observed removable drives with photo folders; enabling automatic discovery clears the observed set so existing drives become eligible on the next scan. `CancelImport` clears queued imports and requests cancellation of the active indexer; cancellation completes asynchronously and publishes its final state.

Status, progress, drive and busy telemetry is coalesced or capped at 128 queued lightweight events. Decision acknowledgements, decision errors and critical export/startup/shutdown errors use a reliable lightweight lane and can exceed this telemetry cap. They deliberately cannot block the writer waiting for the UI to drain events. The event queue contains no full-library photo or group snapshots. A UI that sends decisions indefinitely without consuming acknowledgements can accumulate small acknowledgement events; this does not accumulate media or full metadata vectors.

## Shutdown barrier

For asynchronous pre-quit handling, first send `IoCommand::Flush(ack_sender)`. This is a FIFO barrier with the same pending-preferences-save and catalog-checkpoint error semantics, while keeping the service, indexing and export workers alive. A successful acknowledgement confirms all earlier decisions, coalesced hashes and pending preferences reached the flush boundary; later commands continue to work. Intercept window close, await this acknowledgement off the UI thread, and only then request application quit. GPUI's short application-quit hook deadline is insufficient for performing the initial flush itself.

Send `IoCommand::Shutdown(ack_sender)` after the final settings/decision commands. The command is a FIFO barrier: earlier decisions have completed before shutdown begins. The service requests indexing cancellation, waits for that indexer to stop, saves pending debounced settings, and calls `Catalog::flush()` to checkpoint the catalog. It sends `()` only after successful preference/catalog flushing and graceful export-worker termination. Every accepted export, including untracked UI exports and the queued export, reaches its completed/failed result before acknowledgement. Terminal export events are sent before the private completion signal, and the catalog actor joins the export thread before acknowledging Shutdown, so that worker cannot keep writing after a successful close. New exports after a successful Shutdown request are rejected with a reliable failure. While waiting for existing exports, the actor continues saving decisions, hashes and preferences and servicing explicit Flush requests. Repeated Shutdown callers share the eventual completion. A preference/catalog preflush failure emits its error, drops that caller's acknowledgement, and keeps pending work and the actor alive for retry; treat a disconnected/expired acknowledgement as a visible failure, not a successful save.

The application quit handler must await the acknowledgement with a finite deadline before exiting, and surface a timeout or error. Filesystem operations already in progress cannot be forcibly preempted; a disconnected/hung storage device can exceed that deadline. Flush completion is independent of export completion. Shutdown completion is deliberately different: accepted queued exports drain and the active export is awaited and joined. An expired wait must leave the application open or show an explicit failure; it does not authorize exiting while writes continue. An incomplete export never authorizes deleting or altering an original.

Settings parse-error handling belongs to the desktop: do not send replacement settings after loading a malformed file unless it has been backed up or the user explicitly replaces it. An untouched initial preference set is not rewritten at shutdown.

## Validation

Service tests cover decision and preference persistence while an export worker is deliberately blocked, Shutdown deferral through both the active and queued export, worker exit before acknowledgement, FIFO flushing without consuming acknowledgement events, a standalone Flush barrier that saves pending preferences and leaves the actor responsive, replacement of obsolete photo snapshots, sorting before UI delivery, and latest-only background grouping with coalesced hash updates. Tests do not simulate power loss, a stuck kernel filesystem call, vendor hardware, or a terminated external export destination.
