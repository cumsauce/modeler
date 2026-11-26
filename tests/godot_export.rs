//! A map document exported as a Godot 4 scene (`.tscn`): instanced glTFs,
//! Marker3D markers with metadata, components as a dictionary, colliders as
//! physics nodes, PNG textures. When a `godot` binary is around
//! (`MODELER_GODOT`, else `godot` on PATH) the scene is imported and loaded
//! by Godot itself and the result checked; without one that part is skipped.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use modeler::commands::Session;
use modeler::scene::{SceneFormat, SceneOptions};
use serde_json::json;

fn godot() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MODELER_GODOT") {
        return Some(p.into());
    }
    let out = std::process::Command::new("sh")
        .args(["-c", "command -v godot"])
        .output()
        .ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then(|| p.into())
}

const CHECK: &str = r#"extends SceneTree

func _init():
	var ps = load("res://assets/maps/commune.tscn")
	var root = ps.instantiate()
	var out = {}
	walk(root, root, out)
	print("RESULT " + JSON.stringify(out))
	quit()

func walk(n, root, out):
	var d = {"type": n.get_class()}
	if n is Node3D:
		var t = n.transform
		d["origin"] = [t.origin.x, t.origin.y, t.origin.z]
		d["z"] = [t.basis.z.x, t.basis.z.y, t.basis.z.z]
	for k in n.get_meta_list():
		d["meta_" + k] = var_to_str(n.get_meta(k))
	if n is CollisionShape3D and n.shape:
		d["shape"] = n.shape.get_class()
	if n is MeshInstance3D and n.mesh:
		var m = n.mesh.surface_get_material(0)
		if m is BaseMaterial3D:
			d["albedo_tex"] = m.albedo_texture != null
	out[str(root.get_path_to(n))] = d
	for c in n.get_children():
		walk(c, root, out)
"#;

fn run(godot: &Path, project: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new(godot)
        .arg("--headless")
        .arg("--path")
        .arg(project)
        .args(args)
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn map_exports_as_a_godot_scene() {
    let tmp = std::env::temp_dir().join(format!("modeler-godot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &tmp) };
    let lib = tmp.join("friendslop").join("modeler");
    std::fs::create_dir_all(&lib).unwrap();
    // Godot-only projects may leave out `type` (the Rust path).
    std::fs::write(
        lib.join("components.json"),
        json!({"header": ["// SALT: required comment"],
               "components": {
                 "Place": {"fields": {"name": "string", "radius": "f32", "at": "vec3"}},
                 "Spawn": {},
                 "Path": {"fields": {"to": "refs", "from": "ref"}}}})
        .to_string(),
    )
    .unwrap();
    let game = tmp.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("project.godot"), "config_version=5\n").unwrap();

    // A prefab with a box collider.
    let mut p = Session::default();
    p.apply_values(
        None,
        vec![
            json!({"type": "primitive", "name": "box", "kind": "box", "size": 1, "translation": [0, 0.5, 0]}),
            json!({"type": "collider", "name": "c", "parts": ["box"], "shape": "box"}),
        ],
    )
    .unwrap();
    p.model.name = "crate".into();
    modeler::catalog::save_prefab(&p.model, "a crate", &[], "").unwrap();

    let mut s = Session::default();
    let nodes = json!([
        {"type": "tile_grid", "name": "g", "size": [0.25, 0.0625], "stagger": 0.5},
        {"type": "math", "name": "m", "op": "step", "a": "g.edge", "b": 0.005},
        {"type": "output", "name": "out", "base_color": "m"}
    ]);
    s.materials
        .ops(
            "tiles",
            nodes.as_array().unwrap().clone(),
            None,
            None,
            None,
            Some(64),
            None,
        )
        .unwrap();
    s.apply_values(
        None,
        vec![
            json!({"type": "primitive", "name": "floor", "kind": "box", "size": [10, 0.2, 10], "translation": [0, -0.1, 0]}),
            json!({"type": "collider", "name": "floor_c", "parts": ["floor"], "shape": "mesh"}),
            json!({"type": "material", "name": "floor", "pbr": "tiles"}),
            json!({"type": "primitive", "name": "zone", "kind": "box", "size": 2, "translation": [3, 1, 3]}),
            json!({"type": "hide_part", "part": "zone"}),
            json!({"type": "collider", "name": "zone_c", "parts": ["zone"], "shape": "box", "sensor": true, "hidden": true}),
            json!({"type": "primitive", "name": "ball", "kind": "sphere", "size": 0.5, "translation": [-3, 0.25, 0]}),
            json!({"type": "collider", "name": "ball_c", "parts": ["ball"], "shape": "hull", "body": "dynamic"}),
            json!({"type": "prefab", "name": "crate1", "prefab": "crate", "translation": [2, 0, 1], "rotation": {"y": 90},
                   "components": {"Place": {"name": "depot", "radius": 4, "at": [1, 2, 3]}}}),
            json!({"type": "prefab", "name": "crate2", "prefab": "crate", "translation": [-3, 0, -2]}),
            json!({"type": "marker", "name": "spawn", "kind": "spawn", "translation": [0, 1, 0], "yaw": 90,
                   "size": [1, 2, 1], "components": {"Spawn": {}}}),
            json!({"type": "marker", "name": "path", "kind": "path",
                   "components": {"Path": {"from": "spawn", "to": ["crate1", "crate2"]}}}),
        ],
    )
    .unwrap();

    let out = game.join("assets").join("maps").join("commune.tscn");
    let opts = SceneOptions {
        format: SceneFormat::Godot,
        ..Default::default()
    };
    let report = modeler::scene::export_scene(&s.model, &out, &opts).unwrap();
    assert!(report.contains("exported prefab `crate`"), "{report}");
    assert!(
        report.contains("1 dynamic collider(s) left out"),
        "{report}"
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with("[gd_scene format=3]"), "{text}");
    assert!(text.contains("; SALT: required comment"), "{text}");
    assert!(
        text.contains(r#"path="res://assets/models/prefabs/crate.gltf""#),
        "{text}"
    );
    assert!(
        text.contains(r#"path="res://assets/maps/commune.gltf""#),
        "{text}"
    );
    assert!(text.contains(r#""radius": 4.0"#), "{text}");
    assert!(text.contains(r#""at": Vector3(1.0, 2.0, 3.0)"#), "{text}");
    assert!(
        text.contains(r#""to": PackedStringArray("crate1", "crate2")"#),
        "{text}"
    );
    assert!(
        text.contains(r#"[node name="collision" type="StaticBody3D" parent="crate1"]"#),
        "{text}"
    );
    assert!(
        text.contains(r#"[node name="commune_sensor" type="Area3D" parent="."]"#),
        "{text}"
    );
    assert!(text.contains("ConcavePolygonShape3D"), "{text}");
    // One box shape shared by both crates.
    assert_eq!(text.matches("type=\"BoxShape3D\"").count(), 2, "{text}"); // crate + zone
    // PNG textures, not KTX2.
    let gltf = std::fs::read_to_string(game.join("assets/maps/commune.gltf")).unwrap();
    assert!(!gltf.contains("image/ktx2"), "{gltf}");
    assert!(gltf.contains(".png"), "{gltf}");
    // The plain data file and a stable re-export.
    assert!(game.join("assets/maps/commune.map.json").exists());
    let again = modeler::scene::export_scene(&s.model, &out, &opts).unwrap();
    assert!(!again.contains("exported prefab"), "{again}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
    // LODs are refused.
    let lods = SceneOptions {
        format: SceneFormat::Godot,
        optimize: modeler::export_opt::Options::parse(&json!({"lods": [0.5]})).unwrap(),
        ..Default::default()
    };
    assert!(modeler::scene::export_scene(&s.model, &out, &lods).is_err());
    // So is baked AO: Godot's importer reads it on the first uv set.
    let ao = SceneOptions {
        format: SceneFormat::Godot,
        optimize: modeler::export_opt::Options::parse(&json!({"ao": true})).unwrap(),
        ..Default::default()
    };
    let err = modeler::scene::export_scene(&s.model, &out, &ao).unwrap_err();
    assert!(err.to_string().contains("optimize.ao"), "{err}");
    // A .bsn export needs the Rust paths.
    let err = modeler::scene::export_scene(
        &s.model,
        &tmp.join("game/assets/maps/x.bsn"),
        &Default::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("has no `type`"), "{err}");

    // A Godot project with no assets/ folder: res:// is its root, textures
    // shared in res://textures/.
    let bare = tmp.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(bare.join("project.godot"), "config_version=5\n").unwrap();
    let b = modeler::scene::export_scene(&s.model, &bare.join("maps/level.tscn"), &opts).unwrap();
    let bt = std::fs::read_to_string(bare.join("maps/level.tscn")).unwrap();
    assert!(
        bt.contains(r#"path="res://models/prefabs/crate.gltf""#),
        "{b}\n{bt}"
    );
    assert!(bt.contains(r#"path="res://maps/level.gltf""#), "{bt}");
    assert!(bare.join("textures").is_dir(), "{b}");

    let Some(godot) = godot() else {
        eprintln!("no godot binary: skipping the Godot import check");
        return;
    };
    std::fs::write(game.join("check.gd"), CHECK).unwrap();
    let log = run(&godot, &game, &["--import"]);
    let log = log + &run(&godot, &game, &["--script", "res://check.gd"]);
    let Some(line) = log.lines().find_map(|l| l.strip_prefix("RESULT ")) else {
        panic!("Godot didn't load the scene:\n{log}\n{text}");
    };
    let nodes: serde_json::Value = serde_json::from_str(line).unwrap();
    let get = |k: &str| {
        nodes
            .get(k)
            .unwrap_or_else(|| panic!("no `{k}` in {nodes:#}"))
    };
    let close = |v: &serde_json::Value, want: [f64; 3]| {
        (0..3).all(|i| (v[i].as_f64().unwrap() - want[i]).abs() < 1e-4)
    };
    let spawn = get("spawn");
    assert_eq!(spawn["type"], "Marker3D");
    assert_eq!(spawn["meta_kind"], "\"spawn\"");
    assert!(close(&spawn["origin"], [0.0, 1.0, 0.0]), "{spawn}");
    // Yaw 90 turns +z toward +x, the same in Godot's basis.
    assert!(close(&spawn["z"], [1.0, 0.0, 0.0]), "{spawn}");
    let crate1 = get("crate1");
    assert!(close(&crate1["origin"], [2.0, 0.0, 1.0]), "{crate1}");
    assert!(close(&crate1["z"], [1.0, 0.0, 0.0]), "{crate1}");
    assert!(
        crate1["meta_components"]
            .as_str()
            .unwrap()
            .contains("Vector3(1, 2, 3)"),
        "{crate1}"
    );
    assert_eq!(get("crate1/collision")["type"], "StaticBody3D");
    let shape = get("crate1/collision/box");
    assert_eq!(shape["shape"], "BoxShape3D");
    assert!(close(&shape["origin"], [0.0, 0.5, 0.0]), "{shape}");
    assert_eq!(
        get("commune_collision/mesh")["shape"],
        "ConcavePolygonShape3D"
    );
    assert_eq!(get("commune_sensor/zone")["shape"], "BoxShape3D");
    // The map's own glTF, with its PNG albedo texture.
    let meshes: Vec<&serde_json::Value> = nodes
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, v)| k.starts_with("commune_ground/") && v["type"] == "MeshInstance3D")
        .map(|(_, v)| v)
        .collect();
    assert!(!meshes.is_empty(), "{nodes:#}");
    assert!(meshes.iter().any(|m| m["albedo_tex"] == true), "{nodes:#}");
}
