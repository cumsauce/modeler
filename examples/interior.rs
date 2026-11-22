//! A two-room bunker by subtraction: the hall and the cell are cutters, a
//! doorway is a third cutter through the wall between them, and `carve`
//! keeps only the faces looking into the void as one-sided sheets (split by
//! cutter and facing, each wearing its cutter's material), with room /
//! opening markers and mesh colliders. Checks that the route from the hall
//! to the cell is walkable, then exports a glTF.
//!
//! Run: cargo run --example interior [-- out.gltf]

use modeler::commands::{Session, VerifySpec};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/interior.gltf".into());
    let mut s = Session::default();
    s.model.name = "bunker".into();
    let items = serde_json::json!([
        // Cutters, each with its own material: the sheets inherit it.
        {"type": "primitive", "name": "hall", "kind": "box", "size": [6, 3, 5], "translation": [0, 1.5, 0]},
        {"type": "primitive", "name": "cell", "kind": "box", "size": [4, 3, 4], "translation": [6, 1.5, 0]},
        // The doorway overlaps both rooms by 10 cm.
        {"type": "primitive", "name": "door", "kind": "box", "size": [2, 2.1, 1], "translation": [3, 1.05, 0]},
        {"type": "material", "name": "hall", "color": "#b59a7a", "flat": true},
        {"type": "material", "name": "cell", "color": "#8e9aa0", "flat": true},
        {"type": "material", "name": "door", "color": "#a08060", "flat": true},
        {"type": "hide_part", "part": "hall"},
        {"type": "hide_part", "part": "cell"},
        {"type": "hide_part", "part": "door"},
        {"type": "carve", "name": "bunker", "cutters": ["hall", "cell", "door"],
         "split_by": "both", "markers": true, "collider": "mesh"},
        {"type": "light", "name": "hall_lamp", "kind": "point", "translation": [0, 2.7, 0], "intensity": 20000}
    ]);
    let serde_json::Value::Array(items) = items else {
        return;
    };
    let summary = s
        .apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    println!("{summary}");
    let spec: VerifySpec = serde_json::from_value(serde_json::json!({
        "validate": true,
        "grounded": true,
        "collisions": true,
        "walkable": {"from": [-2, 0, 0], "to": [6, 0, 0]}
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    let (ok, text) = s.verify(&spec);
    println!("{text}");
    assert!(ok, "interior failed its checks");
    let out = std::path::Path::new(&out);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{e}"));
    }
    modeler::gltf::export_gltf(&s.model, out).unwrap_or_else(|e| panic!("{e}"));
    println!("wrote {}", out.display());
}
