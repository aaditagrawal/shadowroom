# Decision durability and closing

Ratings, flags and labels are optimistic until the catalog actor acknowledges
their SQLite FULL-synchronized transaction. The footer and MCP state distinguish
pending, failed and durable decisions. Repeated values have distinct revisions;
an old acknowledgement cannot clear a newer decision. Failed values remain
dirty and the command palette exposes **Retry unsaved decisions**.

Preferences use a synced temporary file and same-directory rename. On Unix,
success also requires syncing the containing directory and its ancestors. A
relative nested output includes `.` in that chain. Directory-sync errors are
reported even though the new file may already be visible; preferences remain
pending. Every retry revisits the ancestor chain, including directories created
by an earlier failed attempt. Directory existence alone is not a durability
acknowledgement. These error-injection regressions do not simulate power loss or
certify all filesystem/hardware persistence behavior.

Normal window close, application-menu Quit, Cmd/Ctrl+Q and MCP quit all enter the
same asynchronous close gate. It stops admitting new mutations, drains pending
decisions/preferences, waits for accepted export jobs to reach a terminal state,
then checks a catalog/preferences flush acknowledgement. A failed decision or
flush leaves the window open. The export worker is joined during service
shutdown, after accepted active and queued jobs finish.

GPUI 0.2.2's macOS OS-termination observer runs too late to veto termination.
Before its fallback shutdown request, Shadowroom atomically writes and syncs
`pending-decisions.json` in its local application-data directory whenever the
workspace has pending/failed decisions or preferences. It contains the latest
value for every dirty photo, including commands still in the UI outbox. This
journal is independent of the catalog's SQLite writer lock.

On launch, journal decisions are staged through the normal catalog writer and
preferences writer. New review decisions wait until recovery succeeds. Failed
replay remains visibly unsaved and can be retried. The journal is removed only
after durable acknowledgements for both decisions and preferences. Malformed
journals are retained for repair and block new decisions instead of being
silently discarded or overwritten.

A native workspace acquires a crash-released exclusive catalog lease before
reading recovery files or starting any actor. A second writable instance for the
same application-data directory exits with an explanation; MCP clients attach
to the existing instance. If normal settings are malformed, recovered settings
remain a separate pending obligation. Repair the settings file and run **Retry
unsaved decisions**; the recovery payload is retained until a preferences ACK.
When restoring/replacing a catalog backup manually, preserve and review its
matching recovery journal together. Journal IDs are scoped to that catalog;
automatic reconciliation with an unrelated replacement database is unsupported.

This fallback requires successful local filesystem writes. It is not a journal
for an unannounced process kill, power failure before the observer runs, or a
machine with no writable storage. Normal quit waits for exports; forcibly ending
the process or irrevocable OS termination can interrupt a copy. Export manifests
and destination files should be checked after such an interruption. Indexing,
rating, copying, and exporting leave original bytes unchanged. Explicitly
confirmed file deletion is a separate operation, described below. Undo/redo
history is limited to the current session; committed
decisions and the selected review position survive restart.

## Explicit file deletion

Delete defaults to **OS Trash**; **Preferences → Culling → Trash destination**
also offers **In-app**, which leaves files in place. In-app Trash remains a
normal catalog decision with acknowledgement, retry, session Undo/Redo, and
decision-journal recovery. The in-app Trash collection does not display the
operating system's Trash contents.

OS Trash and Shift+Delete permanent deletion use a frozen plan confirmed in the
UI, with photo/file counts, source locations, and destination. They wait for
pending decisions/recovery and conflicting background work. They are **not
decision-journal entries** and cannot be reversed by replaying a decision or
pressing Undo. A successful file removal clears session decision history. Restore
OS-trashed files through the operating system; permanent deletion has no
Shadowroom recovery path. After OS restoration, refresh or reopen the folder;
reindexing does not recover unexported decisions from a deleted catalog record.
Use explicit XMP export for a portable copy of decisions, or in-app Trash to
retain the existing catalog work. MCP's `trash` tool remains app-only.

Each file and parent folder is rechecked against the confirmed plan. Operations
stop on failure, with no permanent-deletion fallback for unavailable OS Trash.
Some files may already have moved or been removed; the visible error and
`workspace_state.deletion.report` distinguish completed, failed, and not-attempted
members. Catalog reconciliation only follows completed removals. If that step
fails, refresh the source folder as directed by the error. Normal quit waits for
an executing deletion to finish, but an unannounced process kill or power loss
can interrupt it; inspect source folders and OS Trash before continuing. See
[Trash, deletion, and exact duplicates](DUPLICATES_TRASH.md) for file scope.

## Session history

Session undo retains complete commands, with at most 1,000 undo entries and a
64 MiB target for the capacities of stored before/after decision snapshots across
undo and redo. The latest command remains undoable even if that command alone
exceeds the target. New edits release discarded redo snapshots before accounting
for the new command. This is a snapshot budget, not a process-memory limit.

RAW added after a previously reviewed matching JPEG promotes that existing
catalog row in place, retaining its review ID and decisions. If both files
already have separate catalog identities, both are kept independently. This
avoids invalidating queued decisions and session undo history.
