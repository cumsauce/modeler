//! Procedural materials on parts: the reference a part carries, the uv pass
//! that maps it (metric uvs for tiles, a packed atlas for object materials),
//! cached bakes, and the RGBA8 texture set shared by preview and export.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::atlas;
use super::bake::{self, Channels};
use super::eval::Samples;
use crate::Part;
use crate::params::Params;

/// A material's graph as embedded in a model, so models and prefabs carry
/// what they need to bake (and re-bake identically) without the library.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MatGraph {
    pub domain: String,
    pub tile: f32,
    pub resolution: u32,
    pub nodes: Vec<Value>,
    #[serde(default)]
    pub params: Params,
}

/// A procedural material assigned to a part.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PbrRef {
    /// The material's name (working set or library).
    pub material: String,
    /// Param overrides for this use (a darker brick, a finer grain).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f64>,
    /// Pattern scale: 2 = twice as big on the part (tile materials).
    #[serde(default = "one")]
    pub scale: f32,
    /// The graph, embedded when the item is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Shared: every history snapshot of the op points at one copy.
    pub graph: Option<std::sync::Arc<MatGraph>>,
}

fn one() -> f32 {
    1.0
}

impl PbrRef {
    pub fn graph(&self) -> Result<&MatGraph, String> {
        self.graph.as_deref().ok_or_else(|| {
            format!(
                "pbr `{}`: the material graph was not resolved",
                self.material
            )
        })
    }

    /// The graph's params with this use's overrides.
    pub fn merged_params(&self) -> Params {
        match &self.graph {
            Some(g) => self.params(g),
            None => Params::default(),
        }
    }

    fn params(&self, g: &MatGraph) -> Params {
        let mut p = g.params.clone();
        p.extend(self.params.clone());
        p
    }
}

/// Maps a part for its material: tile materials get metric uvs divided by
/// the tile size (× scale), so one texture repeat = one tile of real size;
/// object materials get a packed atlas.
pub fn map_part(part: &mut Part) -> Result<(), String> {
    let Some(r) = part.material.pbr.clone() else {
        return Ok(());
    };
    let g = r.graph()?;
    if part.verts.is_empty() {
        return Ok(());
    }
    if let Some(d) = &part.detail {
        // A decimate bake needs its own texels: always an atlas.
        let n = d.resolution as usize;
        atlas::box_atlas(part, n);
    } else if g.domain == "object" {
        atlas::box_atlas(part, atlas_size(g));
    } else {
        crate::uv::metric_box(part);
        let k = 1.0 / (g.tile * r.scale.max(1e-6)).max(1e-6);
        for uv in &mut part.uvs {
            *uv = [uv[0] * k, uv[1] * k];
        }
    }
    Ok(())
}

fn atlas_size(g: &MatGraph) -> usize {
    (g.resolution as usize).clamp(64, 2048)
}

/// The cache key of a bake for `part` (tile bakes are shared by every part
/// using the same material + params; object bakes are per part geometry).
pub fn bake_key(r: &PbrRef, part: Option<&Part>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&r.graph)
        .unwrap_or_default()
        .hash(&mut h);
    serde_json::to_string(&r.params)
        .unwrap_or_default()
        .hash(&mut h);
    // Image nodes: the file's mtime, so an edited PNG re-bakes.
    if let Some(g) = &r.graph {
        for n in &g.nodes {
            if let Some(path) = n.get("path").and_then(|v| v.as_str()) {
                std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
                    .hash(&mut h);
            }
        }
    }
    if let (Some(p), Some(g)) = (part, &r.graph)
        && (g.domain == "object" || p.detail.is_some())
    {
        if let Some(d) = &p.detail {
            d.key.hash(&mut h);
        }
        for v in p.verts.iter().chain(&p.normals) {
            v.map(f32::to_bits).hash(&mut h);
        }
        for v in &p.uvs {
            v.map(f32::to_bits).hash(&mut h);
        }
        p.indices.hash(&mut h);
        p.scale.map(f32::to_bits).hash(&mut h);
        hash_occluders(p, &mut h);
    }
    h.finish()
}

/// Folds a part's neighbouring solids into a hash (mesh-aware bakes depend
/// on them).
pub fn hash_occluders(p: &Part, h: &mut impl Hasher) {
    if let Some(o) = &p.occluders {
        for m in o.iter() {
            for t in m {
                for v in t {
                    v.map(f32::to_bits).hash(h);
                }
            }
        }
    }
}

/// Baked materials by bake key, least recently used evicted past
/// [`BAKE_BUDGET`] bytes. (A fixed entry count cleared wholesale used to
/// rebake every part of a scene with more parts than entries on every edit.)
struct BakeCache {
    map: HashMap<u64, (Arc<Channels>, u64)>,
    bytes: usize,
    tick: u64,
}

static BAKES: Mutex<Option<BakeCache>> = Mutex::new(None);

/// Memory the bake cache may hold (about a hundred 512² object atlases).
const BAKE_BUDGET: usize = 1 << 30;

fn bake_bytes(ch: &Channels) -> usize {
    // base 12 + rough 4 + metal 4 + normal 12 + height 4 + ao 4 + emissive 12 + opacity 4
    ch.w * ch.h * 56
}

fn cache_get(key: u64) -> Option<Arc<Channels>> {
    let mut g = BAKES.lock().ok()?;
    let c = g.as_mut()?;
    c.tick += 1;
    let t = c.tick;
    let e = c.map.get_mut(&key)?;
    e.1 = t;
    Some(e.0.clone())
}

fn cache_put(key: u64, ch: Arc<Channels>) {
    let Ok(mut g) = BAKES.lock() else {
        return;
    };
    let c = g.get_or_insert_with(|| BakeCache {
        map: HashMap::new(),
        bytes: 0,
        tick: 0,
    });
    c.tick += 1;
    let size = bake_bytes(&ch);
    if let Some((old, _)) = c.map.insert(key, (ch, c.tick)) {
        c.bytes -= bake_bytes(&old);
    }
    c.bytes += size;
    while c.bytes > BAKE_BUDGET && c.map.len() > 1 {
        let Some((&k, _)) = c
            .map
            .iter()
            .filter(|(k, _)| **k != key)
            .min_by_key(|(_, v)| v.1)
        else {
            break;
        };
        if let Some((old, _)) = c.map.remove(&k) {
            c.bytes -= bake_bytes(&old);
        }
    }
}

/// Bakes a part's material (cached).
pub fn bake_part(part: &Part) -> Result<Option<Arc<Channels>>, String> {
    let Some(r) = &part.material.pbr else {
        return Ok(None);
    };
    let key = bake_key(r, Some(part));
    if let Some(hit) = cache_get(key) {
        return Ok(Some(hit));
    }
    let g = r.graph()?;
    if let Some(d) = &part.detail {
        let ch = if g.domain == "object" {
            let s = atlas::samples(part, d.resolution as usize);
            let base = bake::bake(&g.nodes, &r.params(g), &s, true)
                .map_err(|e| format!("material `{}` on `{}`: {e}", r.material, part.name))?;
            crate::decimate::bake(part, d, crate::decimate::Base::Atlas(&base))
        } else {
            let s = Samples::tile((g.resolution as usize).clamp(16, 2048), g.tile);
            let base = bake::bake(&g.nodes, &r.params(g), &s, true)
                .map_err(|e| format!("material `{}` on `{}`: {e}", r.material, part.name))?;
            let period = (g.tile * r.scale.max(1e-6)).max(1e-6);
            crate::decimate::bake(part, d, crate::decimate::Base::Tile(&base, period))
        };
        let ch = Arc::new(ch);
        cache_put(key, ch.clone());
        return Ok(Some(ch));
    }
    let s = if g.domain == "object" {
        atlas::samples(part, atlas_size(g))
    } else {
        Samples::tile((g.resolution as usize).clamp(16, 2048), g.tile)
    };
    let ch = Arc::new(
        bake::bake(&g.nodes, &r.params(g), &s, true)
            .map_err(|e| format!("material `{}` on `{}`: {e}", r.material, part.name))?,
    );
    cache_put(key, ch.clone());
    Ok(Some(ch))
}

/// Bakes every part's material in parallel (parts at once, not just the
/// samples inside one bake), so a scene of small object atlases uses every
/// core; later [`bake_part`] calls hit the cache. Parts sharing a bake key
/// (tile materials) bake once. Errors are left for `bake_part` to report.
pub fn prebake<'a>(parts: impl IntoIterator<Item = &'a Part>) {
    use rayon::prelude::*;
    let mut seen = std::collections::HashSet::new();
    let todo: Vec<&Part> = parts
        .into_iter()
        .filter(|p| {
            p.material
                .pbr
                .as_ref()
                .is_some_and(|r| seen.insert(bake_key(r, Some(p))))
        })
        .filter(|p| {
            p.material
                .pbr
                .as_ref()
                .is_some_and(|r| cache_get(bake_key(r, Some(p))).is_none())
        })
        .collect();
    todo.par_iter().for_each(|p| {
        let _ = bake_part(p);
    });
}

/// RGBA8 images of a bake, laid out the way glTF / Bevy sample them.
#[derive(Clone, Debug)]
pub struct Textures {
    pub w: usize,
    pub h: usize,
    /// sRGB base colour, alpha = opacity.
    pub base: Vec<u8>,
    /// Linear: G = roughness, B = metallic (glTF metallicRoughness).
    pub metal_rough: Vec<u8>,
    /// Linear: (n + 1) / 2, +y up the image.
    pub normal: Vec<u8>,
    /// Linear: R = ambient occlusion (when the graph drives it).
    pub occlusion: Option<Vec<u8>>,
    /// sRGB emission (when driven).
    pub emissive: Option<Vec<u8>>,
    /// Any texel less than fully opaque.
    pub blend: bool,
}

pub fn textures(ch: &Channels) -> Textures {
    let n = ch.w * ch.h;
    let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut base = Vec::with_capacity(n * 4);
    let mut mr = Vec::with_capacity(n * 4);
    let mut nm = Vec::with_capacity(n * 4);
    for i in 0..n {
        let c = ch.base_color[i];
        base.extend_from_slice(&[
            u(bake::to_srgb(c[0])),
            u(bake::to_srgb(c[1])),
            u(bake::to_srgb(c[2])),
            u(ch.opacity[i]),
        ]);
        mr.extend_from_slice(&[255, u(ch.roughness[i]), u(ch.metallic[i]), 255]);
        let v = ch.normal[i];
        nm.extend_from_slice(&[
            u(v[0] * 0.5 + 0.5),
            u(v[1] * 0.5 + 0.5),
            u(v[2] * 0.5 + 0.5),
            255,
        ]);
    }
    let occlusion = ch.driven.contains(&"ao").then(|| {
        ch.ao
            .iter()
            .flat_map(|a| [u(*a), u(*a), u(*a), 255])
            .collect()
    });
    let emissive = ch.driven.contains(&"emissive").then(|| {
        ch.emissive
            .iter()
            .flat_map(|c| {
                [
                    u(bake::to_srgb(c[0])),
                    u(bake::to_srgb(c[1])),
                    u(bake::to_srgb(c[2])),
                    255,
                ]
            })
            .collect()
    });
    Textures {
        w: ch.w,
        h: ch.h,
        base,
        metal_rough: mr,
        normal: nm,
        occlusion,
        emissive,
        blend: ch.opacity.iter().any(|o| *o < 0.999),
    }
}

/// Box-filtered mip chain of an RGBA8 image (level 0 first, concatenated),
/// with the level count. `srgb` averages in linear light.
pub fn mips(w: usize, h: usize, px: &[u8], srgb: bool) -> (Vec<u8>, u32) {
    let lin = |c: u8| {
        let x = c as f32 / 255.0;
        if srgb {
            if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        } else {
            x
        }
    };
    let enc = |x: f32| {
        ((if srgb {
            bake::to_srgb(x)
        } else {
            x.clamp(0.0, 1.0)
        }) * 255.0
            + 0.5) as u8
    };
    let mut out = px.to_vec();
    let (mut cw, mut chh, mut cur) = (w, h, px.to_vec());
    let mut levels = 1;
    while cw > 1 || chh > 1 {
        let (nw, nh) = ((cw / 2).max(1), (chh / 2).max(1));
        let mut next = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                for k in 0..4 {
                    let mut s = 0.0;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (sx, sy) = ((x * 2 + dx).min(cw - 1), (y * 2 + dy).min(chh - 1));
                        let v = cur[(sy * cw + sx) * 4 + k];
                        s += if k < 3 { lin(v) } else { v as f32 / 255.0 };
                    }
                    let a = s / 4.0;
                    next[(y * nw + x) * 4 + k] = if k < 3 {
                        enc(a)
                    } else {
                        (a * 255.0 + 0.5) as u8
                    };
                }
            }
        }
        out.extend_from_slice(&next);
        cur = next;
        cw = nw;
        chh = nh;
        levels += 1;
    }
    (out, levels)
}
