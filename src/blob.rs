//! Smooth blending of solids (`blob`) and plane splits (`split`).
//!
//! A blob samples each input as a signed distance field on a grid (exact
//! point-to-triangle distance in a narrow band around the surface, sign by
//! ray parity along x), combines them with a polynomial smooth-min of radius
//! `blend` (or smooth subtract / intersect), and meshes the zero level with
//! Manifold's level-set mesher, so the result is a closed, outward solid.

use crate::{ModelerError, Part};
use bevy::math::Vec3;
use manifold_csg::Manifold;
use rayon::prelude::*;

/// Most grid samples one blob may take (per input grid and for the meshing
/// grid), about a 300^3 box.
const MAX_SAMPLES: f64 = 27e6;

/// A signed distance grid (negative inside), clamped to +-`band`.
pub(crate) struct Grid {
    lo: Vec3,
    res: f32,
    n: [usize; 3],
    band: f32,
    d: Vec<f32>,
}

impl Grid {
    fn at(&self, i: usize, j: usize, k: usize) -> f32 {
        self.d[(k * self.n[1] + j) * self.n[0] + i]
    }

    /// Trilinear sample; outside the grid is "far outside" (+band).
    pub(crate) fn sample(&self, p: Vec3) -> f32 {
        let g = (p - self.lo) / self.res;
        let max = |a: usize| (a - 1) as f32;
        if g.x < 0.0
            || g.y < 0.0
            || g.z < 0.0
            || g.x >= max(self.n[0])
            || g.y >= max(self.n[1])
            || g.z >= max(self.n[2])
        {
            return self.band;
        }
        let (i, j, k) = (g.x as usize, g.y as usize, g.z as usize);
        let (fx, fy, fz) = (g.x - i as f32, g.y - j as f32, g.z - k as f32);
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c = |di, dj, dk| self.at(i + di, j + dj, k + dk);
        let x00 = lerp(c(0, 0, 0), c(1, 0, 0), fx);
        let x10 = lerp(c(0, 1, 0), c(1, 1, 0), fx);
        let x01 = lerp(c(0, 0, 1), c(1, 0, 1), fx);
        let x11 = lerp(c(0, 1, 1), c(1, 1, 1), fx);
        lerp(lerp(x00, x10, fy), lerp(x01, x11, fy), fz)
    }
}

/// World-space triangles of a closed part (welded by Manifold, so the
/// parity test sees one clean surface). Errors for sheets and open meshes.
fn world_tris(part: &Part) -> Result<Vec<[Vec3; 3]>, ModelerError> {
    let m = crate::csg::part_to_manifold(part)?;
    let mesh = m.to_meshgl64();
    let n = mesh.num_prop().max(3);
    let props = mesh.vert_properties();
    let pos: Vec<Vec3> = props
        .chunks(n)
        .map(|v| Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32))
        .collect();
    let idx = mesh.tri_verts();
    Ok(idx
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [pos[t[0] as usize], pos[t[1] as usize], pos[t[2] as usize]])
        .collect())
}

/// Closest distance from `p` to triangle `t` (Ericson, Real-Time Collision
/// Detection 5.1.5).
fn tri_dist(p: Vec3, t: &[Vec3; 3]) -> f32 {
    let [a, b, c] = *t;
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return ap.length();
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return bp.length();
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return (p - (a + ab * v)).length();
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return cp.length();
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return (p - (a + ac * w)).length();
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return (p - (b + (c - b) * w)).length();
    }
    let denom = 1.0 / (va + vb + vc);
    let (v, w) = (vb * denom, vc * denom);
    (p - (a + ab * v + ac * w)).length()
}

/// Samples a closed part's signed distance on a grid of `res` spacing over
/// its box grown by `band + res`.
pub(crate) fn sdf_grid(part: &Part, res: f32, band: f32) -> Result<Grid, ModelerError> {
    let tris = world_tris(part)?;
    let (blo, bhi) = part.bounds();
    let pad = Vec3::splat(band + 2.0 * res);
    let lo = blo - pad;
    let ext = bhi + pad - lo;
    let n = [
        (ext.x / res).ceil() as usize + 1,
        (ext.y / res).ceil() as usize + 1,
        (ext.z / res).ceil() as usize + 1,
    ];
    let pt = |i: usize, j: usize, k: usize| lo + Vec3::new(i as f32, j as f32, k as f32) * res;
    let slice = n[0] * n[1];
    let mut d = vec![band; slice * n[2]];
    // Unsigned distance in the band: per z slice, every triangle near it
    // updates the grid points in its box grown by the band.
    d.par_chunks_mut(slice).enumerate().for_each(|(k, out)| {
        let z = lo.z + k as f32 * res;
        for t in &tris {
            let tlo = t[0].min(t[1]).min(t[2]) - Vec3::splat(band);
            let thi = t[0].max(t[1]).max(t[2]) + Vec3::splat(band);
            if z < tlo.z || z > thi.z {
                continue;
            }
            let cell = |v: f32, o: f32, m: usize| (((v - o) / res).max(0.0) as usize).min(m - 1);
            let (i0, i1) = (cell(tlo.x, lo.x, n[0]), cell(thi.x, lo.x, n[0]) + 1);
            let (j0, j1) = (cell(tlo.y, lo.y, n[1]), cell(thi.y, lo.y, n[1]) + 1);
            for j in j0..j1.min(n[1]) {
                for i in i0..i1.min(n[0]) {
                    let o = &mut out[j * n[0] + i];
                    let dist = tri_dist(pt(i, j, k), t);
                    if dist < *o {
                        *o = dist;
                    }
                }
            }
        }
    });
    // Sign by parity: a ray along +x from each (y, z) row, crossings counted
    // left of each sample. Triangles are bucketed by the rows they span; the
    // ray is nudged off the grid lines so it never runs along an edge.
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); n[1] * n[2]];
    for (ti, t) in tris.iter().enumerate() {
        let tlo = t[0].min(t[1]).min(t[2]);
        let thi = t[0].max(t[1]).max(t[2]);
        let j0 = ((tlo.y - lo.y) / res).floor().max(0.0) as usize;
        let j1 = (((thi.y - lo.y) / res).ceil() as usize).min(n[1] - 1);
        let k0 = ((tlo.z - lo.z) / res).floor().max(0.0) as usize;
        let k1 = (((thi.z - lo.z) / res).ceil() as usize).min(n[2] - 1);
        for k in k0..=k1 {
            for j in j0..=j1 {
                buckets[k * n[1] + j].push(ti as u32);
            }
        }
    }
    let (ny_off, nz_off) = (res * 1.234_567e-4, res * 2.345_679e-4);
    d.par_chunks_mut(n[0]).enumerate().for_each(|(row, out)| {
        let (j, k) = (row % n[1], row / n[1]);
        let y = lo.y + j as f32 * res + ny_off;
        let z = lo.z + k as f32 * res + nz_off;
        let mut xs: Vec<f32> = Vec::new();
        for &ti in &buckets[row] {
            let [a, b, c] = tris[ti as usize];
            // Barycentric of (y, z) in the triangle projected on yz.
            let det = (b.y - a.y) * (c.z - a.z) - (c.y - a.y) * (b.z - a.z);
            if det.abs() < 1e-20 {
                continue;
            }
            let u = ((y - a.y) * (c.z - a.z) - (c.y - a.y) * (z - a.z)) / det;
            let v = ((b.y - a.y) * (z - a.z) - (y - a.y) * (b.z - a.z)) / det;
            if u < 0.0 || v < 0.0 || u + v > 1.0 {
                continue;
            }
            xs.push(a.x + u * (b.x - a.x) + v * (c.x - a.x));
        }
        if xs.is_empty() {
            return;
        }
        xs.sort_by(f32::total_cmp);
        let mut c = 0;
        for (i, o) in out.iter_mut().enumerate() {
            let x = lo.x + i as f32 * res;
            while c < xs.len() && xs[c] < x {
                c += 1;
            }
            if c % 2 == 1 {
                *o = -*o;
            }
        }
    });
    Ok(Grid {
        lo,
        res,
        n,
        band,
        d,
    })
}

/// Polynomial smooth minimum of radius `k` (Quilez): equals `min` once the
/// two are more than `k` apart, and rounds the crease between them.
fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

fn smax(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, -b, k)
}

/// The default blend: a fifth of the smallest input's thinnest box size.
pub fn default_blend(parts: &[&Part]) -> f32 {
    parts
        .iter()
        .map(|p| {
            let (lo, hi) = p.bounds();
            (hi - lo).min_element()
        })
        .fold(f32::MAX, f32::min)
        * 0.2
}

/// Smoothly blends closed parts into one solid. `mode`: `union` (every input
/// melted together), `subtract` (the rest carved out of the first with a
/// rounded rim), `intersect` (the common volume, rounded). `blend` is the
/// fillet radius in meters (0 = a sharp boolean), `resolution` the grid
/// spacing (0 = automatic: about 1/120 of the result's size). The first
/// input's material.
pub fn blob(
    name: &str,
    parts: &[&Part],
    mode: &str,
    blend: f32,
    resolution: f32,
) -> Result<Part, ModelerError> {
    let err = |m: String| ModelerError::Csg(format!("blob `{name}`: {m}"));
    let Some(first) = parts.first() else {
        return Err(err("needs at least one input".into()));
    };
    if !matches!(mode, "union" | "subtract" | "intersect") {
        return Err(err(format!(
            "op must be union, subtract or intersect, got `{mode}`"
        )));
    }
    let k = if blend.is_nan() || blend < 0.0 {
        return Err(err(format!("blend must be >= 0 meters, got {blend}")));
    } else {
        blend
    };
    // The result's box: what the mesher samples.
    let boxes: Vec<(Vec3, Vec3)> = parts.iter().map(|p| p.bounds()).collect();
    let (mut lo, mut hi) = boxes[0];
    if mode == "union" {
        for (a, b) in &boxes[1..] {
            lo = lo.min(*a);
            hi = hi.max(*b);
        }
    }
    let grow = Vec3::splat(k);
    lo -= grow;
    hi += grow;
    let size = (hi - lo).max_element();
    let res = if resolution > 0.0 {
        resolution
    } else {
        (size / 120.0).max(1e-4)
    };
    let cells = |e: Vec3, pad: f32| -> f64 {
        let e = e + Vec3::splat(2.0 * pad);
        (e.x / res + 1.0) as f64 * (e.y / res + 1.0) as f64 * (e.z / res + 1.0) as f64
    };
    let band = k + 3.0 * res;
    let worst = boxes
        .iter()
        .map(|(a, b)| cells(*b - *a, band + 2.0 * res))
        .fold(cells(hi - lo, res), f64::max);
    if worst > MAX_SAMPLES {
        let need = res * (worst / MAX_SAMPLES).cbrt() as f32;
        return Err(err(format!(
            "resolution {res} m would sample {:.0}M grid points over a {:.3} m box (limit {:.0}M); \
             use resolution >= {:.4} (or omit it: the default is 1/120 of the size)",
            worst / 1e6,
            size,
            MAX_SAMPLES / 1e6,
            need * 1.01
        )));
    }
    let grids: Vec<Grid> = parts
        .par_iter()
        .map(|p| sdf_grid(p, res, band))
        .collect::<Result<_, _>>()?;
    let field = |p: Vec3| -> f32 {
        let mut d = grids[0].sample(p);
        for g in &grids[1..] {
            let e = g.sample(p);
            d = match mode {
                "union" => smin(d, e, k),
                "subtract" => smax(d, -e, k),
                _ => smax(d, e, k),
            };
        }
        d
    };
    // Manifold's level set is positive inside.
    let pad = Vec3::splat(res);
    let (blo, bhi) = (lo - pad, hi + pad);
    let m = Manifold::from_sdf(
        |x, y, z| -field(Vec3::new(x as f32, y as f32, z as f32)) as f64,
        (
            [blo.x as f64, blo.y as f64, blo.z as f64],
            [bhi.x as f64, bhi.y as f64, bhi.z as f64],
        ),
        res as f64,
        0.0,
        -1.0,
    );
    m.status()
        .map_err(|e| err(format!("meshing failed: {e}")))?;
    if m.is_empty() || m.volume().abs() < 1e-12 {
        return Err(err(match mode {
            "subtract" => "the cutters remove everything".into(),
            "intersect" => "the inputs share no volume".into(),
            _ => "nothing to mesh (inputs too thin for the resolution?)".into(),
        }));
    }
    let mut out = crate::csg::manifold_to_part(&m, name, first.material.clone());
    out.visible = true;
    Ok(out)
}

/// Cuts a closed part with the plane through `point` with `normal`: the
/// halves above (the side `normal` points to) and below, both closed with
/// flat caps. `gap` pushes them apart along the normal (half each way).
pub fn split(
    name: &str,
    part: &Part,
    normal: [f32; 3],
    point: [f32; 3],
    gap: f32,
) -> Result<(Part, Part), ModelerError> {
    let n = Vec3::from(normal);
    if !n.is_finite() || n.length() < 1e-9 {
        return Err(ModelerError::Csg(format!(
            "split `{name}`: plane normal must be a non-zero direction, got {normal:?}"
        )));
    }
    let n = n.normalize();
    let offset = n.dot(Vec3::from(point));
    let m = crate::csg::part_to_manifold(part)?;
    let (above, below) = m.split_by_plane([n.x as f64, n.y as f64, n.z as f64], offset as f64);
    let (lo, hi) = part.bounds();
    let empty = |h: &Manifold| h.is_empty() || h.volume().abs() < 1e-15;
    if empty(&above) || empty(&below) {
        let side = if empty(&above) { "above" } else { "below" };
        // The part's extent along the normal: the box corners projected.
        let (mut a, mut b) = (f32::MAX, f32::MIN);
        for c in 0..8 {
            let pick = |bit: usize, l: f32, h: f32| if c & bit == 0 { l } else { h };
            let s = n.dot(Vec3::new(
                pick(1, lo.x, hi.x),
                pick(2, lo.y, hi.y),
                pick(4, lo.z, hi.z),
            ));
            a = a.min(s);
            b = b.max(s);
        }
        return Err(ModelerError::Csg(format!(
            "split `{name}`: the plane misses `{}` (nothing {side} it): the part spans {a:.3}..{b:.3} \
             along the normal and the plane is at {offset:.3}",
            part.name
        )));
    }
    let half = |h: &Manifold, side: &str, sign: f32| {
        let mut p =
            crate::csg::manifold_to_part(h, &format!("{name}_{side}"), part.material.clone());
        p.visible = true;
        p.translation = (n * (sign * gap * 0.5)).to_array();
        p
    };
    Ok((half(&above, "above", 1.0), half(&below, "below", -1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrimitiveKind;

    fn sphere(r: f32, at: [f32; 3]) -> Part {
        let mut p = crate::primitives::primitive(PrimitiveKind::Sphere, [r, 0.0, 0.0], "s")
            .unwrap_or_else(|e| panic!("{e}"));
        p.translation = at;
        p
    }

    fn volume(p: &Part) -> f64 {
        crate::csg::part_to_manifold(p)
            .unwrap_or_else(|e| panic!("{e}"))
            .volume()
    }

    /// Cross-section area of a part at the plane x = 0 (by Manifold slice of a
    /// thin slab), to measure the neck between two blobs.
    fn neck(p: &Part) -> f64 {
        let m = crate::csg::part_to_manifold(p).unwrap_or_else(|e| panic!("{e}"));
        let slab = Manifold::cube(0.002, 10.0, 10.0, true);
        m.intersection(&slab).volume() / 0.002
    }

    #[test]
    fn blob_is_closed_outward_and_blend_widens_the_neck() {
        let a = sphere(0.1, [-0.09, 0.2, 0.0]);
        let b = sphere(0.1, [0.09, 0.2, 0.0]);
        let sharp = blob("u", &[&a, &b], "union", 0.0, 0.005).unwrap_or_else(|e| panic!("{e}"));
        let soft = blob("u", &[&a, &b], "union", 0.08, 0.005).unwrap_or_else(|e| panic!("{e}"));
        for p in [&sharp, &soft] {
            assert_eq!(crate::verify::open_edge_count(p), 0, "closed");
            assert!(volume(p) > 0.0, "outward");
        }
        let (n0, n1) = (neck(&sharp), neck(&soft));
        assert!(n1 > n0 * 1.3, "blend widens the neck: {n0} -> {n1}");
        // A sharp blob is close to the boolean union.
        let exact = crate::csg::part_to_manifold(&a)
            .and_then(|m| Ok(m.union(&crate::csg::part_to_manifold(&b)?)))
            .unwrap_or_else(|e| panic!("{e}"))
            .volume();
        let v = volume(&sharp);
        assert!((v - exact).abs() / exact < 0.05, "{v} vs {exact}");
    }

    #[test]
    fn blob_subtract_and_intersect() {
        let a = sphere(0.1, [0.0, 0.2, 0.0]);
        let b = sphere(0.06, [0.08, 0.2, 0.0]);
        let va = volume(&a);
        let sub = blob("s", &[&a, &b], "subtract", 0.02, 0.005).unwrap_or_else(|e| panic!("{e}"));
        let int = blob("i", &[&a, &b], "intersect", 0.02, 0.005).unwrap_or_else(|e| panic!("{e}"));
        assert!(volume(&sub) < va * 0.95 && volume(&sub) > 0.0);
        assert!(volume(&int) < volume(&b) && volume(&int) > 0.0);
        assert_eq!(crate::verify::open_edge_count(&sub), 0);
    }

    #[test]
    fn blob_refuses_huge_grids() {
        let a = sphere(1.0, [0.0, 1.0, 0.0]);
        let e = blob("x", &[&a], "union", 0.1, 0.001)
            .err()
            .map(|e| e.to_string());
        assert!(e.is_some_and(|m| m.contains("resolution >=")));
    }

    #[test]
    fn split_halves_sum_to_the_input() {
        let a = sphere(0.1, [0.0, 0.1, 0.0]);
        let (up, down) = split("h", &a, [0.0, 1.0, 0.0], [0.0, 0.13, 0.0], 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(up.name, "h_above");
        for p in [&up, &down] {
            assert_eq!(crate::verify::open_edge_count(p), 0, "caps closed");
        }
        let (v, vu, vd) = (volume(&a), volume(&up), volume(&down));
        assert!(((vu + vd) - v).abs() < v * 1e-4, "{vu} + {vd} vs {v}");
        assert!(vu < vd);
        let (lo, _) = up.bounds();
        assert!((lo.y - 0.13).abs() < 1e-4);
        // A gap pushes the halves apart; a plane that misses errors.
        let (up, _) = split("h", &a, [0.0, 2.0, 0.0], [0.0, 0.13, 0.0], 0.02)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!((up.bounds().0.y - 0.14).abs() < 1e-4);
        assert!(split("h", &a, [0.0, 1.0, 0.0], [0.0, 0.5, 0.0], 0.0).is_err());
    }
}
