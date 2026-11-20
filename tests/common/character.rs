//! A rigged, dressed human exported the way a game would take it: shared by
//! the export validation test (`tests/character_export.rs`) and the Bevy
//! load test (`crates/modeler_bevy/tests/character.rs`).

#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use modeler::commands::Session;
use serde_json::{Value, json};

/// Where the human template lives; tests skip without it.
pub fn human_available() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let here = root.join("data/human/human.bin");
    let up = root.join("../../data/human/human.bin");
    here.exists() || up.exists()
}

/// The walk clip of `examples/human_walk.json`.
fn walk_clip() -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join("examples/human_walk.json"))
        .or_else(|_| std::fs::read_to_string(root.join("../../examples/human_walk.json")))
        .unwrap();
    let items: Vec<Value> = serde_json::from_str(&text).unwrap();
    items
        .into_iter()
        .find(|i| i["type"] == "clip" && i["name"] == "walk")
        .unwrap()
}

pub fn items() -> Vec<Value> {
    vec![
        json!({"type": "body", "name": "eve", "preset": "human",
               "shapes": {"smile": {"expression": {"mouth-corner-puller": 1}}}}),
        json!({"type": "garment", "name": "shirt", "on": "eve", "preset": "tshirt"}),
        json!({"type": "bind", "name": "shirt_bind", "skeleton": "eve_rig",
               "parts": ["shirt"], "mode": "smooth", "weights_from": "eve"}),
        json!({"type": "variant", "name": "bare", "hide": ["shirt"]}),
        json!({"type": "variant", "name": "red",
               "materials": {"shirt": {"color": "#c02020", "flat": true}}}),
        walk_clip(),
        json!({"type": "clip", "name": "jump", "skeleton": "eve_rig", "duration": 0.6,
               "loop": false, "root_motion": true, "events": [[0.3, "land"]],
               "tracks": {"hip": {"translate": [[0, [0, 0, 0]], [0.6, [0, 0, 0.5]]]}}}),
        json!({"type": "clip", "name": "talk", "skeleton": "eve_rig", "over": "walk",
               "additive": true,
               "morphs": {"eve": {"smile": [[0, 0], [0.5, 1], [1, 0]]}}}),
        json!({"type": "clip", "name": "step", "skeleton": "eve_rig", "duration": 1.0,
               "root_motion": true,
               "events": [[0.0, "footstep_l"], [0.5, "footstep_r", {"volume": 0.8}]],
               "tracks": {"hip": {"translate": [[0, [0, 0, 0]], [1, [0, 0, 1.2]]]},
                          "thigh_l": {"rotate": [[0, {"x": 25}], [0.5, {"x": -25}], [1, {"x": 25}]]}}}),
        json!({"type": "sway", "name": "swing", "skeleton": "eve_rig", "kind": "spring",
               "over": "walk", "bones": ["hand_*"]}),
    ]
}

/// The built session, or the error text.
pub fn build() -> Result<Session, String> {
    let mut s = Session::default();
    s.model.name = "eve".into();
    s.apply_values_as(None, items(), false)
        .map_err(|e| format!("{e:?}"))?;
    Ok(s)
}

/// Exports `models/eve.gltf` into a fresh scratch assets dir; returns
/// (assets dir, gltf path, report).
pub fn export(tag: &str, optimize: &Value) -> Result<(PathBuf, PathBuf, String), String> {
    let s = build()?;
    let dir = std::env::temp_dir().join(format!("modeler-character-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let models = dir.join("models");
    std::fs::create_dir_all(&models).map_err(|e| e.to_string())?;
    let path = models.join("eve.gltf");
    let opts = modeler::export_opt::Options::parse(optimize).map_err(|e| format!("{e:?}"))?;
    let report =
        modeler::gltf::export_gltf_with(&s.model, &path, &opts).map_err(|e| format!("{e:?}"))?;
    Ok((dir, path, report))
}
