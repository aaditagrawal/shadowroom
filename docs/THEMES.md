# Themes, fonts and preferences

Shadowroom includes restrained **Shadowroom Dark** and **Shadowroom Light** palettes. Image canvases are neutral; color conveys selections, picks, rejections and ratings. Imported Zed theme families can provide the same application colors.

## Appearance controls

Click the active theme name in the status bar, open **Preferences → Appearance**
with **Cmd/Ctrl+,**, or search **Appearance & Themes** in **Cmd/Ctrl+K**. Appearance
is the first preferences section. Culling, Performance, Controller and Help have their
own sections so a large theme collection does not bury other settings.

Preferences use a separate draft. Choices do not change the running app or write
the settings file until you click **Apply** or **Apply & Close**. **Apply** saves
and leaves preferences open; **Apply & Close** saves and closes them. **Cancel**,
Escape, the close button, or clicking outside discards only the unapplied changes
since the last successful Apply. Switching sections keeps the draft. Imported
theme files are staged and installed only when applied.

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

Shadowroom Dark uses `#0f0f0f` for the workspace/editor/image canvas, `#141414` for panels and chrome, `#1b1b1b` for elevated/hover surfaces, `#232323` for selection and `#2a2a2a` for active controls. Text is `#e6e6e6`, muted text `#8a8a8a`, borders `#262626`/`#333333`, accent `#5b9bd5`, success `#7fbf7f`, error `#d46a6a` and warning `#e0b341`.

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

**Culling → Trash destination** chooses **OS Trash** (the default) or **In-app**.
OS Trash moves confirmed local photo groups out of their source folders and uses
your operating system's recovery interface. In-app Trash only hides catalog
entries and can be restored or undone in Shadowroom. Changing the preference
does not move existing files or convert old in-app Trash decisions. Every UI
deletion requires its own confirmation; **Shift+Delete** requests permanent
deletion regardless of this preference. See [Trash and duplicates](DUPLICATES_TRASH.md).

Preferences live in the user's configuration directory under `shadowroom/settings.json`. `Settings::load(path)` returns defaults for a missing file and fills missing fields in older files. Invalid JSON is reported and left untouched. Unknown future preference keys survive a load/save cycle.

`Settings::save(path)` writes a same-directory temporary file with exclusive creation, flushes and syncs its data, then atomically renames it over the target. Failed writes do not partially replace the existing file. On Unix new snapshots are owner-readable/writable. Saves replace a target symlink itself, rather than following it. This protects one complete snapshot; the desktop holds an exclusive catalog lease to prevent concurrent writable instances.

Defaults are Geist Mono, Shadowroom Dark, burst grouping, 2,000 ms burst gap, visual distance 8, automatic advance on, automatic removable-drive discovery on, OS Trash (`trash_mode: "system"`), 8,192 MiB disk preview budget, automatic memory sizing, and the startup Getting Started guide enabled. Optional in-app Trash is stored as `trash_mode: "in_app"`. Auto is stored as `memory_budget_mb: 0` and uses a quarter of physical RAM, bounded to 512 MiB–4 GiB; manual limits remain available. Help controls the startup guide, which can also be reopened with F1 or ?. Grouping can be turned off. Session fields retain the selected photograph, view mode, inspector visibility, minimum rating and flag filter. Stored budgets are clamped to safe ranges; visual hash distance is limited to 64 and star filters to five. These preferences configure the app; the media pipeline enforces the actual cache limits.
