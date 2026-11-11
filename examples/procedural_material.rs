//! Builds three procedural PBR materials (tile-domain bricks, object-domain
//! oak and marble) the way an agent does through material_ops, applies them
//! to a small scene and exports it as a textured .glb plus each material's
//! preview image. Executable documentation of the material workflow; the
//! library and catalog are untouched.
//!
//! Run: cargo run --example procedural_material [out_dir]

use modeler::commands::Session;
use modeler::material::MatExpect;
use serde_json::{Value, json};

fn nodes(v: Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("modeler-procedural-material"));
    std::fs::create_dir_all(&out).unwrap_or_else(|e| panic!("{e}"));
    let mut s = Session::default();
    let checks = MatExpect {
        tileable: Some(true),
        ranges: Some(true),
        albedo: Some(modeler::material::doc::Albedo::On(true)),
        normals: Some(true),
        coverage: None,
    };

    // Bricks: a staggered grid (tile_grid), a rounded rect SDF per cell with
    // noise-displaced edges, a random tint per brick id, height -> normal + AO.
    // A 1 m tile holding a whole number of 0.25 x 0.0625 m courses: seamless.
    let (report, ok) = s
        .materials
        .ops(
            "bricks",
            nodes(json!([
                {"type": "tile_grid", "name": "g", "size": [0.25, 0.0625], "stagger": 0.5},
                {"type": "rect", "name": "brick", "coords": "g.local", "size": [0.238, 0.0525], "round": 0.004},
                {"type": "noise", "name": "edge_n", "size": 0.02, "octaves": 3, "seed": 2},
                {"type": "displace", "name": "brick_d", "input": "brick", "by": "edge_n", "amount": 0.006},
                {"type": "mask", "name": "m", "sdf": "brick_d", "softness": 0.003},
                {"type": "noise", "name": "n", "size": 0.05, "octaves": 5, "seed": 3},
                {"type": "random", "name": "tint", "id": "g.id", "min": 0.75, "max": 1.15},
                {"type": "hsv", "name": "brick_col", "input": "#9c4a32", "val": "tint"},
                {"type": "blend", "name": "dirty", "a": "brick_col", "b": "n", "mode": "multiply", "opacity": 0.35},
                {"type": "blend", "name": "col", "a": "#9b958a", "b": "dirty", "opacity": "m"},
                {"type": "math", "name": "h", "op": "mul", "a": "m", "b": 0.008},
                {"type": "remap", "name": "rough", "input": "n", "from": [0, 1], "to": [0.75, 0.95]},
                {"type": "ao_from_height", "name": "ao", "height": "h", "radius": 0.01},
                {"type": "output", "name": "out", "base_color": "col", "roughness": "rough", "height": "h", "ao": "ao"}
            ])),
            None,
            None,
            Some(1.0),
            Some(256),
            Some(&checks),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    println!("{report}");
    assert!(ok, "bricks checks");

    // Marble (object domain): 3D noise warps diagonal bands through the
    // solid, so the veins run continuously around every edge.
    let (report, _) = s
        .materials
        .ops(
            "marble",
            nodes(json!([
                {"type": "input", "name": "p", "of": "position"},
                {"type": "noise", "name": "wn", "coords": "p", "size": 0.12, "octaves": 4, "seed": 5},
                {"type": "split", "name": "ps", "input": "p"},
                {"type": "math", "name": "xy", "op": "add", "a": "ps.x", "b": "ps.y"},
                {"type": "math", "name": "xyz", "op": "add", "a": "xy", "b": "ps.z"},
                {"type": "math", "name": "w", "op": "mul", "a": "wn", "b": 0.6},
                {"type": "math", "name": "phase", "op": "add", "a": "xyz", "b": "w"},
                {"type": "math", "name": "f", "op": "mul", "a": "phase", "b": "$veins"},
                {"type": "math", "name": "s", "op": "sin", "a": "f"},
                {"type": "math", "name": "v", "op": "abs", "a": "s"},
                {"type": "levels", "name": "vein", "input": "v", "in": [0.0, 0.25], "gamma": 0.6},
                {"type": "ramp", "name": "col", "input": "vein", "stops": [[0, "#5a5862"], [0.5, "#c9c5be"], [1, "#ece8e1"]]},
                {"type": "remap", "name": "rough", "input": "vein", "from": [0, 1], "to": [0.35, 0.15]},
                {"type": "output", "name": "out", "base_color": "col", "roughness": "rough"}
            ])),
            Some(modeler::params::Params::from([("veins".to_string(), 18.0)])),
            Some("object".into()),
            None,
            Some(256),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    println!("{report}");

    // Oak (object domain): rings around a warped axis along x, fine fibre noise.
    let (report, _) = s
        .materials
        .ops(
            "oak",
            nodes(json!([
                {"type": "input", "name": "p", "of": "position"},
                {"type": "noise", "name": "wn", "coords": "p", "size": 0.3, "octaves": 3, "seed": 9},
                {"type": "warp", "name": "pw", "coords": "p", "by": "wn", "amount": 0.04},
                {"type": "split", "name": "s", "input": "pw"},
                {"type": "combine", "name": "yz", "x": "s.y", "y": "s.z"},
                {"type": "rings", "name": "r", "coords": "yz", "spacing": 0.008, "center": [-0.4, -0.3]},
                {"type": "remap", "name": "grain", "input": "r", "from": [0, 0.004], "to": [0, 1]},
                {"type": "noise", "name": "fib", "coords": "p", "size": 0.01, "octaves": 2, "seed": 3},
                {"type": "ramp", "name": "base", "input": "grain", "stops": [[0, "#6b4428"], [0.6, "#a4743f"], [1, "#b98a52"]]},
                {"type": "blend", "name": "col", "a": "base", "b": "fib", "mode": "multiply", "opacity": 0.25},
                {"type": "math", "name": "h", "op": "mul", "a": "grain", "b": 0.0004},
                {"type": "output", "name": "out", "base_color": "col", "roughness": 0.55, "height": "h"}
            ])),
            None,
            Some("object".into()),
            None,
            Some(256),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    println!("{report}");

    for m in ["bricks", "marble", "oak"] {
        let (_, png) = s
            .materials
            .view(Some(m), None, None, Some(256), None)
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(out.join(format!("{m}.png")), png).unwrap_or_else(|e| panic!("{e}"));
    }

    // A wall, a counter and its top: materials are just another apply_ops item.
    let items = nodes(json!([
        {"type": "primitive", "name": "wall", "kind": "box", "size": [3, 2.4, 0.2], "translation": [0, 1.2, -0.6]},
        {"type": "material", "name": "wall", "pbr": "bricks"},
        {"type": "primitive", "name": "counter", "kind": "box", "size": [1.6, 0.9, 0.6], "translation": [-0.5, 0.45, 0]},
        {"type": "material", "name": "counter", "pbr": "oak"},
        {"type": "primitive", "name": "top", "kind": "box", "size": [1.7, 0.04, 0.65], "translation": [-0.5, 0.92, 0.02]},
        {"type": "material", "name": "top", "pbr": "marble", "params": {"veins": 24}}
    ]));
    s.apply_values_as(None, items, false)
        .unwrap_or_else(|e| panic!("{e}"));
    let glb = out.join("material-demo.glb");
    modeler::gltf::export_glb(&s.model, &glb).unwrap_or_else(|e| panic!("{e}"));
    println!(
        "wrote {} and the material previews in {}",
        glb.display(),
        out.display()
    );
}
