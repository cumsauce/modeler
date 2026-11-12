//! A small critter, auto-rigged from its `skin`, walking: half the walk is
//! written (the left legs) and `mirror` + `phase` make the other half. A
//! spring tail layers over the walk. Checks the motion, then exports a glTF
//! with the skin, the clips and the sway settings for `modeler_bevy`.
//!
//! Run: cargo run --example walk_cycle [-- out.gltf]

use modeler::commands::{Session, VerifySpec};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/walk_cycle.gltf".into());
    let mut s = Session::default();
    s.model.name = "walker".into();
    let items = serde_json::json!([
        // Shape: one smooth skin. Only the +x side is written; mirror "x"
        // makes the left/right pair.
        {"type": "skin", "name": "critter", "mirror": "x", "joints": {
            "hip": [0, 0.3, -0.12, 0.1], "chest": [0, 0.32, 0.12, 0.11], "head": [0, 0.45, 0.28, 0.09]},
         "chains": [
            ["hip", "chest", "head"],
            ["hip", [0.09, 0.22, -0.12, 0.045], [0.1, 0.12, -0.1, 0.035], [0.1, 0.03, -0.08, 0.03]],
            ["chest", [0.09, 0.22, 0.12, 0.045], [0.1, 0.12, 0.14, 0.035], [0.1, 0.03, 0.16, 0.03]],
            ["hip", [0, 0.32, -0.25, 0.04], [0, 0.36, -0.38, 0.025], [0, 0.42, -0.48, 0.015]]]},
        // Rig: a bone per chain point (the legs get _l / _r names).
        {"type": "skeleton", "name": "rig", "from": "critter"},
        {"type": "bind", "name": "skin", "skeleton": "rig", "parts": ["critter"], "mode": "auto"},
        {"type": "material", "name": "critter", "color": "#c98a4b", "roughness": 0.7, "flat": true},
        // Walk: the left legs swing opposite each other (knees lift as each
        // leg swings forward), and each right leg mirrors its left one half
        // a cycle on.
        {"type": "clip", "name": "walk", "skeleton": "rig", "duration": 0.8, "tracks": {
            "c1_1_l": {"rotate": [[0, {"x": 22}], [0.4, {"x": -22}], [0.8, {"x": 22}]]},
            "c1_2_l": {"rotate": [[0, {"x": 0}], [0.2, {"x": -30}], [0.4, {"x": 0}], [0.8, {"x": 0}]]},
            "c2_1_l": {"rotate": [[0, {"x": -22}], [0.4, {"x": 22}], [0.8, {"x": -22}]]},
            "c2_2_l": {"rotate": [[0, {"x": 0}], [0.4, {"x": 0}], [0.6, {"x": -30}], [0.8, {"x": 0}]]},
            "hip": {"translate": {"wave": {"axis": "y", "amount": 0.008, "period": 0.4}}}},
         "mirror": {
            "c1_1_r": {"from": "c1_1_l", "phase": 0.5}, "c1_2_r": {"from": "c1_2_l", "phase": 0.5},
            "c2_1_r": {"from": "c2_1_l", "phase": 0.5}, "c2_2_r": {"from": "c2_2_l", "phase": 0.5}}},
        // The tail lags behind the bobbing hips.
        {"type": "sway", "name": "walk_tail", "skeleton": "rig", "kind": "spring", "over": "walk",
         "bones": ["c3_*"], "stiffness": 50, "damping": 5, "gravity": 0.05}
    ]);
    let serde_json::Value::Array(items) = items else {
        return;
    };
    let summary = s
        .apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    println!("{summary}");
    for clip in ["walk", "walk_tail"] {
        let spec: VerifySpec = serde_json::from_value(serde_json::json!({
            "weights": true,
            "animation": {"clip": clip, "samples": 16, "loop": true, "stretch": 2.0}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let (ok, text) = s.verify(&spec);
        println!("{text}");
        assert!(ok, "{clip} failed its checks");
    }
    let out = std::path::Path::new(&out);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{e}"));
    }
    modeler::gltf::export_gltf(&s.model, out).unwrap_or_else(|e| panic!("{e}"));
    println!("wrote {}", out.display());
}
