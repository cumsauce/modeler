//! A map document exported as a .bsn scene: jackdaw's own parser reads it,
//! prefabs are referenced (exported once), components come from the
//! vocabulary, and terrain heights round-trip.

#![allow(clippy::unwrap_used)]

use modeler::commands::Session;
use serde_json::json;

#[test]
fn map_exports_as_a_jackdaw_scene() {
    let tmp = std::env::temp_dir().join(format!("modeler-scene-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &tmp) };
    let lib = tmp.join("friendslop").join("modeler");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(
        lib.join("components.json"),
        json!({"header": ["// SALT: required comment"],
               "components": {
                 "Place": {"type": "my_game::places::Place", "fields": {"name": "string", "radius": "f32"}},
                 "Spawn": {"type": "my_game::Spawn"},
                 "Hangout": {"type": "my_game::Hangout"},
                 "Path": {"type": "my_game::Path", "fields": {"to": "refs", "from": "ref", "tags": "strings"}}}})
        .to_string(),
    )
    .unwrap();

    // A prefab in the catalog.
    let mut p = Session::default();
    p.apply_values(None, vec![json!({"type": "primitive", "name": "box", "kind": "box", "size": 1, "translation": [0, 0.5, 0]})])
        .unwrap();
    p.model.name = "crate".into();
    modeler::catalog::save_prefab(&p.model, "a crate", &[], "").unwrap();

    let mut s = Session::default();
    // Unknown components fail at apply time.
    let bad = s.apply_values(
        None,
        vec![json!({"type": "marker", "name": "m", "kind": "spawn", "components": {"Nope": {}}})],
    );
    assert!(bad.is_err_and(|e| e.to_string().contains("unknown `Nope`")));
    let bad = s.apply_values(None, vec![json!({"type": "marker", "name": "m", "kind": "spawn", "components": {"Place": {"radius": "x"}}})]);
    assert!(bad.is_err());
    s.apply_values(
        None,
        vec![
            json!({"type": "terrain", "name": "ground", "size": [20, 20], "resolution": 1, "height": 2, "seed": 3}),
            json!({"type": "prefab", "name": "crate1", "prefab": "crate", "translation": [2, 0, 1],
                   "components": {"Place": {"name": "depot", "radius": 4}}}),
            json!({"type": "prefab", "name": "crate2", "prefab": "crate", "translation": [-3, 0, 0]}),
            json!({"type": "marker", "name": "spawn", "kind": "spawn", "translation": [0, 1, 0], "yaw": 90,
                   "components": {"Spawn": {}, "Hangout": {}}}),
            json!({"type": "marker", "name": "path", "kind": "path", "size": [2, 1, 3],
                   "components": {"Path": {"from": "spawn", "to": ["crate1", "crate2"], "tags": ["dirt"]}}}),
        ],
    )
    .unwrap();
    let out = tmp.join("assets").join("maps").join("commune.bsn");
    let report = modeler::scene::export_scene(&s.model, &out, &Default::default()).unwrap();
    assert!(report.contains("exported prefab `crate`"), "{report}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with(modeler::scene::STAMP));
    assert!(text.contains("// SALT: required comment"));
    assert!(text.contains("my_game::places::Place"));
    assert!(text.contains("radius: 4.0"));
    assert!(text.contains("\"models/prefabs/crate.gltf\""), "{text}");
    assert!(text.contains("\"maps/commune.gltf\""), "{text}");
    assert!(tmp.join("assets/models/prefabs/crate.gltf").exists());
    assert!(text.contains(r#"to: ["crate1", "crate2"]"#), "{text}");
    assert!(text.contains("    my_game::Hangout\n"), "{text}");
    jackdaw_bsn::parse_bsn_text(&text).unwrap_or_else(|e| panic!("{e:?}\n{text}"));

    // The plain data file: rotation, yaw, kind and size, components as written.
    let map: modeler::scene::MapFile = serde_json::from_str(
        &std::fs::read_to_string(tmp.join("assets/maps/commune.map.json")).unwrap(),
    )
    .unwrap();
    let spawn = map.entities.iter().find(|e| e.name == "spawn").unwrap();
    assert_eq!(spawn.kind.as_deref(), Some("spawn"));
    assert!((spawn.yaw - 90.0).abs() < 1e-3, "{}", spawn.yaw);
    let path = map.entities.iter().find(|e| e.name == "path").unwrap();
    assert_eq!(path.size, Some([2.0, 1.0, 3.0]));
    assert_eq!(path.components["Path"]["to"], json!(["crate1", "crate2"]));
    assert_eq!(map.terrains[0].heights, "maps/commune.ground.heights");

    // Heights round-trip.
    let bytes = std::fs::read(tmp.join("assets/maps/commune.ground.heights")).unwrap();
    let (nx, nz, x0, z0, cell, h, _) = modeler::scene::read_heights(&bytes).unwrap();
    assert_eq!((nx, nz), (21, 21));
    assert!((x0 + 10.0).abs() < 1e-4 && (z0 + 10.0).abs() < 1e-4 && (cell - 1.0).abs() < 1e-4);
    let modeler::Op::Terrain { def, .. } = s
        .model
        .ops
        .iter()
        .find(|o| matches!(o, modeler::Op::Terrain { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    let want = def.height_at(x0 + 3.0 * cell, z0 + 5.0 * cell);
    assert!(
        (h[5 * nx + 3] - want).abs() < 2e-3,
        "{} vs {want}",
        h[5 * nx + 3]
    );

    // A second export leaves the current prefab glTF alone and is identical.
    let again = modeler::scene::export_scene(&s.model, &out, &Default::default()).unwrap();
    assert!(!again.contains("exported prefab"), "{again}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), text);

    // A reference to an entity the map doesn't have fails the export.
    s.apply_values(
        None,
        vec![json!({"type": "marker", "name": "path", "kind": "path",
                    "components": {"Path": {"to": ["crate1", "crate9"]}}})],
    )
    .unwrap();
    let err = modeler::scene::export_scene(&s.model, &out, &Default::default()).unwrap_err();
    assert!(err.to_string().contains("`crate9`"), "{err}");
}
