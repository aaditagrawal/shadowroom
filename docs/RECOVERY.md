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
and destination files should be checked after such an interruption. Originals
remain read-only. Undo/redo history is limited to the current session; committed
decisions and the selected review position survive restart.

Session undo retains complete commands, with at most 1,000 undo entries and a
64 MiB target for the capacities of stored before/after decision snapshots across
undo and redo. The latest command remains undoable even if that command alone
exceeds the target. New edits release discarded redo snapshots before accounting
for the new command. This is a snapshot budget, not a process-memory limit.

RAW added after a previously reviewed matching JPEG promotes that existing
catalog row in place, retaining its review ID and decisions. If both files
already have separate catalog identities, both are kept independently. This
avoids invalidating queued decisions and session undo history.
