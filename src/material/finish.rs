//! Per-part finishing passes baked into a part's own atlas on top of its
//! material: `bevel` (rounded-edge normals at sharp creases, no geometry
//! change) and `macro` (low-frequency world-space colour / roughness drift
//! and darkening toward the ground, so big tiled surfaces don't repeat).

use super::noise;
use crate::Part;
use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What a `material` item's `bevel` / `macro` ask for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Finish {
    /// Rounded-edge radius in meters baked into the normal map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bevel: Option<f32>,
    /// World-space macro variation.
    #[serde(
        default,
        rename = "macro",
        alias = "macro_",
        skip_serializing_if = "Option::is_none"
    )]
    pub macro_: Option<Macro>,
}

/// Low-frequency variation over a tile (or any) material.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Macro {
    /// Strength of the colour / roughness drift, 0..1 (0.15).
    #[serde(default = "d_amount")]
    pub amount: f32,
    /// Feature size of the drift in meters (4).
    #[serde(default = "d_scale")]
    pub scale: f32,
    /// How much the surface darkens and dirties toward y = 0, 0..1 (0).
    #[serde(default)]
    pub ground_dirt: f32,
    /// Height in meters over which the ground dirt fades out (0.5).
    #[serde(default = "d_ground_height")]
    pub ground_height: f32,
    #[serde(default)]
    pub seed: u32,
}

fn d_amount() -> f32 {
    0.15
}
fn d_scale() -> f32 {
    4.0
}
fn d_ground_height() -> f32 {
    0.5
}

/// The atlas size of a finish bake when nothing else sets it.
pub const RESOLUTION: usize = 1024;

impl Finish {
    pub fn is_empty(&self) -> bool {
        self.bevel.is_none() && self.macro_.is_none()
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(b) = self.bevel
            && !(b > 0.0 && b <= 0.5 && b.is_finite())
        {
            return Err(format!("bevel: {b}: give a radius in meters, 0 < r <= 0.5"));
        }
        if let Some(m) = &self.macro_ {
            if !(0.0..=1.0).contains(&m.amount) {
                return Err(format!("macro.amount: {}: give 0..1", m.amount));
            }
            if !(m.scale > 0.0 && m.scale.is_finite()) {
                return Err(format!("macro.scale: {}: give meters > 0", m.scale));
            }
            if !(0.0..=1.0).contains(&m.ground_dirt) {
                return Err(format!("macro.ground_dirt: {}: give 0..1", m.ground_dirt));
            }
            if !(m.ground_height > 0.0 && m.ground_height.is_finite()) {
                return Err(format!(
                    "macro.ground_height: {}: give meters > 0",
                    m.ground_height
                ));
            }
        }
        Ok(())
    }
}

/// The finish a part's bake applies: its material item's, with the bevel
/// falling back to the graph's `output` node `bevel`.
pub fn of(part: &Part) -> Option<Finish> {
    let mut f = part.material.finish.clone().unwrap_or_default();
    if f.bevel.is_none()
        && let Some(g) = part.material.pbr.as_ref().and_then(|r| r.graph.as_ref())
    {
        f.bevel = g.nodes.iter().find_map(|n| {
            (n.get("type")?.as_str()? == "output")
                .then(|| n.get("bevel")?.as_f64())
                .flatten()
                .map(|b| b as f32)
                .filter(|b| *b > 0.0)
        });
    }
    (!f.is_empty()).then_some(f)
}

/// Sharp creases of a mesh (scaled local space) for the bevel pass, bucketed
/// in a grid so a texel looks up only the edges near it.
pub struct Edges {
    /// Segment ends and the two face normals.
    edges: Vec<(Vec3, Vec3, Vec3, Vec3)>,
    grid: HashMap<[i32; 3], Vec<u32>>,
    cell: f32,
    radius: f32,
}

/// Dihedral angle (degrees) above which an edge gets bevelled.
const CREASE: f32 = 30.5;

impl Edges {
    pub fn new(lv: &[Vec3], tris: &[[u32; 3]], radius: f32) -> Edges {
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in lv {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        let diag = (hi - lo).length().max(1e-6);
        // Weld split corners (uv seams, creased normals) by position.
        let q = diag * 1e-5;
        let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
        let weld: Vec<u32> = lv
            .iter()
            .map(|v| {
                let k = [v.x, v.y, v.z].map(|c| (c / q).round() as i64);
                let n = ids.len() as u32;
                *ids.entry(k).or_insert(n)
            })
            .collect();
        let mut map: HashMap<(u32, u32), Vec<(usize, usize, usize)>> = HashMap::new();
        let mut fnorm = Vec::with_capacity(tris.len());
        for (ti, t) in tris.iter().enumerate() {
            let [a, b, c] = t.map(|i| lv[i as usize]);
            fnorm.push((b - a).cross(c - a).normalize_or_zero());
            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                let (wa, wb) = (weld[t[i] as usize], weld[t[j] as usize]);
                if wa == wb {
                    continue;
                }
                map.entry((wa.min(wb), wa.max(wb))).or_default().push((
                    ti,
                    t[i] as usize,
                    t[j] as usize,
                ));
            }
        }
        let cos = CREASE.to_radians().cos();
        let mut edges = Vec::new();
        for list in map.values() {
            let [(t1, a, b), (t2, ..)] = list.as_slice() else {
                continue;
            };
            let (n1, n2) = (fnorm[*t1], fnorm[*t2]);
            if n1 == Vec3::ZERO || n2 == Vec3::ZERO || n1.dot(n2) > cos {
                continue;
            }
            edges.push((lv[*a], lv[*b], n1, n2));
        }
        let cell = radius.max(diag / 256.0);
        let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        for (ei, (a, b, ..)) in edges.iter().enumerate() {
            let c0 = (a.min(*b) - Vec3::splat(radius)) / cell;
            let c1 = (a.max(*b) + Vec3::splat(radius)) / cell;
            let (c0, c1) = (c0.floor().as_ivec3(), c1.floor().as_ivec3());
            for x in c0.x..=c1.x {
                for y in c0.y..=c1.y {
                    for z in c0.z..=c1.z {
                        grid.entry([x, y, z]).or_default().push(ei as u32);
                    }
                }
            }
        }
        Edges {
            edges,
            grid,
            cell,
            radius,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// The rounded normal at `p` on a face with normal `nf`, or None when
    /// no crease of that face is within the radius.
    pub fn bend(&self, p: Vec3, nf: Vec3) -> Option<Vec3> {
        let c = (p / self.cell).floor().as_ivec3();
        let list = self.grid.get(&[c.x, c.y, c.z])?;
        let mut best: Option<(f32, Vec3)> = None;
        for &ei in list {
            let (a, b, n1, n2) = self.edges[ei as usize];
            // Only the creases of this face's own plane.
            if nf.dot(n1) < 0.999 && nf.dot(n2) < 0.999 {
                continue;
            }
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_squared().max(1e-20)).clamp(0.0, 1.0);
            let d = (p - (a + ab * t)).length();
            if d < self.radius && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, (n1 + n2).normalize_or(nf)));
            }
        }
        let (d, avg) = best?;
        Some(slerp(nf, avg, 1.0 - d / self.radius))
    }
}

fn slerp(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    let c = a.dot(b).clamp(-1.0, 1.0);
    let th = c.acos();
    if th < 1e-5 {
        return a;
    }
    let s = th.sin();
    (a * ((1.0 - t) * th).sin() / s + b * (t * th).sin() / s).normalize_or(a)
}

/// The world position of a scaled-local point of `part`.
pub fn world(part: &Part, p: Vec3) -> Vec3 {
    Quat::from_array(part.rotation).normalize() * p + Vec3::from(part.translation)
}

/// Applies the macro drift at world point `w` to a texel's colour and
/// roughness.
pub fn apply_macro(m: &Macro, w: Vec3, base: &mut [f32; 3], rough: &mut f32) {
    let l = noise::Lattice::new(m.scale, 0.0);
    let n = |seed: u32| {
        noise::fractal(
            |p, l, s| noise::value(p, 3, l, s),
            w.to_array(),
            l,
            m.seed.wrapping_mul(7919).wrapping_add(seed),
            3,
            2.0,
            0.5,
            "fbm",
        ) * 2.0
            - 1.0
    };
    let (n1, n2, n3) = (n(1), n(2), n(3));
    let k = 1.0 + m.amount * n1;
    let warm = 0.3 * m.amount * n2;
    base[0] *= k * (1.0 + warm);
    base[1] *= k;
    base[2] *= k * (1.0 - warm);
    *rough += 0.5 * m.amount * n3;
    if m.ground_dirt > 0.0 {
        let t = (w.y / m.ground_height).clamp(0.0, 1.0);
        let fade = 1.0 - t * t * (3.0 - 2.0 * t);
        let g = (m.ground_dirt * fade * (1.0 + 0.5 * n1)).clamp(0.0, 1.0);
        let dirt = [0.32, 0.27, 0.21];
        for (c, d) in base.iter_mut().zip(dirt) {
            *c += (*c * d - *c) * g;
        }
        *rough += (1.0 - *rough) * 0.5 * g;
    }
    for c in base.iter_mut() {
        *c = c.clamp(0.0, 1.0);
    }
    *rough = rough.clamp(0.0, 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_has_twelve_creases_and_bends_near_them() {
        let p = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let lv: Vec<Vec3> = p.verts.iter().map(|v| Vec3::from(*v)).collect();
        let tris: Vec<[u32; 3]> = p.indices.as_chunks::<3>().0.to_vec();
        let e = Edges::new(&lv, &tris, 0.05);
        assert_eq!(e.edges.len(), 12);
        let top = Vec3::Y;
        let near = e
            .bend(Vec3::new(0.49, 0.5, 0.0), top)
            .unwrap_or_else(|| panic!("no bend near the edge"));
        assert!(near.x > 0.3 && near.y > 0.6, "{near}");
        assert!(e.bend(Vec3::new(0.0, 0.5, 0.0), top).is_none());
    }

    #[test]
    fn macro_darkens_toward_the_ground() {
        let m = Macro {
            amount: 0.0,
            scale: 4.0,
            ground_dirt: 0.8,
            ground_height: 0.5,
            seed: 0,
        };
        let (mut lo, mut hi) = ([0.8f32; 3], [0.8f32; 3]);
        let (mut r0, mut r1) = (0.4, 0.4);
        apply_macro(&m, Vec3::new(0.0, 0.0, 0.0), &mut lo, &mut r0);
        apply_macro(&m, Vec3::new(0.0, 2.0, 0.0), &mut hi, &mut r1);
        assert!(lo[0] < hi[0] - 0.2, "{lo:?} {hi:?}");
        assert!(r0 > r1);
    }
}
