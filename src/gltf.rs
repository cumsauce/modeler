//! Hand-rolled glTF 2.0 writer (JSON + .bin, and single-file .glb) on `serde_json`.
//! One scene, one root node per visible [`Part`], one mesh with one primitive
//! per node, PBR materials. Parts with a procedural material export its baked
//! textures (PNG in the buffer: base colour, metallic-roughness, normal, and
//! occlusion / emissive when driven) and a TANGENT attribute. No gltf crate
//! here; that dep is test-only.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::{Model, Part};

fn align4(n: usize) -> usize {
    n.div_ceil(4) * 4
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

/// The binary buffer plus the views and accessors pointing into it.
#[derive(Default)]
struct Builder {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Builder {
    /// Appends `bytes` (4-byte aligned) as a new bufferView; returns its index.
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> u32 {
        let start = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.bin.resize(align4(self.bin.len()), 0);
        let mut v = json!({"buffer": 0, "byteOffset": start, "byteLength": bytes.len()});
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.views.push(v);
        (self.views.len() - 1) as u32
    }

    fn accessor(
        &mut self,
        bytes: &[u8],
        count: usize,
        kind: &str,
        component: u32,
        index: bool,
    ) -> u32 {
        let view = self.view(bytes, Some(if index { 34963 } else { 34962 }));
        self.accessors.push(json!({
            "bufferView": view,
            "componentType": component,
            "count": count,
            "type": kind,
        }));
        (self.accessors.len() - 1) as u32
    }
}

/// Per-vertex tangents (xyz + handedness w) from positions, normals and uvs,
/// in the glTF convention: the bitangent cross(N, T) * w points toward
/// smaller v (up the image), matching normal maps with +y up.
pub fn tangents(part: &Part) -> Vec<[f32; 4]> {
    let n = part.verts.len();
    let mut t = vec![[0.0f32; 3]; n];
    let mut b = vec![[0.0f32; 3]; n];
    for tri in part.indices.as_chunks::<3>().0 {
        let [i0, i1, i2] = tri.map(|i| i as usize);
        if i0.max(i1).max(i2) >= n || part.uvs.len() < n {
            continue;
        }
        let (p0, p1, p2) = (part.verts[i0], part.verts[i1], part.verts[i2]);
        let (w0, w1, w2) = (part.uvs[i0], part.uvs[i1], part.uvs[i2]);
        let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let (du1, dv1, du2, dv2) = (w1[0] - w0[0], w1[1] - w0[1], w2[0] - w0[0], w2[1] - w0[1]);
        let det = du1 * dv2 - du2 * dv1;
        if det.abs() < 1e-20 {
            continue;
        }
        let r = 1.0 / det;
        let tu = [0, 1, 2].map(|k| (e1[k] * dv2 - e2[k] * dv1) * r);
        let tv = [0, 1, 2].map(|k| (e2[k] * du1 - e1[k] * du2) * r);
        for i in [i0, i1, i2] {
            for k in 0..3 {
                t[i][k] += tu[k];
                b[i][k] += tv[k];
            }
        }
    }
    (0..n)
        .map(|i| {
            let nn = part.normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]);
            let d = t[i][0] * nn[0] + t[i][1] * nn[1] + t[i][2] * nn[2];
            let mut tt = [
                t[i][0] - nn[0] * d,
                t[i][1] - nn[1] * d,
                t[i][2] - nn[2] * d,
            ];
            let l = (tt[0] * tt[0] + tt[1] * tt[1] + tt[2] * tt[2]).sqrt();
            if l < 1e-12 {
                // No uv gradient: any vector perpendicular to the normal.
                let a = if nn[0].abs() < 0.9 {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0]
                };
                let d = a[0] * nn[0] + a[1] * nn[1] + a[2] * nn[2];
                tt = [a[0] - nn[0] * d, a[1] - nn[1] * d, a[2] - nn[2] * d];
                let l = (tt[0] * tt[0] + tt[1] * tt[1] + tt[2] * tt[2]).sqrt();
                return [tt[0] / l, tt[1] / l, tt[2] / l, 1.0];
            }
            tt = tt.map(|c| c / l);
            // Image-up is -v: handedness from cross(N, T) against -dP/dv.
            let c = [
                nn[1] * tt[2] - nn[2] * tt[1],
                nn[2] * tt[0] - nn[0] * tt[2],
                nn[0] * tt[1] - nn[1] * tt[0],
            ];
            let w = if c[0] * -b[i][0] + c[1] * -b[i][1] + c[2] * -b[i][2] < 0.0 {
                -1.0
            } else {
                1.0
            };
            [tt[0], tt[1], tt[2], w]
        })
        .collect()
}

fn check_part(i: usize, part: &Part) -> Result<(), String> {
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
    if let Some(&ix) = part
        .indices
        .iter()
        .find(|&&ix| ix as usize >= part.verts.len())
    {
        return Err(format!(
            "part '{}' index {} out of range ({} verts)",
            part.name,
            ix,
            part.verts.len()
        ));
    }
    Ok(())
}

/// Builds the glTF JSON and its binary buffer. `buffer_uri` is `Some` for
/// .gltf + .bin, `None` for an embedded .glb buffer.
fn build(
    model: &Model,
    buffer_uri: Option<String>,
) -> Result<(Value, Vec<u8>), crate::ModelerError> {
    let exported: Vec<&Part> = model
        .parts
        .iter()
        .map(|p| &**p)
        .filter(|p| p.visible)
        .collect();
    if exported.is_empty() {
        return Err(format!("model '{}' has no visible parts to export", model.name).into());
    }
    let mut b = Builder::default();
    let (mut nodes, mut meshes, mut materials) = (Vec::new(), Vec::new(), Vec::new());
    let (mut images, mut textures) = (Vec::new(), Vec::new());
    // Texture sets by bake key: tile materials are shared across parts.
    let mut sets: HashMap<u64, Value> = HashMap::new();
    for (i, part) in exported.iter().enumerate() {
        check_part(i, part)?;
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in &part.verts {
            for c in 0..3 {
                min[c] = min[c].min(v[c]);
                max[c] = max[c].max(v[c]);
            }
        }
        let n = part.verts.len();
        let pos = b.accessor(&le_f32s(&part.verts), n, "VEC3", 5126, false);
        b.accessors[pos as usize]["min"] = json!(min);
        b.accessors[pos as usize]["max"] = json!(max);
        let nrm = b.accessor(&le_f32s(&part.normals), n, "VEC3", 5126, false);
        let uv = b.accessor(&le_f32s(&part.uvs), n, "VEC2", 5126, false);
        let idx_bytes: Vec<u8> = part
            .indices
            .iter()
            .flat_map(|ix| ix.to_le_bytes())
            .collect();
        let idx = b.accessor(&idx_bytes, part.indices.len(), "SCALAR", 5125, true);
        let mut attributes = json!({"POSITION": pos, "NORMAL": nrm, "TEXCOORD_0": uv});

        let mut pbr = json!({
            "baseColorFactor": part.material.base_color,
            "metallicFactor": part.material.metalness,
            "roughnessFactor": part.material.roughness,
        });
        let mut material = json!({"name": format!("{}-mat", part.name)});
        if let Some(r) = &part.material.pbr {
            let key = crate::material::pbr::bake_key(r, Some(part));
            if let std::collections::hash_map::Entry::Vacant(slot) = sets.entry(key) {
                let ch = crate::material::pbr::bake_part(part)?
                    .ok_or_else(|| format!("part '{}': no bake", part.name))?;
                let t = crate::material::pbr::textures(&ch);
                let mut add = |px: &[u8], what: &str| -> Result<u32, String> {
                    let png = crate::raster::encode_png(t.w as u32, t.h as u32, px)?;
                    let view = b.view(&png, None);
                    images.push(json!({
                        "name": format!("{}-{what}", r.material),
                        "bufferView": view,
                        "mimeType": "image/png",
                    }));
                    textures.push(json!({"sampler": 0, "source": images.len() - 1}));
                    Ok((textures.len() - 1) as u32)
                };
                let mut set = json!({
                    "base": add(&t.base, "base_color")?,
                    "mr": add(&t.metal_rough, "metal_rough")?,
                    "normal": add(&t.normal, "normal")?,
                    "blend": t.blend,
                });
                if let Some(o) = &t.occlusion {
                    set["ao"] = json!(add(o, "occlusion")?);
                }
                if let Some(e) = &t.emissive {
                    set["emissive"] = json!(add(e, "emissive")?);
                }
                slot.insert(set);
            }
            let set = &sets[&key];
            pbr["baseColorTexture"] = json!({"index": set["base"]});
            pbr["metallicRoughnessTexture"] = json!({"index": set["mr"]});
            material["name"] = json!(format!("{}-{}", part.name, r.material));
            material["normalTexture"] = json!({"index": set["normal"]});
            if let Some(a) = set.get("ao") {
                material["occlusionTexture"] = json!({"index": a});
            }
            if let Some(e) = set.get("emissive") {
                material["emissiveTexture"] = json!({"index": e});
                material["emissiveFactor"] = json!([1.0, 1.0, 1.0]);
            }
            if set["blend"] == json!(true) {
                material["alphaMode"] = json!("BLEND");
            }
            let tan: Vec<u8> = tangents(part)
                .iter()
                .flat_map(|t| t.iter().flat_map(|f| f.to_le_bytes()))
                .collect();
            attributes["TANGENT"] = json!(b.accessor(&tan, n, "VEC4", 5126, false));
        }
        material["pbrMetallicRoughness"] = pbr;
        materials.push(material);
        meshes.push(json!({
            "name": part.name,
            "primitives": [{"attributes": attributes, "indices": idx, "material": materials.len() - 1}],
        }));
        nodes.push(json!({
            "name": part.name,
            "translation": part.translation,
            "rotation": part.rotation,
            "scale": part.scale,
            "mesh": meshes.len() - 1,
        }));
    }
    let mut buffer = json!({ "byteLength": b.bin.len() });
    if let Some(uri) = buffer_uri {
        buffer["uri"] = json!(uri);
    }
    let mut doc = json!({
        "asset": { "version": "2.0", "generator": "modeler" },
        "scene": 0,
        "scenes": [{ "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": b.accessors,
        "bufferViews": b.views,
        "buffers": [buffer],
    });
    if !images.is_empty() {
        doc["images"] = json!(images);
        doc["textures"] = json!(textures);
        // Repeat, trilinear (viewers build the mips).
        doc["samplers"] =
            json!([{"magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497}]);
    }
    Ok((doc, b.bin))
}

/// Write `<path>.gltf` plus a sibling `<stem>.bin` (path with the extension replaced by `bin`).
pub fn export_gltf(model: &Model, path: &Path) -> Result<(), crate::ModelerError> {
    let stem = path.with_extension("");
    let stem_name = stem
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("bad export path {}", path.display()))?
        .to_owned();
    let (json, bin) = build(model, Some(format!("{stem_name}.bin")))?;
    let json_text = serde_json::to_string(&json).map_err(|e| e.to_string())?;
    let bin_path = path.with_extension("bin");
    fs::write(&bin_path, &bin).map_err(|e| format!("writing {}: {e}", bin_path.display()))?;
    fs::write(path, json_text).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(())
}

/// Write a single binary `.glb`: 12-byte header, JSON chunk (space padded), BIN chunk (zero padded).
pub fn export_glb(model: &Model, path: &Path) -> Result<(), crate::ModelerError> {
    let (json, bin) = build(model, None)?;
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
    fs::write(path, out)
        .map_err(|e| crate::ModelerError::Io(format!("writing {}: {e}", path.display())))
}
