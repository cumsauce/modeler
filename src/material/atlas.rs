//! Per-part texture atlases for object-domain materials: a box unwrap into
//! islands packed at one texel density, and a rasterizer that turns the
//! atlas into a sample set (object-space position and normal per texel, with
//! a dilated gutter so bilinear filtering never reads off the surface).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use bevy::math::Vec3;

use super::eval::Samples;
use super::node::Buf;
use crate::Part;
use crate::uv::metric_uv;

/// Gutter around each island, in texels.
const GUTTER: f32 = 3.0;

fn tri_count(p: &Part) -> usize {
    if p.indices.is_empty() {
        p.verts.len() / 3
    } else {
        p.indices.len() / 3
    }
}

fn corner(p: &Part, i: usize) -> usize {
    p.indices.get(i).copied().unwrap_or(i as u32) as usize
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Box-unwraps `part` into an `n` × `n` atlas: triangles are grouped by the
/// dominant axis and side of their normal, split into connected islands,
/// projected at real scale (so texel density is uniform) and shelf-packed
/// with a gutter. Rewrites the part's verts (split at island borders),
/// normals, uvs (0..1) and indices.
pub fn box_atlas(part: &mut Part, n: usize) {
    let sc = Vec3::from(part.scale);
    let count = part.verts.len();
    let tris = tri_count(part);
    // Chart key and projected corners (meters) per triangle.
    let mut chart = vec![0u8; tris];
    let mut proj = vec![[[0.0f32; 2]; 3]; tris];
    let mut ok = vec![false; tris];
    for t in 0..tris {
        let c = [
            corner(part, t * 3),
            corner(part, t * 3 + 1),
            corner(part, t * 3 + 2),
        ];
        if c.iter().any(|&k| k >= count) {
            continue;
        }
        let p = c.map(|k| Vec3::from(part.verts[k]) * sc);
        let nn = (p[1] - p[0]).cross(p[2] - p[0]);
        let a = nn.abs();
        let (axis, sign) = if a.x >= a.y && a.x >= a.z {
            (0u8, nn.x.signum())
        } else if a.y >= a.z {
            (1, nn.y.signum())
        } else {
            (2, nn.z.signum())
        };
        chart[t] = axis * 2 + u8::from(sign < 0.0);
        proj[t] = p.map(|w| metric_uv(axis, sign, w));
        ok[t] = true;
    }
    // Islands: triangles of one chart sharing a vertex.
    let mut parent: Vec<usize> = (0..tris).collect();
    let mut first: HashMap<(usize, u8), usize> = HashMap::new();
    for t in 0..tris {
        if !ok[t] {
            continue;
        }
        for k in 0..3 {
            let v = corner(part, t * 3 + k);
            match first.get(&(v, chart[t])) {
                Some(&o) => {
                    let (a, b) = (find(&mut parent, t), find(&mut parent, o));
                    if a != b {
                        parent[a.max(b)] = a.min(b);
                    }
                }
                None => {
                    first.insert((v, chart[t]), t);
                }
            }
        }
    }
    let mut island_of: HashMap<usize, usize> = HashMap::new();
    let mut boxes: Vec<([f32; 2], [f32; 2])> = Vec::new();
    let mut tri_island = vec![usize::MAX; tris];
    for t in 0..tris {
        if !ok[t] {
            continue;
        }
        let r = find(&mut parent, t);
        let id = *island_of.entry(r).or_insert_with(|| {
            boxes.push(([f32::MAX; 2], [f32::MIN; 2]));
            boxes.len() - 1
        });
        tri_island[t] = id;
        for q in proj[t] {
            let b = &mut boxes[id];
            b.0 = [b.0[0].min(q[0]), b.0[1].min(q[1])];
            b.1 = [b.1[0].max(q[0]), b.1[1].max(q[1])];
        }
    }
    // Shelf packing at density d texels per meter, shrinking d until it fits.
    let size = |b: &([f32; 2], [f32; 2])| [b.1[0] - b.0[0], b.1[1] - b.0[1]];
    let area: f32 = boxes
        .iter()
        .map(|b| size(b)[0] * size(b)[1])
        .sum::<f32>()
        .max(1e-12);
    let nf = n as f32;
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| {
        size(&boxes[b])[1]
            .total_cmp(&size(&boxes[a])[1])
            .then(a.cmp(&b))
    });
    let mut d = (0.8 * nf * nf / area).sqrt();
    let mut offs = vec![[0.0f32; 2]; boxes.len()];
    for _ in 0..60 {
        let (mut x, mut y, mut row_h) = (0.0f32, 0.0f32, 0.0f32);
        let mut fits = true;
        for &i in &order {
            let s = size(&boxes[i]);
            let (w, h) = (s[0] * d + 2.0 * GUTTER, s[1] * d + 2.0 * GUTTER);
            if w > nf {
                fits = false;
                break;
            }
            if x + w > nf {
                x = 0.0;
                y += row_h;
                row_h = 0.0;
            }
            offs[i] = [x + GUTTER, y + GUTTER];
            x += w.ceil();
            row_h = row_h.max(h.ceil());
        }
        if fits && y + row_h <= nf {
            break;
        }
        d *= 0.92;
    }
    // Rewrite the mesh: a vertex per (original vertex, island).
    let has_n = part.normals.len() == count;
    let (mut verts, mut normals, mut uvs, mut indices) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut remap: HashMap<(usize, usize), u32> = HashMap::new();
    for t in 0..tris {
        if !ok[t] {
            continue;
        }
        let isl = tri_island[t];
        for (k, q) in proj[t].into_iter().enumerate() {
            let v = corner(part, t * 3 + k);
            let id = *remap.entry((v, isl)).or_insert_with(|| {
                let b = boxes[isl].0;
                let px = [
                    offs[isl][0] + (q[0] - b[0]) * d,
                    offs[isl][1] + (q[1] - b[1]) * d,
                ];
                verts.push(part.verts[v]);
                if has_n {
                    normals.push(part.normals[v]);
                }
                uvs.push([px[0] / nf, px[1] / nf]);
                (verts.len() - 1) as u32
            });
            indices.push(id);
        }
    }
    part.verts = verts;
    part.normals = normals;
    part.uvs = uvs;
    part.indices = indices;
}

/// The sample set of a part's atlas (its uvs, 0..1, on an `n` × `n` grid):
/// each covered texel gets the object-space position (scale applied, meters)
/// and normal of the surface point there; the gutter is dilated from its
/// neighbours. Texels nowhere near the surface are invalid.
pub fn samples(part: &Part, n: usize) -> Samples {
    let n = n.max(4);
    let len = n * n;
    let sc = Vec3::from(part.scale);
    let mut pos = vec![[0.0f32; 3]; len];
    let mut nrm = vec![[0.0f32, 0.0, 1.0]; len];
    let mut valid = vec![false; len];
    let (mut area3, mut area2) = (0.0f32, 0.0f32);
    let has_n = part.normals.len() == part.verts.len();
    let nf = n as f32;
    for t in 0..tri_count(part) {
        let c = [
            corner(part, t * 3),
            corner(part, t * 3 + 1),
            corner(part, t * 3 + 2),
        ];
        if c.iter()
            .any(|&k| k >= part.verts.len() || k >= part.uvs.len())
        {
            continue;
        }
        let p = c.map(|k| Vec3::from(part.verts[k]) * sc);
        let fnrm = (p[1] - p[0]).cross(p[2] - p[0]);
        area3 += fnrm.length() * 0.5;
        let fnrm = fnrm.normalize_or_zero();
        let q = c.map(|k| [part.uvs[k][0] * nf, part.uvs[k][1] * nf]);
        let den =
            (q[1][1] - q[2][1]) * (q[0][0] - q[2][0]) + (q[2][0] - q[1][0]) * (q[0][1] - q[2][1]);
        area2 += den.abs() * 0.5;
        if den.abs() < 1e-12 {
            continue;
        }
        let lo = [
            q.iter().map(|v| v[0]).fold(f32::MAX, f32::min),
            q.iter().map(|v| v[1]).fold(f32::MAX, f32::min),
        ];
        let hi = [
            q.iter().map(|v| v[0]).fold(f32::MIN, f32::max),
            q.iter().map(|v| v[1]).fold(f32::MIN, f32::max),
        ];
        let (x0, x1) = (
            (lo[0] - 0.5).floor().max(0.0) as usize,
            (hi[0] + 0.5).ceil().min(nf - 1.0) as usize,
        );
        let (y0, y1) = (
            (lo[1] - 0.5).floor().max(0.0) as usize,
            (hi[1] + 0.5).ceil().min(nf - 1.0) as usize,
        );
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
                let e = -1e-4;
                if a < e || b < e || g < e {
                    continue;
                }
                let i = y * n + x;
                let w = p[0] * a + p[1] * b + p[2] * g;
                let nn = if has_n {
                    (Vec3::from(part.normals[c[0]]) * a
                        + Vec3::from(part.normals[c[1]]) * b
                        + Vec3::from(part.normals[c[2]]) * g)
                        .normalize_or(fnrm)
                } else {
                    fnrm
                };
                pos[i] = w.to_array();
                nrm[i] = nn.to_array();
                valid[i] = true;
            }
        }
    }
    // Dilate into the gutter (deterministic: scan order, 4-neighbours).
    for _ in 0..GUTTER as usize + 1 {
        let prev = valid.clone();
        for y in 0..n {
            for x in 0..n {
                let i = y * n + x;
                if prev[i] {
                    continue;
                }
                let nb = [
                    (x > 0).then(|| i - 1),
                    (x + 1 < n).then(|| i + 1),
                    (y > 0).then(|| i - n),
                    (y + 1 < n).then(|| i + n),
                ];
                if let Some(j) = nb.into_iter().flatten().find(|&j| prev[j]) {
                    pos[i] = pos[j];
                    nrm[i] = nrm[j];
                    valid[i] = true;
                }
            }
        }
    }
    // Meters per texel from the area ratio; uv in meters on the atlas.
    let texel = if area2 > 0.0 {
        (area3 / area2).sqrt()
    } else {
        1.0 / nf
    };
    let uv: Vec<[f32; 2]> = (0..len)
        .map(|i| {
            [
                ((i % n) as f32 + 0.5) * texel,
                ((i / n) as f32 + 0.5) * texel,
            ]
        })
        .collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("atlas", n).hash(&mut h);
    for v in part.verts.iter().chain(&part.normals) {
        v.map(f32::to_bits).hash(&mut h);
    }
    for v in &part.uvs {
        v.map(f32::to_bits).hash(&mut h);
    }
    part.indices.hash(&mut h);
    part.scale.map(f32::to_bits).hash(&mut h);
    Samples {
        w: n,
        h: n,
        uv: Arc::new(Buf::V2(uv)),
        pos: Arc::new(Buf::V3(pos)),
        nrm: Arc::new(Buf::V3(nrm)),
        valid: Some(Arc::new(valid)),
        period: 0.0,
        texel,
        key: h.finish(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_texels_land_on_the_surface() {
        let mut p = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0, 0.5, 0.25], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        box_atlas(&mut p, 128);
        assert!(
            p.uvs
                .iter()
                .all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]))
        );
        let s = samples(&p, 128);
        let Buf::V3(pos) = &*s.pos else { panic!("pos") };
        let valid = s.valid.clone().unwrap_or_default();
        let covered = valid.iter().filter(|v| **v).count();
        assert!(covered > 128 * 128 / 3, "covered {covered}");
        // Every valid texel lies on (or within a gutter texel of) the box surface.
        for (i, v) in valid.iter().enumerate() {
            if !v {
                continue;
            }
            let q = pos[i];
            let d = [
                (q[0].abs() - 0.5).abs(),
                (q[1].abs() - 0.25).abs(),
                (q[2].abs() - 0.125).abs(),
            ];
            let inside =
                q[0].abs() <= 0.5 + 1e-4 && q[1].abs() <= 0.25 + 1e-4 && q[2].abs() <= 0.125 + 1e-4;
            assert!(inside && d.iter().any(|x| *x < 1e-3), "texel {i} at {q:?}");
        }
        // Uniform density: texel size is the same everywhere (~ meters / texel).
        assert!(s.texel > 0.0 && s.texel < 0.05, "{}", s.texel);
    }
}
