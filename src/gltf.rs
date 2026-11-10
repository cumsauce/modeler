//! Hand-rolled glTF 2.0 writer (JSON + .bin, and single-file .glb) on `serde_json`.
//! Writes only what we need: one scene, one root node per [`Part`], one mesh with one
//! primitive per node, PBR materials. No gltf-crate here; that dep is test-only.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::Model;

/// One part's slice layout inside the concatenated binary buffer.
struct Chunks {
    /// (bufferView index, byteOffset, byteLength)
    pos: (u32, usize, usize),
    nrm: (u32, usize, usize),
    uv: (u32, usize, usize),
    idx: (u32, usize, usize),
    pos_min: [f32; 3],
    pos_max: [f32; 3],
    vert_count: u32,
    index_count: u32,
}

fn align4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// Append `bytes` to `bin` 4-byte aligned and return (view, offset, length).
fn push_chunk(bin: &mut Vec<u8>, view: u32, bytes: &[u8]) -> (u32, usize, usize) {
    let start = bin.len();
    bin.extend_from_slice(bytes);
    bin.resize(align4(bin.len()), 0);
    (view, start, bytes.len())
}

fn le_f32s<'a, const N: usize>(src: impl IntoIterator<Item = &'a [f32; N]>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for arr in src {
        for f in arr {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    bytes
}

/// Validate the model and lay out every part's chunks in one flat binary buffer.
/// Hidden parts are skipped: they stay in the model (ops may still derive from
/// them) but are not exported.
fn layout(model: &Model) -> Result<(Vec<Chunks>, Vec<u8>), String> {
    let exported: Vec<&crate::Part> = model.parts.iter().filter(|p| p.visible).collect();
    if exported.is_empty() {
        return Err(format!(
            "model '{}' has no visible parts to export",
            model.name
        ));
    }
    let mut parts = Vec::with_capacity(exported.len());
    let mut bin: Vec<u8> = Vec::new();
    let mut view = 0u32;
    for (i, part) in exported.iter().enumerate() {
        if part.verts.is_empty() {
            return Err(format!("part {} ('{}') has no vertices", i, part.name));
        }
        if part.normals.len() != part.verts.len() || part.uvs.len() != part.verts.len() {
            return Err(format!(
                "part '{}' has {} verts but {} normals and {} uvs",
                part.name,
                part.verts.len(),
                part.normals.len(),
                part.uvs.len()
            ));
        }
        for &ix in &part.indices {
            if ix as usize >= part.verts.len() {
                return Err(format!(
                    "part '{}' index {} out of range ({} verts)",
                    part.name,
                    ix,
                    part.verts.len()
                ));
            }
        }

        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in &part.verts {
            for c in 0..3 {
                min[c] = min[c].min(v[c]);
                max[c] = max[c].max(v[c]);
            }
        }

        let pos = push_chunk(&mut bin, view, &le_f32s(&part.verts));
        view += 1;
        let nrm = push_chunk(&mut bin, view, &le_f32s(&part.normals));
        view += 1;
        let uv = push_chunk(&mut bin, view, &le_f32s(&part.uvs));
        view += 1;
        let mut idx_bytes = Vec::with_capacity(part.indices.len() * 4);
        for ix in &part.indices {
            idx_bytes.extend_from_slice(&ix.to_le_bytes());
        }
        let idx = push_chunk(&mut bin, view, &idx_bytes);
        view += 1;

        parts.push(Chunks {
            pos,
            nrm,
            uv,
            idx,
            pos_min: min,
            pos_max: max,
            vert_count: part.verts.len() as u32,
            index_count: part.indices.len() as u32,
        });
    }
    Ok((parts, bin))
}

/// One glTF accessor. `min`/`max` are only set on POSITION (required there).
fn accessor(
    view: u32,
    count: u32,
    kind: &str,
    component_type: u32,
    min_max: Option<([f32; 3], [f32; 3])>,
) -> Value {
    let mut acc = json!({
        "bufferView": view,
        "componentType": component_type,
        "count": count,
        "type": kind,
    });
    if let Some((min, max)) = min_max {
        acc["min"] = json!(min);
        acc["max"] = json!(max);
    }
    acc
}

/// Build the full glTF JSON for this model. `bin_len` is the binary's byte length and
/// `buffer_uri` is `Some` for .gltf+.bin, `None` for an embedded .glb buffer.
fn build_json(model: &Model, bin_len: usize, buffer_uri: Option<String>) -> Result<Value, String> {
    let (chunks, _) = layout(model)?;

    let mut nodes = Vec::with_capacity(model.parts.len());
    let mut meshes = Vec::with_capacity(model.parts.len());
    let mut materials = Vec::with_capacity(model.parts.len());
    let mut accessors = Vec::new();
    let mut buffer_views = Vec::new();

    for (part, c) in model.parts.iter().zip(&chunks) {
        let material_index = materials.len() as u32;
        materials.push(json!({
            "name": format!("{}-mat", part.name),
            "pbrMetallicRoughness": {
                "baseColorFactor": part.material.base_color,
                "metallicFactor": part.material.metalness,
                "roughnessFactor": part.material.roughness,
            }
        }));

        let mut push_acc = |a: Value| {
            accessors.push(a);
            (accessors.len() - 1) as u32
        };
        let pos_acc = push_acc(accessor(
            c.pos.0,
            c.vert_count,
            "VEC3",
            5126,
            Some((c.pos_min, c.pos_max)),
        ));
        let nrm_acc = push_acc(accessor(c.nrm.0, c.vert_count, "VEC3", 5126, None));
        let uv_acc = push_acc(accessor(c.uv.0, c.vert_count, "VEC2", 5126, None));
        let idx_acc = push_acc(accessor(c.idx.0, c.index_count, "SCALAR", 5125, None));

        for (view, _, len) in [c.pos, c.nrm, c.uv, c.idx] {
            buffer_views.push(json!({
                "buffer": 0,
                "byteOffset": view_offset(view, &chunks),
                "byteLength": len,
            }));
        }

        let mesh_index = meshes.len() as u32;
        meshes.push(json!({
            "name": part.name,
            "primitives": [{
                "attributes": {
                    "POSITION": pos_acc,
                    "NORMAL": nrm_acc,
                    "TEXCOORD_0": uv_acc,
                },
                "indices": idx_acc,
                "material": material_index,
            }]
        }));

        nodes.push(json!({
            "name": part.name,
            "translation": part.translation,
            "rotation": part.rotation,
            "scale": part.scale,
            "mesh": mesh_index,
        }));
    }

    let mut buffer_obj = json!({ "byteLength": bin_len });
    if let Some(uri) = buffer_uri {
        buffer_obj["uri"] = json!(uri);
    }

    Ok(json!({
        "asset": { "version": "2.0", "generator": "modeler" },
        "scene": 0,
        "scenes": [{ "nodes": (0..nodes.len() as u32).collect::<Vec<_>>() }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": buffer_views,
        "buffers": [buffer_obj],
    }))
}

fn view_offset(view: u32, chunks: &[Chunks]) -> usize {
    for c in chunks {
        for (v, _, off) in [c.pos, c.nrm, c.uv, c.idx] {
            if v == view {
                return off;
            }
        }
    }
    0
}

/// Write `<path>.gltf` plus a sibling `<stem>.bin` (path with the extension replaced by `bin`).
pub fn export_gltf(model: &Model, path: &Path) -> Result<(), String> {
    let (_, bin) = layout(model)?;
    let stem = path.with_extension("");
    let stem_name = stem
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("bad export path {}", path.display()))?
        .to_owned();
    let json = build_json(model, bin.len(), Some(format!("{stem_name}.bin")))?;
    let json_text = serde_json::to_string(&json).map_err(|e| e.to_string())?;
    let bin_path = path.with_extension("bin");
    fs::write(&bin_path, &bin).map_err(|e| format!("writing {}: {e}", bin_path.display()))?;
    fs::write(path, json_text).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(())
}

/// Write a single binary `.glb`: 12-byte header, JSON chunk (space padded), BIN chunk (zero padded).
pub fn export_glb(model: &Model, path: &Path) -> Result<(), String> {
    let (_, bin) = layout(model)?;
    let json = build_json(model, bin.len(), None)?;
    let mut json_chunk = serde_json::to_vec(&json).map_err(|e| e.to_string())?;
    json_chunk.resize(align4(json_chunk.len()), b' ');
    let mut bin_chunk = bin;
    bin_chunk.resize(align4(bin_chunk.len()), 0);

    let total = (12 + 8 + json_chunk.len() + 8 + bin_chunk.len()) as u32;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_chunk);
    out.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin_chunk);
    fs::write(path, out).map_err(|e| format!("writing {}: {e}", path.display()))
}
