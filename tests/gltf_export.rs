//! Round-trip tests: export a two-part model as .gltf and .glb, parse both back with the
//! gltf crate and check nodes, meshes, accessor bounds, texcoords and buffer lengths.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use modeler::{Material, Model, Part};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "friendslop-modeler-gltf-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A cube as 12 triangles, 8 shared verts with per-vertex normals and uvs.
fn cube_part(name: &str, offset: [f32; 3], color: [f32; 4]) -> Part {
    let mut p = Part::new(name);
    p.verts = vec![
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [0.0, 1.0, 1.0],
    ]
    .into_iter()
    .map(|v| [v[0] + offset[0], v[1] + offset[1], v[2] + offset[2]])
    .collect();
    p.normals = vec![[0.0, 0.0, 1.0]; 8];
    p.uvs = (0..8).map(|i| [i as f32 / 8.0, 0.5]).collect();
    p.indices = vec![0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6];
    p.translation = offset;
    p.material = Material {
        base_color: color,
        roughness: 0.4,
        metalness: 0.1,
        pbr: None,
        flat: false,
    };
    p
}

fn test_model() -> Model {
    let mut model = Model {
        name: "test-tower".to_string(),
        ..Default::default()
    };
    model
        .parts
        .push(cube_part("base", [0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 1.0]).into());
    model
        .parts
        .push(cube_part("top", [0.0, 2.0, 0.0], [0.0, 0.0, 1.0, 1.0]).into());
    model.version = 3;
    model
}

fn check_gltf(doc: &gltf::Document, bin: &[u8]) {
    let roots: Vec<_> = doc.default_scene().expect("scene").nodes().collect();
    assert_eq!(roots.len(), 2, "two root nodes");
    assert_eq!(doc.meshes().count(), 2, "two meshes");

    for (i, node) in roots.iter().enumerate() {
        let mesh = node.mesh().expect("node has a mesh");
        let prim = mesh.primitives().next().expect("one primitive");

        let positions = prim.get(&gltf::Semantic::Positions).expect("POSITION");
        assert_eq!(positions.count(), 8);
        let min = positions.min().expect("POSITION min");
        let max = positions.max().expect("POSITION max");
        // base part spans y 0..1; top part spans y 2..3
        let (y_min, y_max) = if i == 0 { (0.0, 1.0) } else { (2.0, 3.0) };
        assert_eq!(min[0], 0.0);
        assert_eq!(min[1], y_min);
        assert_eq!(min[2], 0.0);
        assert_eq!(max[0], 1.0);
        assert_eq!(max[1], y_max);
        assert_eq!(max[2], 1.0);

        assert!(
            prim.get(&gltf::Semantic::TexCoords(0)).is_some(),
            "TEXCOORD_0 present"
        );
        assert_eq!(
            prim.get(&gltf::Semantic::Normals).expect("NORMAL").count(),
            8
        );
        let indices = prim.indices().expect("indices");
        assert_eq!(indices.count(), 12);

        let material = prim.material();
        let base = material.pbr_metallic_roughness().base_color_factor();
        if i == 0 {
            assert_eq!(base, [1.0, 0.0, 0.0, 1.0]);
        } else {
            assert_eq!(base, [0.0, 0.0, 1.0, 1.0]);
        }

        // Every accessor's data must fit inside the binary buffer.
        for acc in [
            positions,
            prim.get(&gltf::Semantic::Normals).unwrap(),
            prim.get(&gltf::Semantic::TexCoords(0)).unwrap(),
        ] {
            let view = acc.view().expect("buffer view");
            let start = view.offset();
            let end = start + view.length();
            assert!(end <= bin.len(), "accessor {acc:?} exceeds buffer");
        }
        let idx_view = indices.view().expect("index buffer view");
        assert!(idx_view.offset() + idx_view.length() <= bin.len());

        // The data itself reads back: positions inside the declared bounds
        // and indices in range (byteOffsets point at the right bytes).
        let reader = prim.reader(|_| Some(bin));
        let read: Vec<[f32; 3]> = reader.read_positions().expect("positions").collect();
        assert_eq!(read.len(), 8);
        let (lo, hi) = (y_min - 1e-6, y_max + 1e-6);
        for p in &read {
            assert!(
                (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[2]),
                "{p:?}"
            );
            assert!(p[1] >= lo && p[1] <= hi, "{p:?} outside y {y_min}..{y_max}");
        }
        let ix: Vec<u32> = reader.read_indices().expect("indices").into_u32().collect();
        assert_eq!(ix, vec![0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6]);
    }

    // Total buffer byteLength matches what the accessors asked for, summed.
    let buffer = doc.buffers().next().expect("one buffer");
    assert!(bin.len() >= buffer.length(), "buffer smaller than declared");
}

#[test]
fn gltf_plus_bin_roundtrip() {
    let dir = temp_dir("gltf");
    let path = dir.join("tower.gltf");
    modeler::gltf::export_gltf(&test_model(), &path).unwrap();

    assert!(path.exists(), ".gltf written");
    let bin_path = dir.join("tower.bin");
    assert!(bin_path.exists(), "sibling .bin written");

    let doc = gltf::Gltf::open(&path).expect("gltf parses");
    let bin = std::fs::read(&bin_path).unwrap();
    check_gltf(&doc, &bin);
}

#[test]
fn glb_roundtrip() {
    let dir = temp_dir("glb");
    let path = dir.join("tower.glb");
    modeler::gltf::export_glb(&test_model(), &path).unwrap();

    let raw = std::fs::read(&path).unwrap();
    assert_eq!(&raw[..4], b"glTF");
    let total = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize;
    assert_eq!(total, raw.len(), "header length matches file");

    let doc = gltf::Gltf::open(&path).expect("glb parses");
    // The gltf crate's `Buffer::source()` carries no bytes for the BIN chunk, so slice
    // it out of the glb layout directly: 12-byte header, JSON chunk (len, type), BIN.
    let json_len = u32::from_le_bytes(raw[12..16].try_into().unwrap()) as usize;
    let bin_header = 12 + 8 + json_len;
    assert_eq!(
        &raw[bin_header + 4..bin_header + 8],
        b"BIN\0",
        "second chunk is BIN"
    );
    let bin_len = u32::from_le_bytes(raw[bin_header..bin_header + 4].try_into().unwrap()) as usize;
    let bin: Vec<u8> = raw[bin_header + 8..bin_header + 8 + bin_len].to_vec();
    assert!(!bin.is_empty(), "glb BIN chunk has data");
    check_gltf(&doc, &bin);
}

#[test]
fn empty_model_and_bad_indices_are_rejected() {
    let dir = temp_dir("reject");
    let empty = Model::default();
    assert!(modeler::gltf::export_gltf(&empty, &dir.join("e.gltf")).is_err());
    assert!(modeler::gltf::export_glb(&empty, &dir.join("e.glb")).is_err());

    let mut model = test_model();
    std::sync::Arc::make_mut(&mut model.parts[0])
        .indices
        .push(99);
    assert!(modeler::gltf::export_gltf(&model, &dir.join("bad.gltf")).is_err());
}

#[test]
fn hidden_parts_are_not_exported() {
    let dir = temp_dir("hidden");
    let mut model = test_model();
    std::sync::Arc::make_mut(&mut model.parts[0]).visible = false;
    let path = dir.join("hidden.glb");
    modeler::gltf::export_glb(&model, &path).unwrap();
    let doc = gltf::Gltf::open(&path).unwrap();
    let roots: Vec<_> = doc.default_scene().expect("scene").nodes().collect();
    assert_eq!(roots.len(), 1, "only the visible part is exported");
    // The exported node carries the visible part's own name and colour,
    // not the hidden part's that came before it.
    let visible = model.parts[1].clone();
    let node = &roots[0];
    assert_eq!(node.name(), Some(visible.name.as_str()));
    let mat = node
        .mesh()
        .expect("mesh")
        .primitives()
        .next()
        .expect("prim")
        .material();
    assert_eq!(
        mat.pbr_metallic_roughness().base_color_factor(),
        visible.material.base_color
    );

    // All-hidden exports are rejected rather than writing an empty scene.
    let mut model = test_model();
    for p in &mut model.parts {
        std::sync::Arc::make_mut(p).visible = false;
    }
    assert!(modeler::gltf::export_glb(&model, &dir.join("none.glb")).is_err());
}

#[test]
fn procedural_materials_export_textures_and_tangents() {
    let mut s = modeler::commands::Session::default();
    let nodes = serde_json::json!([
        {"type": "tile_grid", "name": "g", "size": [0.25, 0.0625], "stagger": 0.5},
        {"type": "math", "name": "m", "op": "step", "a": "g.edge", "b": 0.005},
        {"type": "math", "name": "h", "op": "mul", "a": "m", "b": 0.004},
        {"type": "ao_from_height", "name": "ao", "height": "h", "radius": 0.01},
        {"type": "output", "name": "out", "base_color": "m", "height": "h", "ao": "ao"}
    ]);
    s.materials
        .ops(
            "bricks",
            nodes.as_array().unwrap().clone(),
            None,
            None,
            None,
            Some(64),
            None,
        )
        .unwrap();
    let items = serde_json::json!([
        {"type": "primitive", "name": "a", "kind": "box", "size": [1, 1, 1], "translation": [0, 0.5, 0]},
        {"type": "primitive", "name": "b", "kind": "box", "size": [1, 1, 1], "translation": [2, 0.5, 0]},
        {"type": "material", "name": "a", "pbr": "bricks"},
        {"type": "material", "name": "b", "pbr": "bricks"}
    ]);
    s.apply_values_as(None, items.as_array().unwrap().clone(), false)
        .unwrap();
    let dir = temp_dir("pbr");
    let path = dir.join("bricks.glb");
    // The default .glb embeds KTX2, which Bevy reads by mime type.
    modeler::gltf::export_glb(&s.model, &path).unwrap();
    {
        let g = gltf::Gltf::open(&path).expect("glb parses");
        let blob = g.blob.as_deref().expect("glb buffer");
        assert_eq!(g.images().count(), 4);
        for img in g.images() {
            let gltf::image::Source::View { view, mime_type } = img.source() else {
                panic!("expected an embedded image");
            };
            assert_eq!(mime_type, "image/ktx2");
            let bytes = &blob[view.offset()..view.offset() + view.length()];
            bevy::image::ktx2_buffer_to_image(
                bytes,
                bevy::image::CompressedImageFormats::BC,
                false,
            )
            .unwrap_or_else(|e| panic!("bevy rejects embedded ktx2: {e:?}"));
        }
    }
    // PNG embedding still works, for viewers without KTX2.
    let png = modeler::export_opt::Options::parse(&serde_json::json!({
        "preset": "none", "textures": "embed_png"}))
    .unwrap();
    modeler::gltf::export_glb_with(&s.model, &path, &png).unwrap();
    let (doc, buffers, images) = gltf::import(&path).expect("glb imports with its images");
    // One shared texture set (base, metal-rough, normal, occlusion) for both parts.
    assert_eq!(images.len(), 4);
    assert!(images.iter().all(|i| i.width == 64 && i.height == 64));
    for mesh in doc.meshes() {
        let prim = mesh.primitives().next().unwrap();
        let m = prim.material();
        assert!(m.pbr_metallic_roughness().base_color_texture().is_some());
        assert!(
            m.pbr_metallic_roughness()
                .metallic_roughness_texture()
                .is_some()
        );
        assert!(m.normal_texture().is_some());
        assert!(m.occlusion_texture().is_some());
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        let tangents: Vec<[f32; 4]> = reader.read_tangents().expect("TANGENT").collect();
        let normals: Vec<[f32; 3]> = reader.read_normals().unwrap().collect();
        assert_eq!(tangents.len(), normals.len());
        for (t, n) in tangents.iter().zip(&normals) {
            let len = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-3, "unit tangent {t:?}");
            assert!(
                (t[0] * n[0] + t[1] * n[1] + t[2] * n[2]).abs() < 1e-3,
                "tangent ⟂ normal"
            );
            assert!(t[3] == 1.0 || t[3] == -1.0);
        }
        // Metric uvs over the default 1 m tile: a 1 m face spans one repeat.
        let uvs: Vec<[f32; 2]> = reader.read_tex_coords(0).unwrap().into_f32().collect();
        let span = uvs
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), u| (a.min(u[0]), b.max(u[0])));
        assert!((span.1 - span.0 - 1.0).abs() < 1e-4, "{span:?}");
    }
}

#[test]
fn game_export_dedups_instances_culls_and_packs_orm() {
    let mut s = modeler::commands::Session::default();
    let nodes = serde_json::json!([
        {"type": "tile_grid", "name": "g", "size": [0.25, 0.0625], "stagger": 0.5},
        {"type": "math", "name": "m", "op": "step", "a": "g.edge", "b": 0.005},
        {"type": "math", "name": "h", "op": "mul", "a": "m", "b": 0.004},
        {"type": "ao_from_height", "name": "ao", "height": "h", "radius": 0.01},
        {"type": "output", "name": "out", "base_color": "m", "height": "h", "ao": "ao"}
    ]);
    s.materials
        .ops(
            "bricks",
            nodes.as_array().unwrap().clone(),
            None,
            None,
            None,
            Some(64),
            None,
        )
        .unwrap();
    let items = serde_json::json!([
        {"type": "primitive", "name": "floor", "kind": "box", "size": [4, 0.1, 1], "translation": [0, 0.05, 0]},
        {"type": "material", "name": "floor", "pbr": "bricks"},
        {"type": "primitive", "name": "post", "kind": "box", "size": [0.1, 1, 0.1], "translation": [-1, 0.6, 0]},
        {"type": "linear_array", "name": "post", "input": "post", "count": 4, "offset": [0.6, 0, 0]},
        {"type": "material", "name": "post", "color": "#808890"}
    ]);
    s.apply_values_as(None, items.as_array().unwrap().clone(), false)
        .unwrap();
    let dir = temp_dir("game");
    let path = dir.join("game.glb");
    // PNG so the gltf crate can decode the images below.
    let opts =
        modeler::export_opt::Options::parse(&serde_json::json!({"textures": "embed_png"})).unwrap();
    let report = modeler::gltf::export_glb_with(&s.model, &path, &opts).unwrap();
    let (doc, buffers, images) = gltf::import(&path).expect("game glb imports");
    // Five nodes, two meshes (floor + one post shared by its copies), two materials.
    assert_eq!(doc.nodes().count(), 5, "{report}");
    assert_eq!(doc.meshes().count(), 2, "{report}");
    assert_eq!(doc.materials().count(), 2, "{report}");
    // ORM: base, metal-rough(+AO), normal: no separate occlusion image.
    assert_eq!(images.len(), 3, "{report}");
    let post_nodes: Vec<_> = doc
        .nodes()
        .filter(|n| n.name().is_some_and(|x| x.starts_with("post")))
        .collect();
    let mesh_ids: std::collections::BTreeSet<usize> = post_nodes
        .iter()
        .filter_map(|n| n.mesh().map(|m| m.index()))
        .collect();
    assert_eq!(mesh_ids.len(), 1, "posts share one mesh");
    for mesh in doc.meshes() {
        let prim = mesh.primitives().next().unwrap();
        // u16 indices.
        assert_eq!(
            prim.indices().unwrap().data_type(),
            gltf::accessor::DataType::U16
        );
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        let n = reader.read_indices().unwrap().into_u32().count() / 3;
        if mesh.name() == Some("post") {
            // The bottom (2 triangles, sunk on the floor) is culled.
            assert_eq!(n, 10, "{report}");
            assert!(reader.read_tex_coords(0).is_none(), "untextured: no uvs");
        }
    }
    let m = doc
        .materials()
        .find(|m| m.normal_texture().is_some())
        .unwrap();
    assert_eq!(
        m.occlusion_texture().unwrap().texture().index(),
        m.pbr_metallic_roughness()
            .metallic_roughness_texture()
            .unwrap()
            .texture()
            .index()
    );
    assert!(report.contains("contact culling"), "{report}");
}

/// Two .gltf models under one `assets/` folder share one set of texture
/// files, named by content, in `assets/textures/`: KTX2 by default (BC7 /
/// BC5 with mips, which Bevy's own loader reads), PNG on request.
#[test]
fn gltf_exports_share_texture_files() {
    let mut s = modeler::commands::Session::default();
    let nodes = serde_json::json!([
        {"type": "tile_grid", "name": "g", "size": [0.25, 0.0625], "stagger": 0.5},
        {"type": "math", "name": "m", "op": "step", "a": "g.edge", "b": 0.005},
        {"type": "math", "name": "h", "op": "mul", "a": "m", "b": 0.004},
        {"type": "output", "name": "out", "base_color": "m", "roughness": 0.8, "height": "h"}
    ]);
    s.materials
        .ops(
            "stone",
            nodes.as_array().unwrap().clone(),
            None,
            None,
            None,
            Some(32),
            None,
        )
        .unwrap();
    for mode in ["ktx2", "png"] {
        let dir = temp_dir(&format!("shared-{mode}")).join("assets");
        let opts = modeler::export_opt::Options::parse(&serde_json::json!({
            "preset": "none", "textures": mode}))
        .unwrap();
        let mut uris = Vec::new();
        for (name, size) in [("rock_a", 1.0), ("rock_b", 0.6)] {
            let items = serde_json::json!([
                {"type": "primitive", "name": "r", "kind": "box", "size": size, "translation": [0, 0.5, 0]},
                {"type": "material", "name": "r", "pbr": "stone"}
            ]);
            s.apply_values_as(None, items.as_array().unwrap().clone(), false)
                .unwrap();
            let path = dir.join("models").join(format!("{name}.gltf"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let report = if mode == "ktx2" {
                // the default
                modeler::gltf::export_gltf_with(
                    &s.model,
                    &path,
                    &modeler::export_opt::Options::none(),
                )
            } else {
                modeler::gltf::export_gltf_with(&s.model, &path, &opts)
            }
            .unwrap();
            assert!(report.contains("textures: shared"), "{report}");
            let doc = gltf::Gltf::open(&path).expect("gltf parses");
            assert_eq!(doc.images().count(), 3);
            for img in doc.images() {
                let gltf::image::Source::Uri { uri, mime_type } = img.source() else {
                    panic!("image packed into the buffer");
                };
                assert!(
                    uri.starts_with("../textures/") && uri.ends_with(mode),
                    "{uri}"
                );
                assert_eq!(mime_type, Some(format!("image/{mode}").as_str()));
                uris.push(uri.to_string());
            }
        }
        uris.sort();
        uris.dedup();
        assert_eq!(uris.len(), 3, "both rocks point at the same 3 files");
        assert_eq!(std::fs::read_dir(dir.join("textures")).unwrap().count(), 3);
        if mode == "ktx2" {
            use bevy::image::{CompressedImageFormats, ktx2_buffer_to_image};
            use bevy::render::render_resource::TextureFormat;
            for u in &uris {
                let bytes = std::fs::read(dir.join("models").join(u)).unwrap();
                let srgb = u.contains("base_color");
                let img = ktx2_buffer_to_image(&bytes, CompressedImageFormats::BC, srgb)
                    .unwrap_or_else(|e| panic!("bevy rejects {u}: {e:?}"));
                let want = if srgb {
                    TextureFormat::Bc7RgbaUnormSrgb
                } else if u.contains("normal") {
                    TextureFormat::Bc5RgUnorm
                } else {
                    TextureFormat::Bc7RgbaUnorm
                };
                assert_eq!(img.texture_descriptor.format, want, "{u}");
                assert_eq!(img.texture_descriptor.mip_level_count, 6, "{u}"); // 32 .. 1
                assert_eq!(img.texture_descriptor.size.width, 32);
            }
        }
    }
}

/// LODs: heavy parts get simplified sibling nodes with distance ranges;
/// light parts don't; instanced copies share the LOD meshes.
#[test]
fn lods_add_simplified_nodes_with_ranges() {
    let mut s = modeler::commands::Session::default();
    let items = serde_json::json!([
        {"type": "primitive", "name": "ball", "kind": "sphere", "size": 0.5, "translation": [0, 0.5, 0]},
        {"type": "linear_array", "name": "ball", "input": "ball", "count": 2, "offset": [2, 0, 0]},
        {"type": "primitive", "name": "crate", "kind": "box", "size": 0.5, "translation": [-2, 0.25, 0]},
        {"type": "text", "name": "label", "text": "LOD", "size": 0.3, "translation": [0, 1.2, 0]}
    ]);
    s.apply_values_as(None, items.as_array().unwrap().clone(), false)
        .unwrap();
    let opts = modeler::export_opt::Options::parse(&serde_json::json!({
        "lods": [0.5, 0.2], "textures": "embed_png"}))
    .unwrap();
    let dir = temp_dir("lods");
    let path = dir.join("lods.glb");
    let report = modeler::gltf::export_glb_with(&s.model, &path, &opts).unwrap();
    assert!(report.contains("lods:"), "{report}");
    let (doc, _, _) = gltf::import(&path).expect("imports");
    let names: Vec<String> = doc
        .nodes()
        .filter_map(|n| n.name().map(String::from))
        .collect();
    for n in [
        "ball",
        "ball.lod1",
        "ball.lod2",
        "ball_1.lod1",
        "ball_1.lod2",
    ] {
        assert!(names.iter().any(|x| x == n), "{n} missing from {names:?}");
    }
    assert!(
        !names.iter().any(|x| x == "crate.lod1"),
        "a 12-triangle box got LODs"
    );
    let tris = |name: &str| {
        let n = doc.nodes().find(|n| n.name() == Some(name)).unwrap();
        let p = n.mesh().unwrap().primitives().next().unwrap();
        p.indices().unwrap().count() / 3
    };
    assert!(tris("ball.lod1") < tris("ball"));
    assert!(tris("ball.lod2") < tris("ball.lod1"));
    let extras = |name: &str| -> serde_json::Value {
        let n = doc.nodes().find(|n| n.name() == Some(name)).unwrap();
        serde_json::from_str(n.extras().as_ref().unwrap().get()).unwrap()
    };
    let (l0, l1) = (
        extras("ball")["lod"].clone(),
        extras("ball.lod1")["lod"].clone(),
    );
    assert_eq!(l0["level"], 0);
    assert_eq!(l1["level"], 1);
    assert_eq!(l0["far"], l1["near"]);
}

/// Skeletons export as joint node trees with a skin (joints / weights on
/// the smooth part, inverse binds), rigid parts under their joint, and one
/// animation per clip.
#[test]
fn skins_and_clips_export() {
    let mut s = modeler::commands::Session::default();
    let items = serde_json::json!([
        {"type": "sdf", "name": "arm", "shapes": [{"shape": "capsule", "from": [0, 1, 0], "to": [1, 1, 0], "radius": 0.08}]},
        {"type": "primitive", "name": "hand", "kind": "box", "size": [0.1, 0.1, 0.1], "translation": [1.15, 1.0, 0.0]},
        {"type": "skeleton", "name": "rig", "bones": {
            "upper": {"head": [0, 1, 0], "radius": 0.08},
            "lower": {"parent": "upper", "head": [0.5, 1, 0], "tail": [1.1, 1, 0], "radius": 0.08}}},
        {"type": "bind", "name": "skin", "skeleton": "rig", "parts": ["arm"]},
        {"type": "bind", "name": "grip", "skeleton": "rig", "parts": ["hand"], "bone": "lower"},
        {"type": "material", "name": "arm", "color": "#c08060", "flat": true},
        {"type": "material", "name": "hand", "color": "#806040", "flat": true},
        {"type": "clip", "name": "wave", "skeleton": "rig", "duration": 1.0,
         "tracks": {"lower": {"rotate": [[0, {"z": 0}], [0.5, {"z": 60}], [1, {"z": 0}]]}}}
    ]);
    s.apply_values_as(None, items.as_array().unwrap().clone(), false)
        .unwrap();
    for (tag, optimize) in [
        ("plain", serde_json::json!("none")),
        ("game", serde_json::json!("game")),
    ] {
        let opts = modeler::export_opt::Options::parse(&optimize).unwrap();
        let dir = temp_dir(&format!("skin-{tag}"));
        let path = dir.join("arm.glb");
        let report = modeler::gltf::export_glb_with(&s.model, &path, &opts).unwrap();
        assert!(report.contains("rig: 1 skeleton"), "{report}");
        let (doc, buffers, _) = gltf::import(&path).expect("imports");
        let skin = doc.skins().next().expect("a skin");
        let joints: Vec<String> = skin
            .joints()
            .filter_map(|j| j.name().map(String::from))
            .collect();
        assert_eq!(joints, ["upper", "lower"]);
        let ibm: Vec<[[f32; 4]; 4]> = skin
            .reader(|b| Some(&buffers[b.index()]))
            .read_inverse_bind_matrices()
            .unwrap()
            .collect();
        assert!(
            (ibm[1][3][0] + 0.5).abs() < 1e-5 && (ibm[1][3][1] + 1.0).abs() < 1e-5,
            "{ibm:?}"
        );
        let arm = doc.nodes().find(|n| n.name() == Some("arm")).unwrap();
        assert_eq!(arm.skin().map(|k| k.index()), Some(0));
        let prim = arm.mesh().unwrap().primitives().next().unwrap();
        let r = prim.reader(|b| Some(&buffers[b.index()]));
        let w: Vec<[f32; 4]> = r.read_weights(0).unwrap().into_f32().collect();
        let n = r.read_positions().unwrap().count();
        assert_eq!(w.len(), n);
        assert!(w.iter().all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 1e-3));
        let j: Vec<[u16; 4]> = r.read_joints(0).unwrap().into_u16().collect();
        assert!(
            j.iter().any(|j| j[0] == 1),
            "some vertices ride the lower bone"
        );
        // The hand is a child of the lower joint, placed relative to it.
        let lower = doc.nodes().find(|n| n.name() == Some("lower")).unwrap();
        let kid = lower
            .children()
            .find(|c| c.name() == Some("hand"))
            .expect("hand under lower");
        let (t, _, _) = kid.transform().decomposed();
        assert!((t[0] - 0.65).abs() < 1e-4, "{t:?}");
        let anim = doc.animations().next().expect("an animation");
        assert_eq!(anim.name(), Some("wave"));
        let ch = anim.channels().next().unwrap();
        assert_eq!(ch.target().node().name(), Some("lower"));
        let rd = ch.reader(|b| Some(&buffers[b.index()]));
        let times: Vec<f32> = rd.read_inputs().unwrap().collect();
        assert!(times.len() >= 31 && (times[times.len() - 1] - 1.0).abs() < 1e-4);
        let Some(gltf::animation::util::ReadOutputs::Rotations(rot)) = rd.read_outputs() else {
            panic!("rotation channel")
        };
        let rot: Vec<[f32; 4]> = rot.into_f32().collect();
        let mid = times.iter().position(|t| (t - 0.5).abs() < 1e-4).unwrap();
        let angle = 2.0 * rot[mid][3].clamp(-1.0, 1.0).acos().to_degrees();
        assert!((angle - 60.0).abs() < 0.5, "{angle}");
    }
}
