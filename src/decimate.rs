//! `decimate`: a lighter copy of a dense mesh (quadric edge collapse, the
//! input left untouched in the graph), optionally carrying the dense
//! surface along as a [`Detail`] so the material bake writes its normals
//! and height onto the low mesh: a 400k-triangle sculpt rendered from a few
//! thousand triangles and a normal map.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use bevy::math::Vec3;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::Part;
use crate::material::bake::Channels;
use crate::organic::{TriGrid, World};

/// The `decimate` op's settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Decimate {
    /// Share of the input's triangles to keep, 0.001..1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    /// Target triangle count instead of a ratio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tris: Option<u32>,
    /// Bake the input's surface onto the result (normal + height maps).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bake: bool,
    /// Atlas size of the bake in texels (default 1024).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
    /// How far (meters) from the low surface the bake looks for the high
    /// one (default 3% of the part's size, at least the largest deviation
    /// sampled at the low mesh's vertices x 1.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cage: Option<f32>,
    /// Normals crease where faces meet at more than this (default 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crease: Option<f32>,
    /// "x", "y" or "z": decimate the + half and mirror it across that zero
    /// plane, so the result is exactly symmetric.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symmetric: Option<String>,
}

impl Decimate {
    pub fn validate(&self) -> Result<(), String> {
        match (self.ratio, self.tris) {
            (Some(_), Some(_)) => return Err("give ratio or tris, not both".into()),
            (None, None) => return Err("give ratio (0..1) or tris (a triangle count)".into()),
            (Some(r), None) if !(r > 0.0 && r <= 1.0) => {
                return Err(format!("ratio: {r} is not in (0, 1]"));
            }
            (None, Some(0)) => return Err("tris: must be at least 4".into()),
            _ => {}
        }
        if let Some(r) = self.resolution
            && !(64..=4096).contains(&r)
        {
            return Err(format!("resolution: {r} is not in 64..4096"));
        }
        if let Some(c) = self.cage
            && (c.is_nan() || c <= 0.0)
        {
            return Err("cage: must be > 0 meters".into());
        }
        if (self.resolution.is_some() || self.cage.is_some()) && !self.bake {
            return Err("resolution and cage only apply with bake: true".into());
        }
        if let Some(c) = self.crease
            && !(0.0..=180.0).contains(&c)
        {
            return Err(format!("crease: {c} is not in 0..180 degrees"));
        }
        if let Some(s) = &self.symmetric
            && axis_of(s).is_none()
        {
            return Err(format!("symmetric: x, y or z, got `{s}`"));
        }
        Ok(())
    }

    pub fn summary(&self) -> String {
        let mut s = match (self.ratio, self.tris) {
            (_, Some(t)) => format!("tris={t}"),
            (Some(r), _) => format!("ratio={r}"),
            _ => String::new(),
        };
        if let Some(a) = &self.symmetric {
            s.push_str(&format!(" symmetric={a}"));
        }
        if self.bake {
            s.push_str(&format!(" bake res={}", self.resolution.unwrap_or(1024)));
            if let Some(c) = self.cage {
                s.push_str(&format!(" cage={c}"));
            }
        }
        s
    }
}

/// The dense surface a decimated part was made from, in the part's own
/// (unscaled) vertex space, for the material bake.
#[derive(Debug)]
pub struct Detail {
    pub verts: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub resolution: u32,
    /// Search distance in meters (scaled space).
    pub cage: f32,
    /// Content hash, part of the bake key.
    pub key: u64,
}

impl PartialEq for Detail {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key
    }
}

impl Detail {
    fn of(src: &Part, resolution: u32, cage: f32) -> Detail {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for v in src.verts.iter().chain(&src.normals) {
            v.map(f32::to_bits).hash(&mut h);
        }
        src.indices.hash(&mut h);
        resolution.hash(&mut h);
        cage.to_bits().hash(&mut h);
        Detail {
            verts: src.verts.clone(),
            normals: src.normals.clone(),
            indices: src.indices.clone(),
            resolution,
            cage,
            key: h.finish(),
        }
    }

    /// The same surface with every point and normal mapped (a mirror bakes
    /// the part into new coordinates); `flip` reverses the winding.
    pub fn mapped(
        &self,
        pos: impl Fn(Vec3) -> Vec3,
        nrm: impl Fn(Vec3) -> Vec3,
        flip: bool,
    ) -> Detail {
        let verts: Vec<[f32; 3]> = self
            .verts
            .iter()
            .map(|v| pos(Vec3::from(*v)).to_array())
            .collect();
        let normals: Vec<[f32; 3]> = self
            .normals
            .iter()
            .map(|n| nrm(Vec3::from(*n)).normalize_or_zero().to_array())
            .collect();
        let mut indices = self.indices.clone();
        if flip {
            for t in indices.as_chunks_mut::<3>().0 {
                t.swap(1, 2);
            }
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.key.hash(&mut h);
        for v in verts.iter().take(64) {
            v.map(f32::to_bits).hash(&mut h);
        }
        flip.hash(&mut h);
        Detail {
            verts,
            normals,
            indices,
            resolution: self.resolution,
            cage: self.cage,
            key: h.finish(),
        }
    }
}

/// A decimated copy of `src` named `name`. Attributes are dropped first so
/// uv seams and split normals don't lock vertices (the material pass maps
/// the result again), then normals are re-creased.
pub fn decimate(name: &str, src: &Part, def: &Decimate) -> Result<Part, String> {
    let n = src.tri_count() as usize;
    if n == 0 {
        return Err(format!("`{}` has no triangles", src.name));
    }
    let ratio = match (def.ratio, def.tris) {
        (_, Some(t)) => (t as f32 / n as f32).min(1.0),
        (Some(r), _) => r,
        _ => 1.0,
    };
    let mut bare = src.clone();
    bare.normals.clear();
    bare.uvs.clear();
    bare.detail = None;
    if bare.indices.is_empty() {
        bare.indices = (0..bare.verts.len() as u32).collect();
    }
    // simplify welds by exact bits: snap near-coincident corners together.
    weld(&mut bare);
    // The same crease angle guards the collapses (feature-edge quadrics)
    // and splits the normals afterwards.
    let crease = def.crease.unwrap_or(60.0);
    let mut out = match def.symmetric.as_deref().and_then(axis_of) {
        Some(ax) => symmetric(&bare, ax, ratio, crease)?,
        None => crate::simplify::simplify_creased(&bare, ratio.max(1e-5), Some(crease)),
    };
    out.name = name.to_string();
    out.visible = true;
    crate::uv::crease_normals(&mut out, crease);
    crate::uv::ensure_uvs(&mut out, Some("box")).map_err(|e| e.to_string())?;
    if def.bake {
        let mut hi = src.clone();
        if hi.normals.len() != hi.verts.len() {
            crate::uv::crease_normals(&mut hi, 60.0);
        }
        if hi.indices.is_empty() {
            hi.indices = (0..hi.verts.len() as u32).collect();
        }
        let cage = def.cage.unwrap_or_else(|| auto_cage(&out, &hi));
        out.detail = Some(Arc::new(Detail::of(
            &hi,
            def.resolution.unwrap_or(1024),
            cage,
        )));
    }
    Ok(out)
}

fn axis_of(s: &str) -> Option<usize> {
    ["x", "y", "z"].iter().position(|a| *a == s)
}

/// Decimates the + half of `p` (welded, no attributes) across its own axis
/// `ax` (the part's vertex space: model space for an op's output) and
/// mirrors it: the cut along the plane is a border, which simplification
/// keeps exactly, so the halves join again vertex for vertex.
fn symmetric(p: &Part, ax: usize, ratio: f32, crease: f32) -> Result<Part, String> {
    let name = ["x", "y", "z"][ax];
    let v: Vec<Vec3> = p.verts.iter().map(|q| Vec3::from(*q)).collect();
    let (lo, hi) = v.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), q| (a.min(*q), b.max(*q)),
    );
    let size = (hi - lo).max_element().max(1e-9);
    if (lo[ax] + hi[ax]).abs() > 1e-3 * size {
        return Err(format!(
            "symmetric {name}: `{}` spans {name} = {:.4}..{:.4}, not centred on {name} = 0 (its + half is kept and mirrored)",
            p.name, lo[ax], hi[ax]
        ));
    }
    let eps = size * 1e-6;
    let (hv, ht) = clip_half(&v, &p.indices, ax, eps);
    if ht.is_empty() {
        return Err(format!("symmetric {name}: nothing on the + side"));
    }
    let mut half = p.clone();
    half.verts = hv.iter().map(|q| q.to_array()).collect();
    half.indices = ht;
    let mut out = crate::simplify::simplify_creased(&half, ratio.max(1e-5), Some(crease));
    // Mirror: vertices on the plane are shared, the rest get a twin.
    let n = out.verts.len();
    let mut twin = vec![0u32; n];
    for i in 0..n {
        if out.verts[i][ax] == 0.0 {
            twin[i] = i as u32;
        } else {
            let mut m = out.verts[i];
            m[ax] = -m[ax];
            twin[i] = out.verts.len() as u32;
            out.verts.push(m);
        }
    }
    let tris: Vec<[u32; 3]> = out.indices.as_chunks::<3>().0.to_vec();
    for t in tris {
        out.indices.extend([
            twin[t[0] as usize],
            twin[t[2] as usize],
            twin[t[1] as usize],
        ]);
    }
    out.normals.clear();
    out.uvs.clear();
    Ok(out)
}

/// The triangles of a mesh on the + side of axis `ax`, cut exactly along
/// the zero plane (new vertices on it have that coordinate exactly 0).
fn clip_half(v: &[Vec3], idx: &[u32], ax: usize, eps: f32) -> (Vec<Vec3>, Vec<u32>) {
    let side = |p: Vec3| {
        if p[ax].abs() <= eps { 0.0 } else { p[ax] }
    };
    let mut out: Vec<Vec3> = Vec::new();
    let mut kept: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut cut: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    let mut tris = Vec::new();
    let mut keep = |out: &mut Vec<Vec3>, i: u32| -> u32 {
        *kept.entry(i).or_insert_with(|| {
            let mut p = v[i as usize];
            if side(p) == 0.0 {
                p[ax] = 0.0;
            }
            out.push(p);
            (out.len() - 1) as u32
        })
    };
    for t in idx.as_chunks::<3>().0 {
        if t.iter().any(|&i| i as usize >= v.len()) {
            continue;
        }
        let s = t.map(|i| side(v[i as usize]));
        if s.iter().all(|x| *x >= 0.0) {
            if s.iter().all(|x| *x == 0.0) {
                continue;
            }
            let k = t.map(|i| keep(&mut out, i));
            tris.extend(k);
            continue;
        }
        if s.iter().all(|x| *x <= 0.0) {
            continue;
        }
        let mut poly: Vec<u32> = Vec::with_capacity(4);
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let (sa, sb) = (s[k], s[(k + 1) % 3]);
            if sa >= 0.0 {
                poly.push(keep(&mut out, a));
            }
            if (sa > 0.0 && sb < 0.0) || (sa < 0.0 && sb > 0.0) {
                let key = (a.min(b), a.max(b));
                let id = *cut.entry(key).or_insert_with(|| {
                    let (pa, pb) = (v[a as usize], v[b as usize]);
                    let f = pa[ax] / (pa[ax] - pb[ax]);
                    let mut p = pa + (pb - pa) * f;
                    p[ax] = 0.0;
                    out.push(p);
                    (out.len() - 1) as u32
                });
                poly.push(id);
            }
        }
        for k in 1..poly.len().saturating_sub(1) {
            tris.extend([poly[0], poly[k], poly[k + 1]]);
        }
    }
    (out, tris)
}

/// Welds positions closer than a millionth of the part's size.
fn weld(p: &mut Part) {
    let (lo, hi) = p.verts.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), v| (a.min(Vec3::from(*v)), b.max(Vec3::from(*v))),
    );
    let inv = 1.0 / ((hi - lo).max_element() * 1e-6).max(1e-9);
    let mut map: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
    let mut verts = Vec::new();
    let of: Vec<u32> = p
        .verts
        .iter()
        .map(|v| {
            let k = v.map(|c| (c * inv).round() as i64);
            *map.entry(k).or_insert_with(|| {
                verts.push(*v);
                (verts.len() - 1) as u32
            })
        })
        .collect();
    for i in &mut p.indices {
        if let Some(&w) = of.get(*i as usize) {
            *i = w;
        }
    }
    p.verts = verts;
}

fn scaled(p: &Part) -> Vec<Vec3> {
    let s = Vec3::from(p.scale);
    p.verts.iter().map(|v| Vec3::from(*v) * s).collect()
}

fn tris_of(idx: &[u32], n: usize) -> Vec<[u32; 3]> {
    idx.as_chunks::<3>()
        .0
        .iter()
        .filter(|t| t.iter().all(|&i| (i as usize) < n))
        .copied()
        .collect()
}

/// The high surface as a ray-cast target, its box padded by `pad`.
fn target(hi_v: Vec<Vec3>, idx: &[u32], pad: f32) -> (World, TriGrid) {
    let n = hi_v.len();
    let t = tris_of(idx, n);
    let (lo, hi) = hi_v.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), v| (a.min(*v), b.max(*v)),
    );
    let mut v = hi_v;
    // Unreferenced corners pad the grid, so rays starting outside the
    // dense mesh still walk its cells.
    v.push(lo - Vec3::splat(pad));
    v.push(hi + Vec3::splat(pad));
    let w = World::raw(v, t);
    let g = TriGrid::new(&w);
    (w, g)
}

/// A cage that reaches the high surface from every low vertex: 1.5 x the
/// largest gap found along the low normals, at least 1% of the size.
fn auto_cage(low: &Part, hi: &Part) -> f32 {
    let lv = scaled(low);
    let (lo, hb) = lv.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), v| (a.min(*v), b.max(*v)),
    );
    let size = (hb - lo).max_element().max(1e-6);
    let reach = size * 0.1;
    let (w, g) = target(scaled(hi), &hi.indices, reach);
    let s = Vec3::from(low.scale);
    let worst = lv
        .par_iter()
        .zip(&low.normals)
        .map(|(p, n)| {
            let n = (Vec3::from(*n) / s).normalize_or_zero();
            let a = g.ray_hit(&w, *p, n, 0.0, reach, u32::MAX).map(|h| h.0);
            let b = g.ray_hit(&w, *p, -n, 0.0, reach, u32::MAX).map(|h| h.0);
            match (a, b) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => 0.0,
            }
        })
        .reduce(|| 0.0, f32::max);
    (worst * 1.5).max(size * 0.01)
}

/// Where the material underneath comes from.
pub enum Base<'a> {
    /// Already baked on this part's atlas (object materials).
    Atlas(&'a Channels),
    /// A tile bake repeating every `period` meters, box-mapped per face
    /// the same way tile materials are mapped on parts.
    Tile(&'a Channels, f32),
}

/// Bakes `d` (the dense surface) onto `part`'s atlas over the material in
/// `base`: per texel, a ray from the cage down the low normal finds the
/// high surface; its normal goes to tangent space (the same tangents the
/// export writes) and is blended with the material's own normal, the gap
/// is added to height.
/// `d` is optional (a finish-only bake samples the material onto the
/// atlas); `finish` adds bevelled-edge normals and macro variation.
pub fn bake(
    part: &Part,
    d: Option<&Detail>,
    n: usize,
    base: Base,
    finish: Option<&crate::material::finish::Finish>,
) -> Channels {
    let nf = n as f32;
    let len = n * n;
    let lv = scaled(part);
    let s = Vec3::from(part.scale);
    let tris = tris_of(&part.indices, part.verts.len());
    let tang = crate::gltf::tangents(part);
    let has = |i: usize| i < part.uvs.len() && i < part.normals.len() && i < tang.len();
    // Rasterize: triangle and barycentrics per texel.
    let mut cov: Vec<Option<(u32, [f32; 3])>> = vec![None; len];
    for (ti, t) in tris.iter().enumerate() {
        let c = t.map(|i| i as usize);
        if !c.iter().all(|&i| has(i)) {
            continue;
        }
        let q = c.map(|k| [part.uvs[k][0] * nf, part.uvs[k][1] * nf]);
        let den =
            (q[1][1] - q[2][1]) * (q[0][0] - q[2][0]) + (q[2][0] - q[1][0]) * (q[0][1] - q[2][1]);
        if den.abs() < 1e-12 {
            continue;
        }
        let mn = [0, 1].map(|k| q.iter().map(|v| v[k]).fold(f32::MAX, f32::min));
        let mx = [0, 1].map(|k| q.iter().map(|v| v[k]).fold(f32::MIN, f32::max));
        let x0 = (mn[0] - 0.5).floor().max(0.0) as usize;
        let x1 = (mx[0] + 0.5).ceil().min(nf - 1.0).max(0.0) as usize;
        let y0 = (mn[1] - 0.5).floor().max(0.0) as usize;
        let y1 = (mx[1] + 0.5).ceil().min(nf - 1.0).max(0.0) as usize;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let a = ((q[1][1] - q[2][1]) * (px - q[2][0])
                    + (q[2][0] - q[1][0]) * (py - q[2][1]))
                    / den;
                let b = ((q[2][1] - q[0][1]) * (px - q[2][0])
                    + (q[0][0] - q[2][0]) * (py - q[2][1]))
                    / den;
                let g = 1.0 - a - b;
                if a < -1e-4 || b < -1e-4 || g < -1e-4 {
                    continue;
                }
                cov[y * n + x] = Some((ti as u32, [a, b, g]));
            }
        }
    }
    let high = d.map(|d| {
        let (hw, hg) = target(
            d.verts.iter().map(|v| Vec3::from(*v) * s).collect(),
            &d.indices,
            d.cage * 1.5,
        );
        let hn: Vec<Vec3> = d.normals.iter().map(|n| Vec3::from(*n) / s).collect();
        (hw, hg, hn, d.cage)
    });
    let edges = finish
        .and_then(|f| f.bevel)
        .map(|r| crate::material::finish::Edges::new(&lv, &tris, r))
        .filter(|e| !e.is_empty());
    let macro_ = finish.and_then(|f| f.macro_.as_ref());

    struct Texel {
        base: [f32; 3],
        rough: f32,
        metal: f32,
        normal: [f32; 3],
        height: f32,
        ao: f32,
        emissive: [f32; 3],
        opacity: f32,
    }
    let sample_tile = |ch: &Channels, period: f32, uv: [f32; 2]| -> Texel {
        let (w, h) = (ch.w, ch.h);
        let x = uv[0] / period * w as f32 - 0.5;
        let y = uv[1] / period * h as f32 - 0.5;
        let (fx0, fy0) = (x.floor(), y.floor());
        let (fx, fy) = (x - fx0, y - fy0);
        let at = |i: f32, j: f32| {
            let i = (i as i64).rem_euclid(w as i64) as usize;
            let j = (j as i64).rem_euclid(h as i64) as usize;
            j * w + i
        };
        let k = [
            (at(fx0, fy0), (1.0 - fx) * (1.0 - fy)),
            (at(fx0 + 1.0, fy0), fx * (1.0 - fy)),
            (at(fx0, fy0 + 1.0), (1.0 - fx) * fy),
            (at(fx0 + 1.0, fy0 + 1.0), fx * fy),
        ];
        let f1 = |c: &[f32]| k.iter().map(|(i, w)| c[*i] * w).sum::<f32>();
        let f3 = |c: &[[f32; 3]]| {
            let mut o = [0.0; 3];
            for (i, w) in k {
                for (j, oj) in o.iter_mut().enumerate() {
                    *oj += c[i][j] * w;
                }
            }
            o
        };
        Texel {
            base: f3(&ch.base_color),
            rough: f1(&ch.roughness),
            metal: f1(&ch.metallic),
            normal: Vec3::from(f3(&ch.normal)).normalize_or(Vec3::Z).to_array(),
            height: f1(&ch.height),
            ao: f1(&ch.ao),
            emissive: f3(&ch.emissive),
            opacity: f1(&ch.opacity),
        }
    };
    let texels: Vec<Option<Texel>> = cov
        .par_iter()
        .enumerate()
        .map(|(i, c)| {
            let (ti, [a, b, g]) = (*c)?;
            let t = tris[ti as usize].map(|k| k as usize);
            let p = lv[t[0]] * a + lv[t[1]] * b + lv[t[2]] * g;
            let nrm = |k: usize| (Vec3::from(part.normals[k]) / s).normalize_or_zero();
            let nn = (nrm(t[0]) * a + nrm(t[1]) * b + nrm(t[2]) * g).normalize_or(Vec3::Y);
            let tv = |k: usize| Vec3::new(tang[k][0], tang[k][1], tang[k][2]) * s;
            let tt = tv(t[0]) * a + tv(t[1]) * b + tv(t[2]) * g;
            let tt = (tt - nn * tt.dot(nn)).normalize_or(nn.any_orthonormal_vector());
            let bb = nn.cross(tt) * tang[t[0]][3].signum();
            let mut tx = match &base {
                Base::Atlas(ch) => Texel {
                    base: ch.base_color[i],
                    rough: ch.roughness[i],
                    metal: ch.metallic[i],
                    normal: ch.normal[i],
                    height: ch.height[i],
                    ao: ch.ao[i],
                    emissive: ch.emissive[i],
                    opacity: ch.opacity[i],
                },
                Base::Tile(ch, period) => {
                    let f = (lv[t[1]] - lv[t[0]]).cross(lv[t[2]] - lv[t[0]]);
                    let fa = f.abs();
                    let (axis, sign) = if fa.x >= fa.y && fa.x >= fa.z {
                        (0u8, f.x.signum())
                    } else if fa.y >= fa.z {
                        (1, f.y.signum())
                    } else {
                        (2, f.z.signum())
                    };
                    sample_tile(ch, *period, crate::uv::metric_uv(axis, sign, p))
                }
            };
            let to_tangent =
                |v: Vec3| Vec3::new(v.dot(tt), v.dot(bb), v.dot(nn).max(0.05)).normalize();
            let whiteout = |nd: Vec3, m: [f32; 3]| {
                let m = Vec3::from(m);
                Vec3::new(nd.x + m.x, nd.y + m.y, nd.z * m.z)
                    .normalize_or(Vec3::Z)
                    .to_array()
            };
            if let Some((hw, hg, hn, cage)) = &high {
                let cage = *cage;
                // The outermost high surface within the cage, along the normal.
                let o = p + nn * cage;
                let (nd, gap) = match hg.ray_hit(hw, o, -nn, 0.0, 2.0 * cage, u32::MAX) {
                    Some((th, hti)) => {
                        let [ha, hb, hc] = hw.t[hti as usize].map(|k| k as usize);
                        let hit = o - nn * th;
                        let bc = bary(hit, hw.v[ha], hw.v[hb], hw.v[hc]);
                        let hnorm = if hn.len() > ha.max(hb).max(hc) {
                            (hn[ha] * bc[0] + hn[hb] * bc[1] + hn[hc] * bc[2]).normalize_or(nn)
                        } else {
                            (hw.v[hb] - hw.v[ha])
                                .cross(hw.v[hc] - hw.v[ha])
                                .normalize_or(nn)
                        };
                        (to_tangent(hnorm), cage - th)
                    }
                    None => (Vec3::Z, 0.0),
                };
                // Whiteout blend of the detail and the material's own normal.
                tx.normal = whiteout(nd, tx.normal);
                tx.height += gap;
            }
            if let Some(e) = &edges {
                let nf = (lv[t[1]] - lv[t[0]])
                    .cross(lv[t[2]] - lv[t[0]])
                    .normalize_or(nn);
                if let Some(bent) = e.bend(p, nf, nn) {
                    tx.normal = whiteout(to_tangent(bent), tx.normal);
                }
            }
            if let Some(m) = macro_ {
                let w = crate::material::finish::world(part, p);
                crate::material::finish::apply_macro(m, w, &mut tx.base, &mut tx.rough);
            }
            Some(tx)
        })
        .collect();
    // Fill the gutter from covered neighbours so filtering never reads off
    // an island.
    let mut src: Vec<Option<usize>> = (0..len).map(|i| texels[i].as_ref().map(|_| i)).collect();
    for _ in 0..4 {
        let prev = src.clone();
        for y in 0..n {
            for x in 0..n {
                let i = y * n + x;
                if prev[i].is_some() {
                    continue;
                }
                let nb = [
                    (x > 0).then(|| i - 1),
                    (x + 1 < n).then(|| i + 1),
                    (y > 0).then(|| i - n),
                    (y + 1 < n).then(|| i + n),
                ];
                src[i] = nb.into_iter().flatten().find_map(|j| prev[j]);
            }
        }
    }
    let blank = Texel {
        base: [1.0; 3],
        rough: 1.0,
        metal: 0.0,
        normal: [0.0, 0.0, 1.0],
        height: 0.0,
        ao: 1.0,
        emissive: [0.0; 3],
        opacity: 1.0,
    };
    let get = |i: usize| src[i].and_then(|j| texels[j].as_ref()).unwrap_or(&blank);
    let (extras, mut driven) = match &base {
        Base::Atlas(ch) | Base::Tile(ch, _) => (ch.extras.clone(), ch.driven.clone()),
    };
    let mut more = vec![];
    if high.is_some() {
        more.extend(["normal", "height"]);
    }
    if edges.is_some() {
        more.push("normal");
    }
    if macro_.is_some() {
        more.extend(["base_color", "roughness"]);
    }
    for c in more {
        if !driven.contains(&c) {
            driven.push(c);
        }
    }
    Channels {
        w: n,
        h: n,
        base_color: (0..len).map(|i| get(i).base).collect(),
        roughness: (0..len).map(|i| get(i).rough).collect(),
        metallic: (0..len).map(|i| get(i).metal).collect(),
        normal: (0..len).map(|i| get(i).normal).collect(),
        height: (0..len).map(|i| get(i).height).collect(),
        ao: (0..len).map(|i| get(i).ao).collect(),
        emissive: (0..len).map(|i| get(i).emissive).collect(),
        opacity: (0..len).map(|i| get(i).opacity).collect(),
        driven,
        extras,
        runs: 0,
    }
}

fn bary(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> [f32; 3] {
    let (v0, v1, v2) = (b - a, c - a, p - a);
    let (d00, d01, d11) = (v0.dot(v0), v0.dot(v1), v1.dot(v1));
    let (d20, d21) = (v2.dot(v0), v2.dot(v1));
    let den = d00 * d11 - d01 * d01;
    if den.abs() < 1e-20 {
        return [1.0, 0.0, 0.0];
    }
    let v = (d11 * d20 - d01 * d21) / den;
    let w = (d00 * d21 - d01 * d20) / den;
    [1.0 - v - w, v, w]
}

/// The material graph a baked part wears when it has no procedural
/// material of its own: white, fully rough and metallic factors (the part's
/// flat colour multiplies it), so only the baked normals show.
pub const PLAIN: &str = "_detail";

pub fn plain_graph() -> crate::material::MatGraph {
    crate::material::MatGraph {
        domain: "object".into(),
        tile: 1.0,
        resolution: 1024,
        nodes: vec![serde_json::json!({
            "type": "output", "name": "o",
            "base_color": [1.0, 1.0, 1.0], "roughness": 1.0, "metallic": 1.0
        })],
        params: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_decimate_mirrors_the_kept_half() {
        let b = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0, 0.6, 2.0], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let sm = crate::smooth::Smooth {
            mode: "subdiv".into(),
            levels: Some(4),
            ..Default::default()
        };
        let mut hi = crate::smooth::smooth("hi", &b, &sm).unwrap_or_else(|e| panic!("{e}"));
        // Nudge one -x vertex so the plain result can't be symmetric.
        if let Some(v) = hi.verts.iter_mut().find(|v| v[0] < -0.3) {
            v[1] += 0.01;
        }
        let def = Decimate {
            tris: Some(800),
            symmetric: Some("x".into()),
            ..Default::default()
        };
        let lo = decimate("lo", &hi, &def).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            crate::verify::open_edge_count(&lo),
            0,
            "joined along the plane"
        );
        let n = lo.tri_count();
        assert!((600..=1100).contains(&n), "{n} triangles");
        for v in &lo.verts {
            let m = [-v[0], v[1], v[2]];
            assert!(
                lo.verts
                    .iter()
                    .any(|w| (0..3).all(|k| (w[k] - m[k]).abs() < 1e-6)),
                "no mirror for {v:?}"
            );
        }
        // Off centre is refused.
        let mut off = hi.clone();
        for v in &mut off.verts {
            v[0] += 0.2;
        }
        let e = decimate("lo", &off, &def).err().unwrap_or_default();
        assert!(e.contains("not centred"), "{e}");
    }

    fn tri_dist(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
        // Distance from p to triangle abc (Ericson's closest point).
        let (ab, ac, ap) = (b - a, c - a, p - a);
        let (d1, d2) = (ab.dot(ap), ac.dot(ap));
        if d1 <= 0.0 && d2 <= 0.0 {
            return p.distance(a);
        }
        let bp = p - b;
        let (d3, d4) = (ab.dot(bp), ac.dot(bp));
        if d3 >= 0.0 && d4 <= d3 {
            return p.distance(b);
        }
        let vc = d1 * d4 - d3 * d2;
        if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
            return p.distance(a + ab * (d1 / (d1 - d3)));
        }
        let cp = p - c;
        let (d5, d6) = (ab.dot(cp), ac.dot(cp));
        if d6 >= 0.0 && d5 <= d6 {
            return p.distance(c);
        }
        let vb = d5 * d2 - d1 * d6;
        if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
            return p.distance(a + ac * (d2 / (d2 - d6)));
        }
        let va = d3 * d6 - d5 * d4;
        if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
            return p.distance(b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
        }
        let den = 1.0 / (va + vb + vc);
        p.distance(a + ab * (vb * den) + ac * (vc * den))
    }

    /// Max distance from points on `crease` to `lo`'s surface.
    fn crease_dev(lo: &Part, crease: &[Vec3]) -> f32 {
        crease
            .iter()
            .map(|&p| {
                lo.indices
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|t| {
                        let [a, b, c] = t.map(|i| Vec3::from(lo.verts[i as usize]));
                        tri_dist(p, a, b, c)
                    })
                    .fold(f32::MAX, f32::min)
            })
            .fold(0.0, f32::max)
    }

    fn sdf_part(shapes: serde_json::Value, res: f32) -> Part {
        let def: crate::sdf::SdfDef = serde_json::from_value(serde_json::json!({
            "shapes": shapes, "blend": 0.0, "resolution": res
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let none = |n: &str| -> Result<Arc<Part>, crate::ModelerError> {
            Err(crate::ModelerError::Invalid(format!("no part {n}")))
        };
        crate::sdf::build("s", &def, &none).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn decimate_keeps_sdf_creases() {
        let res = 0.01;
        let q = bevy::math::Quat::from_rotation_y(30f32.to_radians())
            * bevy::math::Quat::from_rotation_x(20f32.to_radians());
        // A rotated box (flat faces) and a sphere cut flat top and bottom
        // (two crease circles between a curved and a flat face).
        let boxy = sdf_part(
            serde_json::json!([{"shape": "box", "size": [0.3, 0.2, 0.25], "rotation": q.to_array()}]),
            res,
        );
        let h = Vec3::new(0.15, 0.1, 0.125);
        let mut box_edges = Vec::new();
        for axis in 0..3 {
            for sa in [-1.0f32, 1.0] {
                for sb in [-1.0f32, 1.0] {
                    for k in 0..9 {
                        let mut l = [0.0; 3];
                        l[axis] = (-0.8 + 0.2 * k as f32) * h[axis];
                        l[(axis + 1) % 3] = sa * h[(axis + 1) % 3];
                        l[(axis + 2) % 3] = sb * h[(axis + 2) % 3];
                        box_edges.push(q * Vec3::from(l));
                    }
                }
            }
        }
        let puck = sdf_part(
            serde_json::json!([
                {"shape": "sphere", "radius": [0.15]},
                {"shape": "box", "size": [0.4, 0.2, 0.4], "op": "intersect"}
            ]),
            res,
        );
        let r = (0.15f32 * 0.15 - 0.1 * 0.1).sqrt();
        let circles: Vec<Vec3> = (0..72)
            .flat_map(|k| {
                let a = (k as f32 * 5.0).to_radians();
                [-0.1, 0.1].map(|y| Vec3::new(r * a.cos(), y, r * a.sin()))
            })
            .collect();
        for (what, hi, crease, tris) in [
            ("box", &boxy, &box_edges, 400),
            ("puck", &puck, &circles, 2000),
        ] {
            let d = Decimate {
                tris: Some(tris),
                ..Default::default()
            };
            let t = std::time::Instant::now();
            let lo = decimate("lo", hi, &d).unwrap_or_else(|e| panic!("{e}"));
            let took = t.elapsed();
            let worst = crease_dev(&lo, crease);
            eprintln!(
                "decimated {what} ({} -> {} tris, {took:?}): crease off by {:.3} res",
                hi.tri_count(),
                lo.tri_count(),
                worst / res
            );
            crate::csg::part_to_manifold(&lo).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(
                (lo.tri_count() as i64 - tris as i64).abs() < tris as i64 / 10,
                "{what}: {}",
                lo.tri_count()
            );
            assert!(
                worst < 0.2 * res,
                "{what}: crease off by {} res",
                worst / res
            );
        }
    }

    fn bumpy_sphere() -> Part {
        let p = crate::primitives::primitive(crate::PrimitiveKind::Sphere, [0.5, 0.0, 0.0], "s")
            .unwrap_or_else(|e| panic!("{e}"));
        let w = crate::deform::Deform {
            mode: "displace".into(),
            amount: Some(0.03),
            scale: Some(0.12),
            seed: Some(3),
            detail: Some(0.008),
            ..Default::default()
        };
        crate::deform::deform("hi", &p, &w).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn decimate_keeps_a_closed_solid_at_the_target_count() {
        let hi = bumpy_sphere();
        let n = hi.tri_count();
        let d = Decimate {
            tris: Some(400),
            ..Default::default()
        };
        let lo = decimate("lo", &hi, &d).unwrap_or_else(|e| panic!("{e}"));
        assert!(n > 20_000, "{n}");
        assert!(
            (lo.tri_count() as i64 - 400).abs() < 40,
            "{}",
            lo.tri_count()
        );
        assert!(crate::csg::part_to_manifold(&lo).is_ok());
        let v = |p: &Part| crate::inspect::volume_centroid(p).0.abs();
        assert!(
            (v(&lo) / v(&hi) - 1.0).abs() < 0.06,
            "{} vs {}",
            v(&lo),
            v(&hi)
        );
    }

    #[test]
    fn a_baked_normal_map_carries_the_dense_bumps() {
        let hi = bumpy_sphere();
        let d = Decimate {
            ratio: Some(0.02),
            bake: true,
            resolution: Some(256),
            ..Default::default()
        };
        let mut lo = decimate("lo", &hi, &d).unwrap_or_else(|e| panic!("{e}"));
        let det = lo.detail.clone().unwrap_or_else(|| panic!("no detail"));
        assert!(det.cage > 0.005 && det.cage < 0.2, "cage {}", det.cage);
        crate::material::atlas::box_atlas(&mut lo, 256);
        let flat = Channels {
            w: 1,
            h: 1,
            base_color: vec![[1.0; 3]],
            roughness: vec![1.0],
            metallic: vec![0.0],
            normal: vec![[0.0, 0.0, 1.0]],
            height: vec![0.0],
            ao: vec![1.0],
            emissive: vec![[0.0; 3]],
            opacity: vec![1.0],
            driven: vec![],
            extras: Default::default(),
            runs: 0,
        };
        let ch = bake(
            &lo,
            Some(&det),
            det.resolution as usize,
            Base::Tile(&flat, 1.0),
            None,
        );
        // The bumps tilt the normals away from the low surface's.
        let tilted = ch.normal.iter().filter(|n| n[2] < 0.97).count();
        assert!(
            tilted > ch.normal.len() / 10,
            "{tilted} of {}",
            ch.normal.len()
        );
        assert!(ch.normal.iter().all(|n| n[2] > 0.0));
        let (lo_h, hi_h) = ch
            .height
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), h| (a.min(*h), b.max(*h)));
        assert!(hi_h - lo_h > 0.01, "height span {lo_h}..{hi_h}");
    }

    #[test]
    fn fields_are_checked() {
        let bad = |d: Decimate| d.validate().is_err();
        assert!(bad(Decimate::default()));
        assert!(bad(Decimate {
            ratio: Some(1.5),
            ..Default::default()
        }));
        assert!(bad(Decimate {
            ratio: Some(0.5),
            tris: Some(10),
            ..Default::default()
        }));
        assert!(bad(Decimate {
            ratio: Some(0.5),
            cage: Some(0.1),
            ..Default::default()
        }));
    }
}
