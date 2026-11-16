# Keyboard and Creator Micro controls

Shadowroom receives normal keyboard shortcuts from the focused application window. No driver, device claim, raw HID protocol, firmware flash, or vendor background bridge is required by the app. Configuring the controller itself requires its manufacturer's configuration tool.

## Keyboard defaults

| Shortcut | Action |
| --- | --- |
| Left / Right | Previous / next photograph; at either end the status bar says "First photo" / "Last photo" |
| Shift+Down / Shift+Up | Extend / shrink selection by a grid row, keeping the original anchor |
| Home / End | First / last photograph; Shift extends a Catalog selection. Home re-centres a zoomed photo |
| Page Up / Page Down | One screen of Catalog rows (Shift extends the selection); nine frames in Review |
| `[` / `]` | Previous / next group |
| `P` / `X` / `U` | Pick / reject / clear flag |
| `0`–`5` | Set star rating; zero clears it |
| `R` | Reset stars and pick/reject together without advancing; keep color label |
| `6`–`9` | Red / yellow / green / blue label; the key of the label a photo already has clears it, without advancing |
| `G` / `L` (or `E`) / `C` / `S` | Grid / loupe / compare / survey |
| Enter or Space in Catalog | Enter Review; a selection of two or more photos limits navigation (and Survey) to that set |
| Double-click a Catalog photo | Review the full current collection at that photo |
| `Z` or Space | Toggle fit / 100% zoom, centred on the point under the pointer when it rests on the photo |
| Click the photo | At Fit, 100% centred where you clicked; when zoomed, back to Fit |
| `=` / `-` | Zoom in / out; in Catalog, larger / smaller thumbnails |
| Ctrl/Cmd + scroll | Zoom in / out around the pointer; plain scroll pans while zoomed |
| Alt/Option + arrows | Pan a zoomed photo by a third of the view |
| `B` | Pin the current photo as the Compare reference; in Compare, make the candidate the reference and bring in the next frame |
| Shift+B / Shift+C | Compare: swap reference and candidate / link or unlink the panes' pan and zoom |
| Tab / Shift+Tab | Focus mode (hide side panels); in Compare, Tab changes the active pane and Shift+Tab is Focus mode |
| Shift+P | In Review or Survey: pick this frame and reject the undecided frames of its burst (Survey: of what it shows) |
| `F` | Toggle cyan focus peaking in Review and Compare |
| `F1` | Open Getting Started |
| `H` / `I` | Toggle histogram / inspector |
| Shift+A | Toggle automatic advance |
| Ctrl+O or Cmd+O | Open a folder |
| Ctrl+, or Cmd+, | Settings |
| Ctrl+Q or Cmd+Q | Quit from any view, field, menu or dialog; waits for pending saves, exports and imports. With unsaved decisions it asks first; a second press quits anyway from the recovery journal |
| Escape | Dismiss the error notice when one is showing (nothing else that press); in an Import field, leave the field; otherwise close the topmost menu, dialog or Import, then cancel a held batch decision, collapse a Catalog multi-selection, return zoom to Fit and leave Focus mode, one step per press |
| Delete / Backspace | Open the confirmation for the configured Trash destination |
| Shift+Delete | Open the permanent-deletion confirmation |

Automatic advance has no bare-letter shortcut, so a stray key can't change how decisions move: use Shift+A or the toggle in the bottom bar, which also shows your position (`2 / 7`). The status bar confirms each change. When the bar is narrow it drops, in order, the **Reset** label, the reviewed count, the zoom −/+ buttons, Pin and Survey, and finally the Pick/Reject words; the keycaps, position, auto-advance and zoom stay. In Catalog the bar shows an **S / M / L** thumbnail-size control instead of loupe zoom and focus peaking.

A decision on 50 or more selected photos is confirmed in the status bar with its count and **Undo**. A decision on 1,000 or more photos first asks above the grid: Enter applies it and Escape cancels. With several photos selected the bar and inspector show what the photos share: a flag is highlighted only when every photo has it (a partial mark when some do), and stars read "mixed" when ratings differ.

Flags and ratings never delete source media. Repeating the same rating, Pick or Reject keeps that decision. **R**, the **Reset** button and **Reset Rating and Flag** in the command palette clear stars and pick/reject together, preserving color labels. Reset never auto-advances, works on the selected Catalog photos or the active Review/Compare photo, and can be undone as one decision. A photo may leave the visible list if it no longer matches the current filter. Automatic advance, when enabled, moves after an explicit rating or flag decision; rotary rating changes should be checked with automatic advance off during calibration.

The loupe shows the current photo's flag, stars and label at its top-left. When a decision moves on to another frame (automatic advance, or the photo leaving the filtered view), the previous frame's decision appears briefly at the bottom-left of the photo, and the status bar names it with an **Undo** button for a few seconds. The status bar reports other completed actions the same way (undo/redo, pinning, focus peaking, resolved bursts, duplicate scans, Trash, imports with **Reveal**, exports with **Reveal**, ejects), then returns to the photo count.

Zoom ranges from 12.5% to 800%; the zoom button shows the actual magnification
at Fit (for example **Fit 45%**), the **−** and **+** buttons change it, and **Z**
restores 100% or Fit. While zoomed, a navigator at the photo's bottom-right shows
where the view sits in the frame; click or drag it to move. Adjacent frames retain
zoom and pan. In Compare, arrows skip the reference frame, so the two panes never
show the same photo, and each pane is labelled on the picture with its role, file
and decision; the active pane's label is underlined. Small windows give the photo
room: below 1280 px wide, Review, Compare and Survey hide the sidebar (and below
1100 px, Compare and Survey also hide the inspector) without changing your panel
settings; Ctrl/Cmd+B brings the sidebar back. Survey (`S`) shows a frozen Catalog
selection of two or more photos (**Selection · 4 photos**), otherwise the current
burst, otherwise up to nine neighbouring frames (**Neighbours**); its header's
**⇧P Keep this · reject undecided** button acts on exactly those frames. Focus peaking
highlights abrupt luminance edges in the displayed preview, including both Compare
panes. It is a visual aid, not an automatic sharpness verdict, and never changes
the source or histogram.

Catalog and import grids support anchored Shift+Arrow and Shift-click selection;
Ctrl/Cmd-click toggles individual frames. In the Import grid a plain click or
arrow only moves the current tile; the checkbox, Space and Ctrl/Cmd-click check
or uncheck a photo, Shift extends from the anchor, Tab enters the fields (Job
code first; J jumps there) and Ctrl/Cmd+Return starts the import.
With zero or one selected photo, **Enter/L** opens Review across the current
folder and filters, starting at the selected (outlined) photo, never at the
photo under the pointer. With two or more selected photos, **Enter/L** freezes that
selection for Review navigation. Compare also constrains a multiple selection.
Decisions in Review/Compare affect the current frame. Double-click and the
**Show in review** context action always open the full current collection.
**Show full folder** leaves a subset. Returning to Catalog restores its selection.

Right-click a Catalog photo, Review image, or filmstrip thumbnail for **Show in
folder**, **Show in review**, **Survey**, **Compare**, Pick, Reject, Reset,
and deletion actions. Right-clicking a selected Catalog photo keeps that batch;
right-clicking a different photo makes it the action target. The menu names the
photo or selected count. Up/Down and Enter activate a menu action; Escape closes
it.

Delete uses **Preferences → Culling → Trash destination**: **OS Trash** by
default, or optional **In-app** Trash. Shift+Delete always requests permanent
deletion. Each action opens a confirmation naming the photo (or the count),
with thumbnails, file counts, source locations and destination, and a warning
when the selection includes picks or photos rated 3 stars and up. Permanent
deletion is marked with an error-coloured warning, a "can't be undone" callout
and a filled red confirm button. **Cancel is selected initially: Enter cancels,
Tab then Enter confirms, and Escape cancels.** Restore OS-trashed files with your file
manager; app Restore/Undo applies only to in-app Trash. Physical deletion cannot
be undone in Shadowroom and clears decision Undo history after files change.
See [Trash and duplicates](DUPLICATES_TRASH.md) for grouping and partial failures.

Text fields show a blinking caret, support arrows, Home/End, Shift selection,
Backspace/Delete, and Ctrl/Cmd+A/C/X/V. Tab changes import or network-host fields.

In **Preferences**, Tab/Shift+Tab move focus, arrows choose within a group, Space
activates, Return is **Apply & Close** (or activates a focused button),
Ctrl+Tab/Ctrl+PageDown switch sections, Cmd/Ctrl+S applies and Escape closes
(asking first when there are unsaved changes). See [Themes and preferences](THEMES.md).

`src/controls.rs` defines the shared keyboard/controller commands and bindings. The desktop also handles Review entry, context menus, and deletion gestures. Dedicated F13–F24 shortcuts provide a keyboard-layout-independent controller layer:

| Key | Command | Original Micro matrix position |
| --- | --- | --- |
| F13 | Pick | 0,1 |
| F14 | Reject | 0,2 |
| F15 | Clear flag | 1,0 |
| F16 | Rating down | 1,1 |
| F17 | Rating up | 1,2 |
| F18 | Grid | 1,3 |
| F19 | Loupe | 2,0 |
| F20 | Compare | 2,1 |
| F21 | Fit / 100% | 2,2 |
| F22 | Histogram | 2,3 |
| F23 | Previous group | 3,1 |
| F24 | Next group | 3,2 |

## Identify the Micro generation first

The original Creator Micro uses VIA. Work Louder's [original setup guide](https://worklouder.cc/setup) describes assigning both keys and dials in VIA. The official [VIA keyboard definition](https://github.com/the-via/keyboards/blob/master/v3/work_louder/micro.json) identifies it as USB **574C:E6E3**, a **4 × 4** matrix with two encoders. The matrix positions above come from that definition, rather than inferred visual numbering.

Creator Micro 2 uses Work Louder **Input**, as documented in its [Micro 2 setup guide](https://worklouder.cc/micro-setup). It has a different layout and one dial; configure the app shortcut layer manually in Input. The original Micro VIA file is incompatible with Micro 2. Shadowroom's assignment sheet is portable because both tools can assign keyboard shortcuts. Shadowroom does not claim an Input-native import format or device lighting support.

## Original Creator Micro: VIA setup

1. Connect by a data-capable USB cable, open [VIA](https://usevia.app/) in a supported browser and authorize your Micro. Check the displayed product identity.
2. Use VIA's **Save+Load → Save** before changing anything. Keep this original backup.
3. Choose a layer for Shadowroom. In **Configure**, map the twelve main keys to F13–F24 in the table. If needed, use **Special → Any** and enter `KC_F13` through `KC_F24`.
4. Assign the left dial counterclockwise to `KC_LEFT`, clockwise to `KC_RGHT`, and its press to `KC_F21`. Assign the right dial counterclockwise to `KC_F16`, clockwise to `KC_F17`, and its press to `KC_F22`.
5. Keep the device's existing layer switching controls, then save a second VIA backup of the configured layout.

VIA can store encoder keymaps only when the firmware supports them. Its [QMK configuration guide](https://caniusevia.com/docs/configuring_qmk/) distinguishes a configurable encoder map from ordinary encoder hardware. If your VIA screen lacks editable dial directions, use the manufacturer's supported configuration or map the events the firmware already emits. Shadowroom does not enable encoder remapping by writing a keyboard definition.

### Safe export API

`assets/controls/creator-micro.shortcuts.json` is an app assignment sheet, **not** a VIA keyboard definition or VIA Save+Load file. `export_shortcut_profile(path)` writes the same sheet.

For a genuine importable VIA file, the Rust API patches your device's own exported Save+Load backup:

```rust
shadowroom::controls::export_via_layout(
    std::path::Path::new("creator-micro-original.layout.json"),
    std::path::Path::new("shadowroom.layout.json"),
    0, // zero-based existing layer to replace
)?;
```

Load the resulting file using **Save+Load → Load**, not the Design tab. It validates the original Micro ID and matrix; replaces twelve main keys, two dial presses and existing configurable dial turns only on the requested layer; and preserves all other layers, macros, extra metadata and layer controls at matrix positions 3,0 and 3,3. It refuses files for another device or malformed matrices. No encoder support is invented when the backup has none. The encoder array follows VIA's actual [Save+Load source](https://github.com/the-via/app/blob/main/src/components/panes/configure-panes/save-load.tsx): `[encoder][layer][counterclockwise, clockwise]`.

## Micro 2: Input setup

Create a Shadowroom layer in Input and assign the desired buttons to F13–F24. Configure the one dial to emit Left/Right for photograph navigation. Put F16/F17 on two buttons for rating, and use the dial press for F21 if Input exposes that control. Map an additional layer or joystick direction to `[` / `]` for group navigation if useful. Configure keyboard shortcuts, rather than text insertion macros. Use the portable letter shortcuts if your OS or Input build cannot emit extended function keys.

## Calibration and limits

Before a session, open a small disposable test catalog and disable automatic advance. Confirm that one clockwise detent moves exactly one photograph forward and one counterclockwise detent moves backward. If reversed, swap the two assignments in VIA/Input. Confirm both dial presses independently. Check F13 pick, F14 reject and F15 clear, then F16/F17 rating bounds at zero and five. Enable automatic advance only after the mappings behave as intended.

If a detent generates multiple events, first check the vendor tool's macro/repeat settings and firmware's encoder resolution; the app intentionally handles each keyboard event once. OS key repeat may repeat a held button. F13–F24 can be reserved by desktop shortcuts or accessibility tools; release those shortcuts or use the portable defaults. The app must have keyboard focus, and an active text input may consume keys. Device disconnection simply stops events; reconnecting requires no app-side device discovery.

Automated tests verify unique app bindings, every profile assignment's command, VIA matrix indices, encoder array orientation, preservation of other layers and macros, and rejection of wrong devices. Physical USB/Bluetooth, individual encoder detents, Input/VIA UI interactions, OS global shortcuts, and device LEDs require the user's hardware for validation; they have not been claimed as hardware-tested.
