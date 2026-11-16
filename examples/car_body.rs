//! A car body from three outlines (`silhouette` as an sdf shape: side, top,
//! front, smoothed), with wheel arches carved out by smooth subtractions,
//! four wheels on two axles, kept light with `tris`. Checks every part is
//! closed and the whole car stands on the ground.
//!
//! Run: cargo run --example car_body

use modeler::commands::{Session, VerifySpec};

fn main() {
    let mut s = Session::default();
    s.model.name = "car_body".into();
    let wheel_r = 0.33;
    let mut items = vec![serde_json::json!(
        {"type": "sdf", "name": "body", "blend": 0.04, "tris": 8000, "mirror": "x", "shapes": [
            {"shape": "silhouette", "smooth": true, "round": 0.06,
             // Side [z, y] (front = +z), top [x, z], front [x, y].
             "side": [[2.2, 0.3], [2.25, 0.75], [1.0, 0.85], [0.4, 1.4], [-1.2, 1.4], [-2.2, 0.95], [-2.2, 0.3]],
             "top": [[-0.8, 2.2], [0.8, 2.2], [0.9, 1.2], [0.9, -1.4], [0.85, -2.2], [-0.85, -2.2], [-0.9, -1.4], [-0.9, 1.2]],
             "front": [[-0.9, 0.3], [0.9, 0.3], [0.92, 0.85], [0.65, 1.42], [-0.65, 1.42], [-0.92, 0.85]]},
            // Wheel arches: a cylinder over each wheel (mirror "x" makes
            // the other side), subtracted; the floor between stays.
            {"shape": "cylinder", "op": "subtract", "blend": 0.03, "radius": 0.4, "height": 0.5,
             "at": [0.8, wheel_r, 1.35], "rotation": {"z": 90}},
            {"shape": "cylinder", "op": "subtract", "blend": 0.03, "radius": 0.4, "height": 0.5,
             "at": [0.8, wheel_r, -1.35], "rotation": {"z": 90}}]}
    )];
    for (i, z) in [1.35f32, -1.35].into_iter().enumerate() {
        // An axle through both wheels and the floor of the body holds them.
        items.push(
            serde_json::json!({"type": "primitive", "name": format!("axle_{i}"),
            "kind": "cylinder", "size": [0.05, 1.7], "translation": [0, wheel_r, z],
            "rotation": {"z": 90}}),
        );
        for (side, x) in [("l", 0.8f32), ("r", -0.8)] {
            items.push(
                serde_json::json!({"type": "primitive", "name": format!("wheel_{i}_{side}"),
                "kind": "cylinder", "size": [wheel_r, 0.22], "round": 0.04, "segments": 4,
                "translation": [x, wheel_r, z], "rotation": {"z": 90}}),
            );
        }
    }
    items.push(
        serde_json::json!({"type": "material", "name": "body", "color": "#b8322c",
        "roughness": 0.3, "flat": true}),
    );
    items.push(
        serde_json::json!({"type": "material", "name": "wheel_*", "color": "#1d1d1f",
        "roughness": 0.9, "flat": true}),
    );
    items.push(
        serde_json::json!({"type": "material", "name": "axle_*", "color": "#555555",
        "metalness": 1.0, "flat": true}),
    );
    let summary = s
        .apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    println!("{summary}");
    let spec: VerifySpec = serde_json::from_value(serde_json::json!({
        "validate": true, "grounded": true, "materials": true,
        "bounds": {"min": [null, 0, null], "tol": 0.01},
        "collisions": {"allow": [["axle_*", "body"]]}
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    let (ok, text) = s.verify(&spec);
    println!("{text}");
    assert!(ok, "the car failed its checks");
}
