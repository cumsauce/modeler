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
        Ok(())
    }

    pub fn summary(&self) -> String {
        let mut s = match (self.ratio, self.tris) {
            (_, Some(t)) => format!("tris={t}"),
            (Some(r), _) => format!("ratio={r}"),
            _ => String::new(),
        };
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
    let mut out = crate::simplify::simplify(&bare, ratio.max(1e-5));
    out.name = name.to_string();
    out.visible = true;
    crate::uv::crease_normals(&mut out, def.crease.unwrap_or(60.0));
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
pub fn bake(part: &Part, d: &Detail, base: Base) -> Channels {
    let n = d.resolution as usize;
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
    let (hw, hg) = target(
        d.verts.iter().map(|v| Vec3::from(*v) * s).collect(),
        &d.indices,
        d.cage * 1.5,
    );
    let hn: Vec<Vec3> = d.normals.iter().map(|n| Vec3::from(*n) / s).collect();
    let cage = d.cage;

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
            // The outermost high surface within the cage, along the normal.
            let o = p + nn * cage;
            let (nd, gap) = match hg.ray_hit(&hw, o, -nn, 0.0, 2.0 * cage, u32::MAX) {
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
                    let v = Vec3::new(hnorm.dot(tt), hnorm.dot(bb), hnorm.dot(nn).max(0.05));
                    (v.normalize(), cage - th)
                }
                None => (Vec3::Z, 0.0),
            };
            // Whiteout blend of the detail and the material's own normal.
            let m = Vec3::from(tx.normal);
            tx.normal = Vec3::new(nd.x + m.x, nd.y + m.y, nd.z * m.z)
                .normalize_or(Vec3::Z)
                .to_array();
            tx.height += gap;
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
    for c in ["normal", "height"] {
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
        let ch = bake(&lo, &det, Base::Tile(&flat, 1.0));
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
