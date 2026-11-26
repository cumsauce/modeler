//! A carve with `collider: "mesh"` exports its exact surfaces as a
//! `<scene>.collision` file (named in `map.json`) that reads back, so a game
//! can collide with sloped and curved floors without the glTF.

#![allow(clippy::unwrap_used)]

use modeler::commands::Session;
use serde_json::json;

#[test]
fn a_carved_ramp_exports_its_triangles() {
    let tmp = std::env::temp_dir().join(format!("modeler-collision-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &tmp) };
    let mut s = Session::default();
    s.apply_values(
        None,
        vec![
            json!({"type": "primitive", "name": "hall", "kind": "box", "size": [6, 3, 6], "translation": [0, 1.5, 0]}),
            // A tunnel tilted 10 degrees down toward -x: a sloped floor.
            json!({"type": "primitive", "name": "ramp", "kind": "box", "size": [12, 3, 3],
                   "translation": [-8, 0.5, 0], "rotation": {"z": 10}}),
            json!({"type": "hide_part", "part": "hall"}),
            json!({"type": "hide_part", "part": "ramp"}),
            json!({"type": "carve", "name": "fs", "cutters": ["hall", "ramp"], "collider": "mesh"}),
        ],
    )
    .unwrap();
    let out = tmp.join("assets").join("maps").join("ramp.bsn");
    let report = modeler::scene::export_scene(&s.model, &out, &Default::default()).unwrap();
    assert!(report.contains("collision:"), "{report}");
    let map: modeler::scene::MapFile = serde_json::from_str(
        &std::fs::read_to_string(tmp.join("assets/maps/ramp.map.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(map.collision.as_deref(), Some("maps/ramp.collision"));
    let bytes = std::fs::read(tmp.join("assets/maps/ramp.collision")).unwrap();
    let (verts, indices) = modeler::scene::read_collision(&bytes).unwrap();
    assert!(!indices.is_empty() && indices.len() % 3 == 0);
    // Some floor lies on the slope: below the hall's floor, out along -x.
    assert!(
        verts.iter().any(|v| v[0] < -10.0 && v[1] < -0.5),
        "the ramp's low end is in the collider"
    );
    assert!(modeler::scene::read_collision(&bytes[..bytes.len() - 2]).is_none());

    // Without mesh colliders the file goes away.
    s.apply_values(
        None,
        vec![json!({"type": "carve", "name": "fs", "cutters": ["hall", "ramp"]})],
    )
    .unwrap();
    modeler::scene::export_scene(&s.model, &out, &Default::default()).unwrap();
    assert!(!tmp.join("assets/maps/ramp.collision").exists());
}
