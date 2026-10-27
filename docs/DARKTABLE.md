# darktable reference for review and deletion

Checked on 2026-10-27 against the official **5.6 manual** and source pinned to
**release-5.6.1**, the current stable release. These findings describe darktable;
the recommendations below describe Shadowroom design choices, not verified
feature support. No development-version behavior is used.
[Stable release announcement](https://www.darktable.org/2026/08/darktable-5.6.1-released/).

## Review, selection, and hover

In darktable's fixed culling mode, multiple selected images constrain navigation
to that selection. With zero or one image selected, navigation covers the entire
current collection. Dynamic culling displays the selection together, with a
fallback when there are fewer than two selected images. Zoom and pan are
synchronized by default; Shift allows manipulating only one image.
[Culling manual](https://docs.darktable.org/usermanual/5.6/en/lighttable/lighttable-modes/culling/).

The grid distinguishes hover from explicit selection. A hovered thumbnail is the
implicit target of shortcuts; some shortcuts operate on the whole selection when
that thumbnail belongs to it. Enter selects a thumbnail in darktable; it does not
mean Shadowroom's requested Enter-to-Review action.
[Filemanager manual](https://docs.darktable.org/usermanual/5.6/en/lighttable/lighttable-modes/filemanager/).
Double-clicking a grid thumbnail opens darkroom, the editor, in the pinned source.
[Thumbnail event handler](https://github.com/darktable-org/darktable/blob/release-5.6.1/src/dtgtk/thumbtable.c#L1415-L1456).

**Shadowroom guidance:** adopt the selection rule for keyboard Review entry, while
keeping the requested Enter/L bindings. A single image opens the current folder
with that image focused; multiple selected images constrain Review. Double-click
opens the full folder at the clicked image. This follows the owner's instruction
rather than copying darktable's editor shortcut.

## Image actions

darktable presents file and metadata actions in its right sidebar's **actions on
selection** module: remove, delete/trash, move, copy, duplicate, rotate, local
copies, grouping, and metadata operations. Its documentation describes that panel;
the inspected stock grid event handler does not implement a thumbnail action
context menu. This is a source finding, not a claim about every Lua extension.
[Actions manual](https://docs.darktable.org/usermanual/5.6/en/module-reference/utility-modules/lighttable/selected-image/),
[sidebar implementation](https://github.com/darktable-org/darktable/blob/release-5.6.1/src/libs/image.c#L490-L598).

**Shadowroom guidance:** the requested right-click menu is useful independently of
darktable's layout. Include Show in folder, Show in Review, Survey burst, existing
decision actions, and explicitly named deletion actions. Make the target scope
visible when several photos are selected.

## OS Trash, library removal, and failures

darktable separates library-only removal from disk deletion. Removal leaves the
image and XMP files on disk. Deletion removes the library entry and sidecars; it
retains a source image while another editing version still uses it.
[Actions manual](https://docs.darktable.org/usermanual/5.6/en/module-reference/utility-modules/lighttable/selected-image/).

Its defaults enable **system Trash**, confirmation before disk deletion, and
confirmation before library-only removal. The preferences allow permanent deletion
instead of attempting Trash; this is not an in-app holding area.
[Security manual](https://docs.darktable.org/usermanual/5.6/en/preferences-settings/security/).

The pinned deletion implementation:

- Captures the target image list before the confirmation; its initial dialog gives
  an image count and identifies Trash versus permanent deletion, without listing
  source folders or the final Trash location.
- Calls the platform Trash implementation (`g_file_trash` on Linux).
- On failure, shows the filename/error and offers permanent deletion, library-only
  removal, skip, or abort, with an optional apply-to-all choice. **Abort is the
  default; Trash failure never silently authorizes permanent deletion.**
- Processes source and sidecars sequentially. A source can already have moved
  before a later sidecar fails; this is not an atomic grouped transaction.

[Deletion and failure handling](https://github.com/darktable-org/darktable/blob/release-5.6.1/src/control/jobs/control_jobs.c#L1012-L1377),
[initial confirmation](https://github.com/darktable-org/darktable/blob/release-5.6.1/src/control/jobs/control_jobs.c#L2118-L2150).
The underlying Linux API explicitly permits an unsupported-Trash error for some
mounts; OS Trash availability cannot be assumed on every drive.
[GIO API](https://docs.gtk.org/gio/method.File.trash.html).

darktable automatically groups same-basename RAW/JPEG files and editing duplicates
for display. An editing duplicate is another XMP history using the same source,
not a second physical RAW. These concepts differ from Shadowroom's logical
RAW-plus-companion asset and must not be confused when counting/deleting files.
[Grouping manual](https://docs.darktable.org/usermanual/5.6/en/lighttable/digital-asset-management/grouping/).

**Shadowroom guidance:** default to OS Trash, retain optional app-only Trash, and
make Shift+Delete a separately confirmed permanent action. Show both photo and
physical-file counts, source locations, and the applicable destination/recovery
route. Preserve RAW/companion/sidecar scope and report partial results. Never fall
back to permanent deletion because Trash is unavailable. These confirmations are
deliberately more specific than darktable's initial count-only dialog.

## Preferences: Apply and closing

The manual describes closing preferences with the window close control or Escape;
there is no global Apply/Cancel workflow. In the pinned implementation, ordinary
generated controls write their configuration values on the dialog-close response.
Some bespoke theme, font, and CSS controls take effect immediately. Therefore,
“darktable saves every setting immediately” would be inaccurate.
[Preferences overview](https://docs.darktable.org/usermanual/5.6/en/preferences-settings/overview/),
[generated close/change callbacks](https://github.com/darktable-org/darktable/blob/release-5.6.1/tools/generate_prefs.xsl#L205-L219),
[generated preference writes](https://github.com/darktable-org/darktable/blob/release-5.6.1/tools/generate_prefs.xsl#L319-L345),
[bespoke controls and dialog](https://github.com/darktable-org/darktable/blob/release-5.6.1/src/gui/preferences.c#L157-L184).

**Shadowroom guidance:** preserve explicit draft/Apply/Cancel safety. Apply can
commit while leaving preferences open; a separately named Apply & Close action
removes ambiguity for users who want both. darktable's behavior is reference
material, not a reason to restore implicit saving.

## Where photo work is stored

darktable leaves original RAW bytes alone, stores image information and editing
history in adjacent XMP sidecars, and also keeps image data in its library
database for fast access. After import, its database takes precedence over XMP;
external XMP changes require explicit reconciliation. Custom sort order is one
documented exception that is not stored in sidecars. Thus darktable is not a
stateless file browser.
[Sidecar manual](https://docs.darktable.org/usermanual/5.6/en/overview/sidecar-files/sidecar/).

The default is to create XMP on import and subsequently update it with edits. The
storage preferences also offer after-edit and never, and an optional startup scan
for externally changed sidecars. Reimporting backed-up originals plus XMP can
reconstruct the saved image work, while a database backup preserves additional
application state.
[Storage manual](https://docs.darktable.org/usermanual/5.6/en/preferences-settings/storage/).

**Shadowroom guidance:** keep files authoritative for physical location and OS
Trash status, keep caches rebuildable, and make portable decision exports clear.
Retain explicit metadata writes and original-byte safety. Any future automatic
adjacent-XMP synchronization needs a stated policy for external edits and failed
writes; it should not be inferred from this comparison or enabled silently.
