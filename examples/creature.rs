//! A cat from the `body` preset: proportions through joint overrides,
//! sculpted with local warps (fuller chest, a pinched waist, a flatter
//! brow), kept light with `tris`. Checks it is a closed, grounded, stable
//! solid and prints its proportions.
//!
//! Run: cargo run --example creature

use modeler::commands::{Session, VerifySpec};

fn main() {
    let mut s = Session::default();
    s.model.name = "creature".into();
    let items = serde_json::json!([
        {"type": "body", "name": "cat", "preset": "quadruped", "height": 0.25,
         "build": 1.1, "tail": 1.2,
         // Bigger head and paws, ears raised a little.
         "joints": {"head": {"scale": 1.1}, "paw_l": {"scale": 1.15}, "paw_r": {"scale": 1.15},
                    "ear_l": {"offset": [0, 0.01, 0]}, "ear_r": {"offset": [0, 0.01, 0]}},
         // Local warps, centred on joints: a deeper chest, a slimmer waist,
         // the brow pushed back.
         "warps": [
            {"mode": "inflate", "at": "chest", "radius": 0.09, "amount": 0.012},
            {"mode": "pinch", "at": "spine", "radius": 0.08, "amount": 0.3},
            {"mode": "push", "at": "head", "radius": 0.05, "amount": 0.006, "direction": [0, 0, -1]}],
         "tris": 6000},
        {"type": "material", "name": "cat", "color": "#9a7b5c", "roughness": 0.8, "flat": true}
    ]);
    let serde_json::Value::Array(items) = items else {
        return;
    };
    let summary = s
        .apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    println!("{summary}");
    let spec: VerifySpec = serde_json::from_value(serde_json::json!({
        "validate": true, "grounded": true, "materials": true
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    let (ok, text) = s.verify(&spec);
    println!("{text}");
    assert!(ok, "the cat failed its checks");
    let tris = s
        .model
        .parts
        .iter()
        .map(|p| p.indices.len() / 3)
        .sum::<usize>();
    println!("triangles: {tris}");
    assert!(tris <= 6600, "tris: {tris}");
}
