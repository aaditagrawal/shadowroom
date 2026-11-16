# Themes, fonts and preferences

Shadowroom includes restrained **Shadowroom Dark** and **Shadowroom Light** palettes. Image canvases are neutral; color conveys selections, picks, rejections and ratings. Imported Zed theme families can provide the same application colors.

## Appearance controls

Click the active theme name in the status bar, open **Preferences → Appearance**
with **Cmd/Ctrl+,**, or search **Appearance & Themes** in **Cmd/Ctrl+K**. Appearance
is the first preferences section. Culling, Performance, Controller and Help have their
own sections so a large theme collection does not bury other settings.

Preferences use a separate draft. Choices do not change the running app or write
the settings file until you click **Apply** or **Apply & Close**. **Apply** saves
and leaves preferences open; **Apply & Close** (the default, Return) saves and
closes them. The footer counts unsaved changes, each edited section shows a dot
on its tab, and **Apply** is disabled while nothing has changed. **Cancel**
discards the unapplied changes at once. With unsaved changes, Escape, the close
button, Cmd/Ctrl+, or clicking outside first asks **Discard N changes?** with
**Keep Editing** as the default; without changes they simply close. Switching
sections keeps the draft. Imported theme files are staged and installed only
when applied. macOS names the dialog **Settings**; Linux keeps **Preferences**.

Preferences work entirely from the keyboard. Tab and Shift+Tab move through the
section tabs, the controls and the footer buttons; a 2 px focus ring
(`shadowroom.focus_ring`, defaulting to `border.focused`) appears only after
keyboard navigation. Each choice group and the theme list is one Tab stop:
arrows choose the neighbouring option. Space activates the focused control,
Return activates a focused button and is otherwise **Apply & Close**,
Ctrl+Tab or Ctrl+PageDown/PageUp switches sections, and Cmd/Ctrl+S applies.
Grouping options that do not apply (Burst gap with Grouping Off, Similarity
with Off or Time) are dimmed and inert.

The theme list groups Dark and Light palettes, with background/accent/text
swatches and a separate bounded scroll area. Installed Zed themes are discovered
automatically. **Import Zed theme…** accepts a Zed color-theme family JSON/JSONC
file and stages it in Manual mode. Apply saves a local copy and activates it.
The explicit MCP `theme_import` tool installs and activates its requested family
immediately.

- **Manual:** selecting a theme changes the draft’s chosen theme.
- **System:** the panel shows the selected light/dark pair. Selecting a theme
  updates its matching appearance slot while staying in System mode.
- **Follow Zed:** follows Zed's selected color theme. The panel explains when its
  theme file or settings are unavailable. Selecting a theme explicitly switches
  to Manual.

The selected palette is named in Preferences; the status bar retains the active palette until Apply. Theme source
can also be changed through the palette's **Theme Source** commands.

### Motion

Menus, dialogs and the command palette fade in over 120 ms, starting at 30%
opacity so they are readable on the first frame. Status-bar notices, the error
toast and the trailing decision note fade in over 150 ms; a notice that replaces
one already showing appears at once, so rapid decisions do not flicker. The
on-photo decision chip settles in from 60% over 120 ms when a photo on screen
gets its first decision (with automatic advance off); later changes update it
at once. Navigation, photo changes, zoom and pan are never animated, and
everything closes instantly.

**Preferences → Appearance → Reduce motion** turns these fades off. Fades are
also off when the system asks for reduced motion: on Linux, the desktop portal's
`reduced-motion` setting or GNOME's **Animations** switch (read through the
portal, else `gsettings`); on macOS, **Accessibility → Display → Reduce motion**.
Shadowroom reads the system setting at startup and whenever Preferences opens.
`SHADOWROOM_REDUCE_MOTION=1` (or `0`) overrides both, for measurements.

### Built-in palettes

Both palettes are defined as theme roles in `src/theme.rs`; render code reads
roles, not hex values.

| Role | Shadowroom Dark | Shadowroom Light | Used for |
| --- | --- | --- | --- |
| `background`, `shadowroom.canvas` | `#0f0f0f`, `#0f0f0f` | `#f1f2f4`, `#dcdcdc` | window and grid background; image canvas (neutral, R=G=B) |
| `panel.background` (title, toolbar, status bars) | `#181818` | `#f8f9fb` | chrome |
| `elevated_surface.background` | `#202020` | `#ffffff` | dialogs, menus, palette, tooltips |
| `shadowroom.well` | `#161616` | `#e4e4e4` | thumbnail image wells in grid and filmstrip |
| `text` / `text.muted` / `text.disabled` | `#e6e6e6` / `#9a9a9a` / `#666666` | `#292e37` / `#566070` / `#8992a0` | copy |
| `text.accent`, `border.focused` | `#5b9bd5` | `#245c91`, `#3979b4` | the single accent (see below) |
| `border` / `border.variant` | `#292929` / `#333333` | `#cbd1da` / `#dce0e7` | decorative hairlines between regions; `border.variant` also outlines segmented controls and keycaps, whose state is shown by the chosen segment's bar |
| `shadowroom.control_border` | `#707070` | `#717a88` | text-field outlines and the empty label swatch: inputs with no other boundary, 3:1 against every surface |
| `ghost_element.hover` / `ghost_element.selected` | `#ffffff08` / `#ffffff12` | `#1728420a` / `#3979b418` | translucent hover and selection fills |
| `shadowroom.pick` / `reject` / `rating` | `#7fbf7f` / `#d46a6a` / `#e0b341` | `#2f6844` / `#a53e42` / `#765612` | decisions on chrome |
| `shadowroom.label.{red,yellow,green,blue,purple}` | `#e0918a` `#d9bd72` `#86bfa8` `#82abe0` `#c497d0` | `#b3362e` `#7e6200` `#2a7560` `#2560b0` `#8a3fa6` | inspector label swatches and thumbnail label strips (3:1 on panels, wells and the filmstrip canvas); `shadowroom.label.text` (`#0f0f0f` / `#ffffff`) draws each swatch's key (6-9) |
| `shadowroom.histogram.background` / `shadowroom.histogram.grid` | `#0f0f0f` / `#1c1c1c` | `#ffffff` / `#00000014` | histogram plot plate and quarter-tone guides; a light plate switches the curves to darker inks |

State ladder: hover is a fill only and always weaker than selection, on panels,
dialogs and the canvas alike, which is why the ghost fills are translucent.
Selection adds a second cue: the accent bar or underline for a location, a 2 px
text-coloured bar on a chosen segment, or full-strength text and MEDIUM weight.

The accent means "you are here": the active view tab and inspector tab
underline, the current collection's bar, the current photo's outline, the
command palette's selected row, and keyboard focus. Toggles (panels,
histogram, zoom, focus peaking, grouping) show "on" as a quiet filled state with
a full-strength icon instead.

**Photo-overlay roles** (`shadowroom.scrim`, `shadowroom.scrim.{text,muted,pick,reject,rating,accent}`
and `shadowroom.scrim.label.*`) are identical in both palettes: thumbnail
badges and compare captions sit on photographs, which can be white or black in
either appearance, so they use a dark `#0f0f0fd0` plate with colours that keep
3:1 (glyphs) or 4.5:1 (text) over the plate on a white photo. A thumbnail's
flag glyph and label chip share one plate; the chip is filled with
`shadowroom.scrim.label.*` and carries the label's initial (R, Y, G, B, P) in
`shadowroom.scrim.label.text` (`#0f0f0f`, at least 7:1 on every fill).

The **cull ledger** draws on chrome, not photographs: `shadowroom.pick` and
`shadowroom.reject` ticks, `icon.muted` for reviewed photos without a flag,
`border` as the unreviewed rail, and `text.accent` only for the current
photo's playhead.

Imported Zed themes never define Shadowroom roles. `shadowroom.well` and
`shadowroom.control_border` are derived from the theme's own background, panel
and muted-text roles (`Theme::derived_color`); other Shadowroom roles use the
built-in palette of the same appearance. Colours a theme supplies itself always
win, and derivation never rewrites the imported theme.

## Zed import

`Theme::from_zed_json(&str)` returns every theme in a Zed family, and also accepts a single theme object or an array. Standard JSON and JSONC with line/block comments, trailing commas and an optional UTF-8 BOM are supported. Family name, author, theme metadata and **all style keys** are retained, including unknown keys, null optional colors, syntax styles, players, accents, arrays and non-color values. The `style` map is lossless for style data. The `colors` map exposes top-level strings for UI use.

`color(key, fallback)` uses the fallback for an absent, null, or invalid color. `resolved_color(key)` inherits the built-in palette matching the imported theme's appearance. `color_rgba` returns explicit channels. Hex formats `#RGB`, `#RGBA`, `#RRGGBB`, and `#RRGGBBAA` and `transparent` are recognized; alpha is always the last channel and is preserved. Unsupported CSS color functions remain in `style` but use the UI fallback. `Rgba::composite_over` is available when a renderer needs an opaque color.

Zed has style fields for programming editor features that a photo culler does not display. Those values survive import without inventing equivalent photo controls. Shadowroom maps shared surface, text, border, element, status and editor-background colors to the corresponding UI surfaces. An imported theme is app chrome; it does not transform photographs or affect image color management.

## Discovery

`discover_themes()` returns built-ins plus locally installed themes and a list of diagnostic strings. It never downloads or runs extension code. Files are sorted, duplicate physical paths are read once, and later roots replace a same-name theme. Shadowroom local themes have precedence over Zed themes.

Locations include:

- Shadowroom: `$XDG_CONFIG_HOME/shadowroom/themes` or `~/.config/shadowroom/themes`.
- Zed: `$XDG_CONFIG_HOME/zed/themes` and `~/.config/zed/themes` (including macOS), matching [Zed's local-theme documentation](https://zed.dev/docs/themes).
- Linux Zed extensions: `$XDG_DATA_HOME/zed/extensions/installed` or `~/.local/share/zed/extensions/installed`, plus extension development folders.
- macOS Zed extensions: `~/Library/Application Support/zed/extensions/installed`.
- Windows Zed: `%APPDATA%/Zed/themes`, `%LOCALAPPDATA%/Zed/extensions/installed`.
- Zed preview and development application directories are also checked.

Only `.json` and `.jsonc` files inside theme directories are imported; extension manifests and language configuration are ignored. Traversal is bounded to eight directory levels without following symbolic links in discovered children. Files over 16 MiB and malformed themes are reported. An explicit `discover_themes_in(&[PathBuf])` API supports alternative installations and tests. `read_theme_file(&Path)` imports a selected file.

## Live reload and Zed selection

`theme_watch::start_theme_watch()` returns a guard and a bounded receiver containing the latest `ThemeUpdate { themes, diagnostics, zed_theme }`. Keep the guard in the workspace and consume updates off the existing UI poll. Dropping the guard signals the worker to stop; it does not block the UI while joining a filesystem operation. The first update is asynchronous and subsequent checks occur every two seconds.

The watcher initially indexes configured theme directories. Later polls inspect cached directory/file metadata and settings metadata only: unchanged directories are not listed again, and unchanged JSON is not reopened or parsed. Newly changed directories update the cached graph, while only changed files are reparsed. Extension traversal skips deep non-theme trees. One pending update replaces an older unconsumed update; it cannot accumulate full theme families. Malformed edits report a diagnostic and retain the last valid family/settings until a valid edit arrives. Removing a file removes its themes. A metadata-preserving rewrite on a filesystem that does not expose a changed timestamp/identity cannot be detected by metadata polling; Unix fingerprints also include inode and change time.

`ThemeWatcher::start_with_paths(ThemeWatchPaths { theme_roots, zed_settings }, interval)` supports explicit installations and test fixtures; intervals are bounded between 20 ms and two seconds. Theme-root order retains discovery precedence. Settings candidates prefer the configured Zed directory, then `~/.config/zed/settings.json`, then the Windows Zed configuration directory.

Zed's [documented settings](https://zed.dev/docs/themes) accept a fixed theme name or an object with mode, light and dark choices. `parse_zed_settings` accepts either form as JSONC. `ZedSelection::selected_name(system_dark)` resolves a fixed name or the appropriate appearance without making operating-system calls. The desktop supplies its window's current appearance. Named `theme_overrides` deep-merge style objects, preserving unspecified syntax attributes; arrays replace arrays and explicit nulls remain in the raw style map. Rebuilding always starts from imported theme data so repeated reloads do not stack old overrides.

The desktop stores its choice in preferences as `theme_mode` (`manual`, `system`, or `zed`), with `theme_light` and `theme_dark` for appearance-specific choices. Manual uses the chosen family; system follows the current window appearance; Zed follows the parsed Zed selection. Built-in themes embedded inside the Zed binary, such as One Dark, may not have a corresponding installed JSON file. The watcher reports a missing selected name explicitly; import its family to follow it exactly. It does not relabel an approximate palette as that theme.

## Role fallbacks and contrast

Dialogs share one frame: an elevated surface with a 13 px title bar, a single
modal scrim `shadowroom.scrim.modal` (dark `#00000099`, light `#1f2a3a59`) drawn
only behind the topmost dialog so nested dialogs never darken twice, and one
button hierarchy: an accent-filled default, outlined secondary actions, and
error-coloured destructive actions (filled only when irreversible).

Built-in appearance fallbacks cover canonical Zed text/icon/placeholder/disabled/accent roles, main/panel/elevated/title/status/toolbar/tab surfaces, active and disabled elements, ghost elements, focused borders, search/drop highlights, scrollbars and semantic status backgrounds/borders. Invalid present colors use the same fallback path as absent/null colors. Valid external colors and alpha retain their original values.

`Theme::contrast_ratio(foreground_key, background_key)` reports explicitly supplied roles, compositing foreground/background transparency over the theme's canvas. Missing or unsupported role colors return `None`. `accessibility_diagnostics()` flags sparse required roles, invalid color values and low contrast for ordinary text and meaningful icons. It uses the [W3C contrast guidance](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html): 4.5:1 for ordinary text; 3:1 for meaningful non-text controls. Disabled text is excluded. Diagnostics are advisory and never recolor an imported theme; preserving the user's chosen theme remains the default. Alpha-aware ratios, black/white's 21:1 ratio, sparse themes, nested overrides, real-file reload and unchanged metadata polling are covered by tests.

## Fonts

The app bundles original open-source font binaries under `assets/fonts`; it does not require system font installation. **Geist Mono** is the default interface font for new workspaces, with **DM Mono** and **DM Sans** as bundled alternatives. Existing saved font choices are retained. Settings may also name an installed system font. Select the family name, not a filename.

| Family | Bundled font | License |
| --- | --- | --- |
| DM Sans | `dmsans/DMSans[opsz,wght].ttf` (variable) | `dmsans/OFL.txt` |
| Geist Mono | `geistmono/GeistMono[wght].ttf` (variable) | `geistmono/OFL.txt` |
| DM Mono | `dmmono/DMMono-Regular.ttf`, `DMMono-Medium.ttf` | `dmmono/OFL.txt` |

The UI registers bundled bytes with GPUI at startup. The variable fonts include multiple weights. `assets/fonts/README.md` records upstream locations, license information and checksums.

## Preference persistence

**Culling → Peaking color** (Red, the default, Cyan, Yellow or White) and
**Peaking sensitivity** (Low, Medium or High) style focus peaking; they are stored
as `peaking_color` and `peaking_sensitivity`.

**Culling → Trash destination** chooses **OS Trash** (the default) or **In-app**.
OS Trash moves confirmed local photo groups out of their source folders and uses
your operating system's recovery interface. In-app Trash only hides catalog
entries and can be restored or undone in Shadowroom. Changing the preference
does not move existing files or convert old in-app Trash decisions. Every UI
deletion requires its own confirmation; **Shift+Delete** requests permanent
deletion regardless of this preference. See [Trash and duplicates](DUPLICATES_TRASH.md).

Preferences live in the user's configuration directory under `shadowroom/settings.json`. `Settings::load(path)` returns defaults for a missing file and fills missing fields in older files. Invalid JSON is reported and left untouched. Unknown future preference keys survive a load/save cycle.

`Settings::save(path)` writes a same-directory temporary file with exclusive creation, flushes and syncs its data, then atomically renames it over the target. Failed writes do not partially replace the existing file. On Unix new snapshots are owner-readable/writable. Saves replace a target symlink itself, rather than following it. This protects one complete snapshot; the desktop holds an exclusive catalog lease to prevent concurrent writable instances.

Defaults are Geist Mono, Shadowroom Dark, burst grouping, 2,000 ms burst gap, visual distance 8, automatic advance on, automatic removable-drive discovery on, OS Trash (`trash_mode: "system"`), 8,192 MiB disk preview budget, automatic memory sizing, and the startup Getting Started guide enabled. Optional in-app Trash is stored as `trash_mode: "in_app"`. Auto is stored as `memory_budget_mb: 0` and uses a quarter of physical RAM, bounded to 512 MiB–4 GiB; manual limits remain available. Help controls the startup guide, which can also be reopened with F1 or ?; closing the guide the first time it opens by itself turns the startup guide off (a notice says so) unless **Show at startup** was changed in that session. Grouping can be turned off. Session fields retain the selected photograph, view mode, inspector visibility, minimum rating and flag filter. Stored budgets are clamped to safe ranges; visual hash distance is limited to 64 and star filters to five. These preferences configure the app; the media pipeline enforces the actual cache limits.
