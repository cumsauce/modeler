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
    } else if g.domain == "card" || g.domain == "decal" {
        // The part's own uvs (0..1 per card) carry the image as authored.
        if part.uvs.len() != part.verts.len() {
            crate::uv::metric_box(part);
        }
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
    // Streamed into the hasher: no string per part per lookup.
    {
        let mut w = HashInto(&mut h);
        let _ = serde_json::to_writer(&mut w, &r.graph);
        let _ = serde_json::to_writer(&mut w, &r.params);
    }
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
        geometry_hash(p).hash(&mut h);
        p.scale.map(f32::to_bits).hash(&mut h);
        hash_occluders(p, &mut h);
    }
    h.finish()
}

struct HashInto<'a, H: Hasher>(&'a mut H);

impl<H: Hasher> std::io::Write for HashInto<'_, H> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type GeomKey = (usize, usize, usize, usize, usize, usize, u64);
static GEOMETRY: Mutex<Option<HashMap<GeomKey, u64>>> = Mutex::new(None);

/// A hash of a part's vertices, normals, uvs and indices, memoised by the
/// buffers' identity and a sample of their content (parts are shared
/// `Arc`s, so an unchanged part isn't rehashed on every bake lookup).
fn geometry_hash(p: &Part) -> u64 {
    let (nv, ni) = (p.verts.len(), p.indices.len());
    let sample = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if nv > 0 && ni > 0 {
            for k in 0..32 {
                p.verts[k * nv / 32].map(f32::to_bits).hash(&mut h);
                p.indices[k * ni / 32].hash(&mut h);
            }
        }
        h.finish()
    };
    let key = (
        p.verts.as_ptr() as usize,
        p.normals.as_ptr() as usize,
        p.uvs.as_ptr() as usize,
        nv,
        ni,
        p.uvs.len(),
        sample,
    );
    if let Ok(g) = GEOMETRY.lock()
        && let Some(v) = g.as_ref().and_then(|m| m.get(&key))
    {
        return *v;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in p.verts.iter().chain(&p.normals) {
        v.map(f32::to_bits).hash(&mut h);
    }
    for v in &p.uvs {
        v.map(f32::to_bits).hash(&mut h);
    }
    p.indices.hash(&mut h);
    let v = h.finish();
    if let Ok(mut g) = GEOMETRY.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() > 8192 {
            m.clear();
        }
        m.insert(key, v);
    }
    v
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

/// Bakes a material on its own (a decal's image: one `tile`-meter square,
/// independent of any mesh), cached like a tile bake.
pub fn bake_ref(r: &PbrRef) -> Result<Arc<Channels>, String> {
    let key = bake_key(r, None);
    if let Some(hit) = cache_get(key) {
        return Ok(hit);
    }
    let g = r.graph()?;
    let s = Samples::tile((g.resolution as usize).clamp(16, 2048), g.tile);
    let ch = Arc::new(
        bake::bake(&g.nodes, &r.params(g), &s, true)
            .map_err(|e| format!("material `{}`: {e}", r.material))?,
    );
    cache_put(key, ch.clone());
    Ok(ch)
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
    /// How opacity renders.
    pub alpha: Alpha,
    /// Both faces drawn (cutout cards).
    pub double_sided: bool,
}

/// A bake's alpha mode (glTF `alphaMode`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alpha {
    Opaque,
    /// A cutout at this threshold.
    Mask(f32),
    Blend,
}

/// The alpha mode of a bake: the output's `alpha` when given, else mask
/// when the opacity is nearly all 0 or 1 (a cutout), blend when it has
/// real in-between values, opaque when it is all 1.
pub fn alpha_of(ch: &Channels) -> Alpha {
    let cut = ch.extras.alpha_cutoff.unwrap_or(0.5).clamp(0.0, 1.0);
    match ch.extras.alpha.as_deref() {
        Some("opaque") => return Alpha::Opaque,
        Some("mask") => return Alpha::Mask(cut),
        Some("blend") => return Alpha::Blend,
        _ => {}
    }
    let n = ch.opacity.len().max(1);
    if ch.opacity.iter().all(|o| *o >= 0.999) {
        return Alpha::Opaque;
    }
    let mid = ch
        .opacity
        .iter()
        .filter(|o| **o > 0.05 && **o < 0.95)
        .count();
    if (mid as f32) < 0.1 * n as f32 {
        Alpha::Mask(cut)
    } else {
        Alpha::Blend
    }
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
        alpha: alpha_of(ch),
        double_sided: ch
            .extras
            .double_sided
            .unwrap_or(matches!(alpha_of(ch), Alpha::Mask(_))),
    }
}

/// Box-filtered mip chain of an RGBA8 image (level 0 first, concatenated),
/// with the level count. `srgb` averages in linear light.
pub fn mips(w: usize, h: usize, px: &[u8], srgb: bool) -> (Vec<u8>, u32) {
    mips_with(w, h, px, srgb, None)
}

/// Share of texels whose alpha passes `cutoff`.
fn coverage(px: &[u8], cutoff: f32) -> f32 {
    let n = (px.len() / 4).max(1);
    let c = (cutoff * 255.0) as u8;
    px.as_chunks::<4>().0.iter().filter(|p| p[3] > c).count() as f32 / n as f32
}

/// Scales a mip level's alpha so as many texels pass the `cutoff` as in
/// the full-size image (`target`): box-filtered alpha otherwise thins a
/// cutout (leaves, grass) until it vanishes at a distance.
pub fn keep_coverage(px: &mut [u8], cutoff: f32, target: f32) {
    let alpha: Vec<u8> = px.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
    let scaled = |k: f32| -> Vec<u8> {
        alpha
            .iter()
            .map(|a| (*a as f32 * k).min(255.0) as u8)
            .collect()
    };
    let cov = |k: f32| {
        let c = (cutoff * 255.0) as u8;
        scaled(k).iter().filter(|a| **a > c).count() as f32 / alpha.len().max(1) as f32
    };
    let (mut lo, mut hi) = (0.25f32, 8.0f32);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if cov(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // Coverage moves in steps on a small level: take the closer side.
    let k = if (cov(lo) - target).abs() < (cov(hi) - target).abs() {
        lo
    } else {
        hi
    };
    for (p, a) in px.as_chunks_mut::<4>().0.iter_mut().zip(scaled(k)) {
        p[3] = a;
    }
}

/// [`mips`], keeping a cutout's coverage at every level when `cutoff` is
/// given (see [`keep_coverage`]).
pub fn mips_with(w: usize, h: usize, px: &[u8], srgb: bool, cutoff: Option<f32>) -> (Vec<u8>, u32) {
    let target = cutoff.map(|c| coverage(px, c));
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
        if let (Some(c), Some(t)) = (cutoff, target) {
            keep_coverage(&mut next, c, t);
        }
        out.extend_from_slice(&next);
        cur = next;
        cw = nw;
        chh = nh;
        levels += 1;
    }
    (out, levels)
}

#[cfg(test)]
mod alpha_tests {
    use super::*;

    fn ch(opacity: Vec<f32>) -> Channels {
        let n = opacity.len();
        Channels {
            w: n,
            h: 1,
            base_color: vec![[1.0; 3]; n],
            roughness: vec![1.0; n],
            metallic: vec![0.0; n],
            normal: vec![[0.0, 0.0, 1.0]; n],
            height: vec![0.0; n],
            ao: vec![1.0; n],
            emissive: vec![[0.0; 3]; n],
            opacity,
            driven: vec![],
            extras: Default::default(),
            runs: 0,
        }
    }

    #[test]
    fn opacity_picks_mask_blend_or_opaque() {
        assert_eq!(alpha_of(&ch(vec![1.0; 8])), Alpha::Opaque);
        let cut = ch(vec![0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        assert_eq!(alpha_of(&cut), Alpha::Mask(0.5));
        assert!(textures(&cut).double_sided);
        assert_eq!(alpha_of(&ch(vec![0.4; 8])), Alpha::Blend);
        let mut forced = ch(vec![0.4; 8]);
        forced.extras.alpha = Some("mask".into());
        forced.extras.alpha_cutoff = Some(0.3);
        assert_eq!(alpha_of(&forced), Alpha::Mask(0.3));
    }

    #[test]
    fn cutout_mips_keep_their_coverage() {
        // Fine scattered speckle covering about a third of the texels (thin
        // foliage): a plain box filter drops most of it under a 0.5 cutoff.
        let (w, h) = (64, 64);
        let mut px = vec![0u8; w * h * 4];
        for i in 0..w * h {
            let r = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40;
            if r % 100 < 35 {
                px[i * 4 + 3] = 255;
            }
        }
        let cov = |lv: &[u8]| {
            lv.as_chunks::<4>().0.iter().filter(|p| p[3] > 127).count() as f32
                / (lv.len() / 4) as f32
        };
        let level = |data: &[u8], k: usize| {
            let mut off = 0;
            let (mut lw, mut lh) = (w, h);
            for _ in 0..k {
                off += lw * lh * 4;
                lw = (lw / 2).max(1);
                lh = (lh / 2).max(1);
            }
            data[off..off + lw * lh * 4].to_vec()
        };
        let (plain, _) = mips(w, h, &px, false);
        let (kept, _) = mips_with(w, h, &px, false, Some(0.5));
        let base = cov(&px);
        // Plain filtering loses the speckle once a texel averages 16.
        assert!(
            cov(&level(&plain, 2)) < base * 0.5,
            "{}",
            cov(&level(&plain, 2))
        );
        // Stripes blur to one grey at 8x8, so coverage can't match exactly,
        // but it stays far closer than plain filtering's.
        let (p3, k3) = (cov(&level(&plain, 3)), cov(&level(&kept, 3)));
        assert!(
            (k3 - base).abs() <= (p3 - base).abs(),
            "plain {p3} kept {k3} base {base}"
        );
        // Level 1 averages 4 texels, so its coverage moves in steps.
        for (lv, tol) in [(1, 0.15), (2, 0.1), (3, 0.1)] {
            let (p, k) = (cov(&level(&plain, lv)), cov(&level(&kept, lv)));
            assert!((k - base).abs() < tol, "level {lv}: kept {k} vs {base}");
            assert!(
                (p - base).abs() >= (k - base).abs(),
                "level {lv}: plain {p}"
            );
        }
    }
}
