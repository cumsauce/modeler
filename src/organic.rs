//! Checks for organic shapes: self-intersections (a deform or skin that
//! folded the surface through itself), the thinnest point of a part (a limb
//! pinched to nothing), and per-vertex mean curvature (lumps and pinches,
//! drawn as a heat map by `agent_views` style "curvature").

use std::collections::HashMap;

use crate::Part;
use bevy::math::Vec3;
use rayon::prelude::*;

/// A part's triangles in world space, welded by position (`ids` per corner).
struct World {
    v: Vec<Vec3>,
    t: Vec<[u32; 3]>,
    /// Welded id of each of the part's own vertices.
    of: Vec<u32>,
}

fn world(part: &Part) -> World {
    let n = part.verts.len();
    let pos: Vec<Vec3> = (0..n).map(|i| part.world_vert(i)).collect();
    let (lo, hi) = pos.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), p| (a.min(*p), b.max(*p)),
    );
    let inv = 1.0 / ((hi - lo).max_element() * 1e-6).clamp(1e-7, 1e-5);
    let mut map: HashMap<[i64; 3], u32> = HashMap::new();
    let mut v = Vec::new();
    let of: Vec<u32> = pos
        .iter()
        .map(|p| {
            let k = [
                (p.x * inv).round() as i64,
                (p.y * inv).round() as i64,
                (p.z * inv).round() as i64,
            ];
            *map.entry(k).or_insert_with(|| {
                v.push(*p);
                (v.len() - 1) as u32
            })
        })
        .collect();
    let idx: Vec<u32> = if part.indices.is_empty() {
        (0..(n - n % 3) as u32).collect()
    } else {
        part.indices.clone()
    };
    let t = idx
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|t| t.iter().all(|&i| (i as usize) < n))
        .map(|t| t.map(|i| of[i as usize]))
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    World { v, t, of }
}

/// A uniform grid of triangle lists over a part's box.
struct TriGrid {
    lo: Vec3,
    cell: f32,
    n: [usize; 3],
    cells: Vec<Vec<u32>>,
}

impl TriGrid {
    fn new(w: &World) -> TriGrid {
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &w.v {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let ext = (hi - lo).max(Vec3::splat(1e-6));
        // About two triangles' worth of cells per triangle, at most 128 a side.
        let target = (w.t.len().max(1) as f32 * 2.0).cbrt().clamp(1.0, 128.0);
        let cell = (ext.max_element() / target).max(1e-6);
        let n = [
            ((ext.x / cell).ceil() as usize).max(1),
            ((ext.y / cell).ceil() as usize).max(1),
            ((ext.z / cell).ceil() as usize).max(1),
        ];
        let mut cells = vec![Vec::new(); n[0] * n[1] * n[2]];
        let idx =
            |p: Vec3, k: usize| (((p[k] - lo[k]) / cell).floor().max(0.0) as usize).min(n[k] - 1);
        for (i, t) in w.t.iter().enumerate() {
            let [a, b, c] = t.map(|j| w.v[j as usize]);
            let (mn, mx) = (a.min(b).min(c), a.max(b).max(c));
            for z in idx(mn, 2)..=idx(mx, 2) {
                for y in idx(mn, 1)..=idx(mx, 1) {
                    for x in idx(mn, 0)..=idx(mx, 0) {
                        cells[(z * n[1] + y) * n[0] + x].push(i as u32);
                    }
                }
            }
        }
        TriGrid { lo, cell, n, cells }
    }

    /// First hit of the ray `o + t d` (t in (tmin, tmax)), walking cells.
    fn ray(&self, w: &World, o: Vec3, d: Vec3, tmin: f32, tmax: f32, skip: u32) -> Option<f32> {
        let g = (o - self.lo) / self.cell;
        let mut c = [0i64; 3];
        let mut step = [0i64; 3];
        let mut tnext = [f32::MAX; 3];
        let mut tdelta = [f32::MAX; 3];
        for k in 0..3 {
            c[k] = (g[k].floor() as i64).clamp(0, self.n[k] as i64 - 1);
            if d[k] > 0.0 {
                step[k] = 1;
                tnext[k] = ((c[k] + 1) as f32 * self.cell + self.lo[k] - o[k]) / d[k];
                tdelta[k] = self.cell / d[k];
            } else if d[k] < 0.0 {
                step[k] = -1;
                tnext[k] = (c[k] as f32 * self.cell + self.lo[k] - o[k]) / d[k];
                tdelta[k] = -self.cell / d[k];
            }
        }
        let mut best = tmax;
        loop {
            let i = ((c[2] as usize * self.n[1]) + c[1] as usize) * self.n[0] + c[0] as usize;
            for &ti in &self.cells[i] {
                if ti == skip {
                    continue;
                }
                let [a, b, cc] = w.t[ti as usize].map(|j| w.v[j as usize]);
                if let Some(t) = ray_tri(o, d, a, b, cc)
                    && t > tmin
                    && t < best
                {
                    best = t;
                }
            }
            let k = if tnext[0] <= tnext[1] && tnext[0] <= tnext[2] {
                0
            } else if tnext[1] <= tnext[2] {
                1
            } else {
                2
            };
            if tnext[k] >= best {
                break;
            }
            c[k] += step[k];
            if c[k] < 0 || c[k] >= self.n[k] as i64 {
                break;
            }
            tnext[k] += tdelta[k];
        }
        (best < tmax).then_some(best)
    }
}

/// Möller-Trumbore, both sides.
fn ray_tri(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let (e1, e2) = (b - a, c - a);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(e2.dot(q) * inv)
}

/// Does segment p..q cross triangle abc (strictly inside both)?
fn seg_tri(p: Vec3, q: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<Vec3> {
    let d = q - p;
    let t = ray_tri(p, d, a, b, c)?;
    (t > 1e-6 && t < 1.0 - 1e-6).then(|| p + d * t)
}

/// Pairs of non-adjacent triangles of `part` that pass through each other:
/// the count and up to five crossing points.
pub fn self_intersections(part: &Part) -> (usize, Vec<Vec3>) {
    let w = world(part);
    if w.t.len() < 4 {
        return (0, Vec::new());
    }
    let g = TriGrid::new(&w);
    let hits: Vec<(u32, u32, Vec3)> = g
        .cells
        .par_iter()
        .enumerate()
        .flat_map_iter(|(ci, list)| {
            let mut out = Vec::new();
            for (x, &i) in list.iter().enumerate() {
                let ti = w.t[i as usize];
                let [a, b, c] = ti.map(|j| w.v[j as usize]);
                let (amn, amx) = (a.min(b).min(c), a.max(b).max(c));
                for &j in &list[x + 1..] {
                    let tj = w.t[j as usize];
                    if ti.iter().any(|v| tj.contains(v)) {
                        continue;
                    }
                    let [d, e, f] = tj.map(|k| w.v[k as usize]);
                    let (bmn, bmx) = (d.min(e).min(f), d.max(e).max(f));
                    if amx.cmplt(bmn).any() || bmx.cmplt(amn).any() {
                        continue;
                    }
                    // Count the pair once: in the first cell both share.
                    let first = first_shared_cell(&g, amn.max(bmn));
                    if first != ci {
                        continue;
                    }
                    let hit = seg_tri(a, b, d, e, f)
                        .or_else(|| seg_tri(b, c, d, e, f))
                        .or_else(|| seg_tri(c, a, d, e, f))
                        .or_else(|| seg_tri(d, e, a, b, c))
                        .or_else(|| seg_tri(e, f, a, b, c))
                        .or_else(|| seg_tri(f, d, a, b, c));
                    if let Some(p) = hit {
                        out.push((i, j, p));
                    }
                }
            }
            out.into_iter()
        })
        .collect();
    let pts = hits.iter().take(5).map(|h| h.2).collect();
    (hits.len(), pts)
}

fn first_shared_cell(g: &TriGrid, p: Vec3) -> usize {
    let k = |a: usize| (((p[a] - g.lo[a]) / g.cell).floor().max(0.0) as usize).min(g.n[a] - 1);
    (k(2) * g.n[1] + k(1)) * g.n[0] + k(0)
}

/// How thick a part is: from up to `samples` surface points, the distance
/// straight through the part along the inward normal.
#[derive(Debug, Clone)]
pub struct Thickness {
    pub min: f32,
    pub at: Vec3,
    pub p5: f32,
    pub median: f32,
    pub samples: usize,
}

pub fn thickness(part: &Part, samples: usize) -> Option<Thickness> {
    let w = world(part);
    if w.t.is_empty() {
        return None;
    }
    let g = TriGrid::new(&w);
    // Area-weighted sample: triangle centroids, a stride over the list.
    let stride = (w.t.len() / samples.max(1)).max(1);
    let size = {
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &w.v {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        (hi - lo).max_element()
    };
    let eps = size * 1e-5;
    let mut ts: Vec<(f32, Vec3)> = (0..w.t.len())
        .step_by(stride)
        .collect::<Vec<_>>()
        .par_iter()
        .filter_map(|&i| {
            let [a, b, c] = w.t[i].map(|j| w.v[j as usize]);
            let n = (b - a).cross(c - a);
            if n.length_squared() < 1e-20 {
                return None;
            }
            let n = n.normalize();
            let o = (a + b + c) / 3.0;
            g.ray(&w, o, -n, eps, size * 2.0, i as u32).map(|t| (t, o))
        })
        .collect();
    if ts.is_empty() {
        return None;
    }
    ts.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(Thickness {
        min: ts[0].0,
        at: ts[0].1,
        p5: ts[ts.len() / 20].0,
        median: ts[ts.len() / 2].0,
        samples: ts.len(),
    })
}

/// Mean curvature per vertex of `part` (1/m: +1/r on a sphere of radius r,
/// negative in hollows), from the welded umbrella Laplacian.
pub fn vertex_curvature(part: &Part) -> Vec<f32> {
    let w = world(part);
    let nv = w.v.len();
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); nv];
    let mut nrm = vec![Vec3::ZERO; nv];
    for t in &w.t {
        let [a, b, c] = t.map(|j| w.v[j as usize]);
        let n = (b - a).cross(c - a);
        for k in 0..3 {
            nrm[t[k] as usize] += n;
            let (x, y) = (t[k], t[(k + 1) % 3]);
            nb[x as usize].push(y);
            nb[y as usize].push(x);
        }
    }
    let k: Vec<f32> = (0..nv)
        .map(|i| {
            let l = &mut nb[i].clone();
            l.sort_unstable();
            l.dedup();
            if l.is_empty() {
                return 0.0;
            }
            let p = w.v[i];
            let c = l.iter().map(|&j| w.v[j as usize]).sum::<Vec3>() / l.len() as f32;
            let e2 = l
                .iter()
                .map(|&j| w.v[j as usize].distance_squared(p))
                .sum::<f32>()
                / l.len() as f32;
            let n = nrm[i].normalize_or_zero();
            if e2 <= 1e-20 {
                0.0
            } else {
                -2.0 * (c - p).dot(n) / e2
            }
        })
        .collect();
    w.of.iter().map(|&i| k[i as usize]).collect()
}

/// The heat-map colour (linear RGB) of curvature `k` on a part `size`
/// meters across: white flat, red convex, blue concave, saturated where
/// the radius of curvature is an eighth of the part or tighter.
pub fn curvature_color(k: f32, size: f32) -> [f32; 3] {
    let t = (k * size / 16.0).clamp(-1.0, 1.0);
    if t >= 0.0 {
        [1.0, 1.0 - 0.85 * t, 1.0 - 0.9 * t]
    } else {
        let t = -t;
        [1.0 - 0.9 * t, 1.0 - 0.6 * t, 1.0]
    }
}

/// `inspect thickness`: per visible part (matching `parts`, `*` globs),
/// the thinnest point straight through it, where, and the 5% / median.
pub fn thickness_report(
    model: &crate::Model,
    parts: Option<&[String]>,
) -> (String, serde_json::Value) {
    let picked: Vec<&Part> = model
        .parts
        .iter()
        .map(|p| p.as_ref())
        .filter(|p| p.visible && !p.verts.is_empty())
        .filter(|p| parts.is_none_or(|l| l.iter().any(|q| crate::name_matches(q, &p.name))))
        .collect();
    let rows: Vec<(String, Option<Thickness>, usize)> = picked
        .par_iter()
        .map(|p| (p.name.clone(), thickness(p, 4000), self_intersections(p).0))
        .collect();
    let mut text = vec![format!(
        "thickness (straight through each part along the inward normal, {} part(s)):",
        rows.len()
    )];
    let mut json = Vec::new();
    for (name, t, crossings) in &rows {
        let cross = if *crossings > 0 {
            format!("; SELF-INTERSECTS ({crossings} crossing triangle pairs)")
        } else {
            String::new()
        };
        match t {
            Some(t) => {
                text.push(format!(
                    "  {name}: thinnest {:.4} m at [{:.3}, {:.3}, {:.3}], 5% {:.4} m, median {:.4} m{cross}",
                    t.min, t.at.x, t.at.y, t.at.z, t.p5, t.median
                ));
                json.push(
                    serde_json::json!({"part": name, "min": t.min, "at": t.at.to_array(),
                    "p5": t.p5, "median": t.median, "self_intersections": crossings}),
                );
            }
            None => {
                text.push(format!(
                    "  {name}: no thickness (an open sheet or no triangles){cross}"
                ));
                json.push(serde_json::json!({"part": name, "self_intersections": crossings}));
            }
        }
    }
    if rows.is_empty() {
        text.push("  no visible part matches".into());
    }
    (text.join("\n"), serde_json::Value::Array(json))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere(r: f32) -> Part {
        crate::primitives::primitive(crate::PrimitiveKind::Sphere, [r, 0.0, 0.0], "s")
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_sphere_has_no_self_intersections_and_its_curvature_is_one_over_r() {
        let s = sphere(0.5);
        assert_eq!(self_intersections(&s).0, 0);
        let k = vertex_curvature(&s);
        let mut v = k.clone();
        v.sort_by(f32::total_cmp);
        let med = v[v.len() / 2];
        assert!((med - 2.0).abs() < 0.2, "median curvature {med}");
    }

    #[test]
    fn two_overlapping_spheres_in_one_part_self_intersect() {
        let a = sphere(0.5);
        let mut b = sphere(0.5);
        for v in &mut b.verts {
            v[0] += 0.4;
        }
        let mut g = a.clone();
        let off = g.verts.len() as u32;
        g.verts.extend(&b.verts);
        g.normals.clear();
        g.uvs.clear();
        g.indices.extend(b.indices.iter().map(|i| i + off));
        let (n, pts) = self_intersections(&g);
        assert!(n > 10, "{n}");
        assert!(pts.iter().all(|p| (p.x - 0.2).abs() < 0.1), "{pts:?}");
    }

    #[test]
    fn thickness_of_a_slab_is_its_thin_side() {
        let b = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0, 0.02, 0.5], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let t = thickness(&b, 500).unwrap_or_else(|| panic!("no thickness"));
        assert!((t.min - 0.02).abs() < 1e-3, "{t:?}");
    }
}
