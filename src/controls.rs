//! Device-independent keyboard commands and original Creator Micro VIA export.
//! Device firmware emits ordinary keyboard events; no raw HID access is needed.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Previous,
    Next,
    PreviousGroup,
    NextGroup,
    Pick,
    Reject,
    ClearFlag,
    SetRating(u8),
    RatingDown,
    RatingUp,
    Grid,
    Loupe,
    Compare,
    ToggleZoom,
    ZoomIn,
    ZoomOut,
    ToggleHistogram,
    ToggleInspector,
    ToggleAutoAdvance,
    OpenFolder,
    Settings,
    ExportXmp,
    ExportPicks,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutBinding {
    pub keystroke: String,
    pub command: Command,
}

pub fn default_bindings() -> Vec<ShortcutBinding> {
    use Command::*;
    let mut bindings: Vec<_> = [
        ("left", Previous),
        ("right", Next),
        ("[", PreviousGroup),
        ("]", NextGroup),
        ("p", Pick),
        ("x", Reject),
        ("u", ClearFlag),
        ("g", Grid),
        ("l", Loupe),
        ("e", Loupe),
        ("c", Compare),
        ("z", ToggleZoom),
        ("space", ToggleZoom),
        ("=", ZoomIn),
        ("-", ZoomOut),
        ("h", ToggleHistogram),
        ("i", ToggleInspector),
        ("shift-a", ToggleAutoAdvance),
        ("ctrl-o", OpenFolder),
        ("cmd-o", OpenFolder),
        ("ctrl-,", Settings),
        ("cmd-,", Settings),
    ]
    .into_iter()
    .map(|(key, command)| ShortcutBinding {
        keystroke: key.into(),
        command,
    })
    .collect();
    bindings.extend((0..=5).map(|rating| ShortcutBinding {
        keystroke: rating.to_string(),
        command: SetRating(rating),
    }));
    bindings.extend(creator_micro_bindings());
    bindings
}

/// Dedicated keys avoid dependence on keyboard character layout. Users may need
/// to free F13–F24 from desktop/global shortcuts before the app can receive them.
pub fn creator_micro_bindings() -> Vec<ShortcutBinding> {
    use Command::*;
    [
        Pick,
        Reject,
        ClearFlag,
        RatingDown,
        RatingUp,
        Grid,
        Loupe,
        Compare,
        ToggleZoom,
        ToggleHistogram,
        PreviousGroup,
        NextGroup,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, command)| ShortcutBinding {
        keystroke: format!("f{}", index + 13),
        command,
    })
    .collect()
}

pub fn command_for_keystroke(keystroke: &str) -> Option<Command> {
    use std::{collections::HashMap, sync::OnceLock};
    static BINDINGS: OnceLock<HashMap<String, Command>> = OnceLock::new();
    // All default shortcuts are ASCII and at most seven bytes. Rejecting longer or
    // Unicode keys avoids allocation while retaining ASCII-only case folding.
    let mut key = [0u8; 16];
    if keystroke.len() > key.len() || !keystroke.is_ascii() {
        return None;
    }
    for (out, byte) in key.iter_mut().zip(keystroke.bytes()) {
        *out = byte.to_ascii_lowercase();
    }
    let key = std::str::from_utf8(&key[..keystroke.len()]).ok()?;
    BINDINGS
        .get_or_init(|| {
            default_bindings()
                .into_iter()
                .map(|b| (b.keystroke, b.command))
                .collect()
        })
        .get(key)
        .cloned()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlAssignment {
    pub control: String,
    pub keycode: String,
    pub keystroke: String,
    pub command: Command,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<[usize; 2]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControllerProfile {
    pub schema_version: u32,
    pub name: String,
    pub transport: String,
    pub compatibility: String,
    pub assignments: Vec<ControlAssignment>,
}

/// Logical assignment sheet. This is intentionally distinct from VIA's keyboard
/// definition and from its Save+Load file, and works with Input's shortcut editor.
pub fn creator_micro_profile() -> ControllerProfile {
    let positions = [
        [0, 1],
        [0, 2],
        [1, 0],
        [1, 1],
        [1, 2],
        [1, 3],
        [2, 0],
        [2, 1],
        [2, 2],
        [2, 3],
        [3, 1],
        [3, 2],
    ];
    let mut assignments: Vec<_> = creator_micro_bindings()
        .into_iter()
        .enumerate()
        .map(|(i, binding)| ControlAssignment {
            control: format!("key {}", i + 1),
            keycode: format!("KC_F{}", i + 13),
            keystroke: binding.keystroke,
            command: binding.command,
            matrix: Some(positions[i]),
        })
        .collect();
    use Command::*;
    for (name, keycode, key, command, matrix) in [
        (
            "left dial counterclockwise",
            "KC_LEFT",
            "left",
            Previous,
            None,
        ),
        ("left dial clockwise", "KC_RGHT", "right", Next, None),
        ("left dial press", "KC_F21", "f21", ToggleZoom, Some([0, 0])),
        (
            "right dial counterclockwise",
            "KC_F16",
            "f16",
            RatingDown,
            None,
        ),
        ("right dial clockwise", "KC_F17", "f17", RatingUp, None),
        (
            "right dial press",
            "KC_F22",
            "f22",
            ToggleHistogram,
            Some([0, 3]),
        ),
    ] {
        assignments.push(ControlAssignment {
            control: name.into(),
            keycode: keycode.into(),
            keystroke: key.into(),
            command,
            matrix,
        });
    }
    ControllerProfile {
        schema_version:1, name:"Shadowroom · Creator Micro".into(), transport:"standard_keyboard".into(),
        compatibility:"Original Creator Micro: VIA; Micro 2: manually map these shortcuts in Work Louder Input (one dial; no VIA file import).".into(),
        assignments,
    }
}

pub fn export_shortcut_profile(path: &Path) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(&creator_micro_profile())?;
    bytes.push(b'\n');
    crate::settings::atomic_write(path, &bytes)
}

/// Original Micro identity from VIA's official definition. VIA combines IDs as
/// an unsigned 32-bit number with vendor in the high word, product in the low.
pub const CREATOR_MICRO_VENDOR_PRODUCT_ID: u64 = (0x574C_u64 << 16) | 0xE6E3;

/// Patch a real VIA Save+Load backup for the ORIGINAL 4×4 Creator Micro.
/// Preserves other layers, macros, layer controls (3,0 / 3,3), metadata and
/// unsupported encoder configuration. Requires a backup to avoid inventing a
/// layer/macro count or overwriting a board revision's firmware assumptions.
/// Encoders are stored in VIA's order [encoder][layer][CCW, CW].
pub fn patch_via_layout(json: &str, layer_index: usize) -> Result<String> {
    let mut value: Value = serde_json::from_str(json).context("Parse VIA Save+Load backup")?;
    if value.get("vendorProductId").and_then(Value::as_u64) != Some(CREATOR_MICRO_VENDOR_PRODUCT_ID)
    {
        bail!("VIA backup is not for the original Creator Micro (574C:E6E3); Micro 2 uses Input");
    }
    if value.get("name").and_then(Value::as_str).is_none() {
        bail!("VIA backup requires a name");
    }
    let layers = value
        .get_mut("layers")
        .and_then(Value::as_array_mut)
        .context("VIA backup requires layers")?;
    if layers.is_empty()
        || layers.iter().any(|layer| {
            layer
                .as_array()
                .is_none_or(|a| a.len() != 16 || a.iter().any(|v| !v.is_string()))
        })
    {
        bail!("Original Creator Micro VIA backup must contain 16 keycodes per layer");
    }
    let layer_count = layers.len();
    let layer = layers
        .get_mut(layer_index)
        .and_then(Value::as_array_mut)
        .context("Selected VIA layer does not exist")?;
    for assignment in creator_micro_profile().assignments {
        if let Some([row, col]) = assignment.matrix {
            layer[row * 4 + col] = Value::String(assignment.keycode);
        }
    }
    // A real backup with no encoder array may come from older firmware lacking
    // configurable encoders. Never claim that adding an array enables support.
    if let Some(encoders) = value.get_mut("encoders") {
        let encoders = encoders
            .as_array_mut()
            .context("VIA encoders must be an array")?;
        if !encoders.is_empty() {
            if encoders.len() != 2
                || encoders.iter().any(|e| {
                    e.as_array().is_none_or(|layers| {
                        layers.len() != layer_count
                            || layers.iter().any(|pair| {
                                pair.as_array().is_none_or(|p| {
                                    p.len() != 2 || p.iter().any(|v| !v.is_string())
                                })
                            })
                    })
                })
            {
                bail!("Expected original Micro encoder layout [2 encoders][layer count][CCW,CW]");
            }
            encoders[0][layer_index] = serde_json::json!(["KC_LEFT", "KC_RGHT"]);
            encoders[1][layer_index] = serde_json::json!(["KC_F16", "KC_F17"]);
        }
    }
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

pub fn export_via_layout(backup: &Path, destination: &Path, layer_index: usize) -> Result<()> {
    let patched = patch_via_layout(
        &std::fs::read_to_string(backup).context("Read VIA backup")?,
        layer_index,
    )?;
    crate::settings::atomic_write(destination, patched.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore]
    fn bench_shortcut_lookup() {
        let began = std::time::Instant::now();
        for _ in 0..100_000 {
            std::hint::black_box(command_for_keystroke(std::hint::black_box("F22")));
        }
        eprintln!("shortcut 100k {:?}", began.elapsed());
    }
    #[test]
    fn bindings_are_unique_and_device_profile_resolves() {
        let mut seen = std::collections::HashSet::new();
        for binding in default_bindings() {
            assert!(
                seen.insert(binding.keystroke.clone()),
                "Duplicate shortcut: {}",
                binding.keystroke
            );
        }
        for assignment in creator_micro_profile().assignments {
            assert_eq!(
                command_for_keystroke(&assignment.keystroke),
                Some(assignment.command)
            );
        }
        assert_eq!(creator_micro_bindings().len(), 12);
        assert_eq!(command_for_keystroke("F13"), Some(Command::Pick));
        assert_eq!(command_for_keystroke("5"), Some(Command::SetRating(5)));
        assert_eq!(command_for_keystroke("unknown"), None);
    }
    #[test]
    fn bundled_assignment_sheet_matches_export() {
        let bundled: Value = serde_json::from_str(include_str!(
            "../assets/controls/creator-micro.shortcuts.json"
        ))
        .unwrap();
        assert_eq!(
            bundled,
            serde_json::to_value(creator_micro_profile()).unwrap()
        );
    }
    fn backup() -> Value {
        serde_json::json!({"name":"Creator Micro","vendorProductId":CREATOR_MICRO_VENDOR_PRODUCT_ID,
            "layers":[vec!["KC_A";16],vec!["KC_B";16]],"macros":["preserve me"],
            "encoders":[[["KC_UP","KC_DOWN"],["KC_A","KC_B"]],[["KC_VOLD","KC_VOLU"],["KC_C","KC_D"]]],
            "future":{"preserve":true}})
    }
    #[test]
    fn via_patch_preserves_other_layers_macros_and_layer_controls() {
        let original = backup();
        let patched: Value =
            serde_json::from_str(&patch_via_layout(&original.to_string(), 0).unwrap()).unwrap();
        assert_eq!(patched["layers"][1], original["layers"][1]);
        for index in [12, 15] {
            assert_eq!(patched["layers"][0][index], "KC_A");
        }
        assert_eq!(patched["layers"][0][1], "KC_F13");
        assert_eq!(patched["layers"][0][14], "KC_F24");
        assert_eq!(
            patched["encoders"][0][0],
            serde_json::json!(["KC_LEFT", "KC_RGHT"])
        );
        assert_eq!(patched["encoders"][1][1], original["encoders"][1][1]);
        assert_eq!(patched["macros"], original["macros"]);
        assert_eq!(patched["future"], original["future"]);
    }
    #[test]
    fn refuses_wrong_device_or_invented_layouts() {
        let mut original = backup();
        original["vendorProductId"] = serde_json::json!(123);
        assert!(patch_via_layout(&original.to_string(), 0).is_err());
        let mut original = backup();
        original["layers"][0] = serde_json::json!(["KC_A"]);
        assert!(patch_via_layout(&original.to_string(), 0).is_err());
        assert!(patch_via_layout(&backup().to_string(), 5).is_err());
        let mut original = backup();
        original["encoders"] = serde_json::json!([[[]]]);
        assert!(patch_via_layout(&original.to_string(), 0).is_err());
    }
    #[test]
    fn does_not_invent_encoder_support() {
        let mut original = backup();
        original.as_object_mut().unwrap().remove("encoders");
        let patched: Value =
            serde_json::from_str(&patch_via_layout(&original.to_string(), 0).unwrap()).unwrap();
        assert!(patched.get("encoders").is_none());
    }
}
