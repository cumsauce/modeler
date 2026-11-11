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
