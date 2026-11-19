//! Finishing pass for meshed level sets (`sdf`, `skin`, `body`, `silhouette`,
//! `blob`, `smooth remesh`).
//!
//! Manifold's marching tetrahedra puts every vertex on a linear crossing of a
//! lattice edge: smooth areas band along the lattice and sharp joins come out
//! as zigzag chamfers. This pass, on the position-welded mesh:
//!
//! 1. projects every vertex onto the field's zero set (Newton steps along the
//!    gradient, each clamped to half a cell so nothing jumps to another sheet);
//! 2. snaps vertices onto creases: where an edge's two field normals differ
//!    by more than [`FEATURE_DEG`], its nearer end moves to the point that best
//!    fits the tangent planes around it (a truncated-eigen QEF solve, clamped
//!    near the vertex), so a crease runs as a straight polyline of mesh edges;
//!    slivers this leaves on the crease line are removed by edge flips;
//! 3. sets normals from the field gradient, split per smooth side at
//!    feature vertices.
//!
//! The field is negative inside (the gradient points out).

use crate::Part;
use bevy::math::Vec3;
use rayon::prelude::*;
use std::collections::HashMap;

/// A triangle whose vertex normals span more than this is on a crease.
const FEATURE_DEG: f32 = 35.0;

/// Corners of a crease vertex whose normals differ by more than this get
/// separate vertices (one per side of the crease).
const GROUP_DEG: f32 = 8.0;

/// Below [`FEATURE_DEG`], an edge whose normals differ by more than this is
/// tested for a gradient jump (a shallow crease).
const SHALLOW_DEG: f32 = 1.5;

/// The farthest a vertex moves onto a crease, in cells.
const SNAP_RES: f32 = 1.5;

/// A field: signed distance (or anything with the same zero set and sign),
/// negative inside.
pub type Field<'a> = dyn Fn(Vec3) -> f32 + Sync + 'a;

/// Where a crease can be: true near a sharp-edged shape or a hard join.
/// Outside it the surface is smooth, however tightly it bends.
pub type Zone<'a> = dyn Fn(Vec3) -> bool + Sync + 'a;

/// How to sample a field.
#[derive(Clone, Copy, Debug)]
pub struct Sampling {
    /// The meshing cell size (meters): bounds every projection step and
    /// the feature-point clamp.
    pub res: f32,
    /// The gradient's finite-difference step: about a quarter cell for an
    /// analytic field, a full cell for a trilinear grid (its gradient is
    /// kinked inside cells).
    pub step: f32,
    /// Whether the field can have creases (hard joins, sharp-edged
    /// shapes). Without, the pass only projects and sets smooth normals: a
    /// tight smooth bend is never mistaken for a crease and split.
    pub creases: bool,
    /// Whether the field's gradient is piecewise flat (sampled from a
    /// polyline outline or a grid): then normals are averaged over half a
    /// cell so facet joins shade smooth. An analytic field doesn't need it.
    pub wide: bool,
}

impl Sampling {
    /// The same sampling for a field with a piecewise-flat gradient.
    pub fn wide(self) -> Self {
        Self { wide: true, ..self }
    }

    /// The same sampling for a field known to be smooth everywhere.
    pub fn smooth(self) -> Self {
        Self {
            creases: false,
            ..self
        }
    }

    /// An analytic field meshed at `res`.
    pub fn analytic(res: f32) -> Self {
        Self {
            res,
            step: res * 0.25,
            creases: true,
            wide: false,
        }
    }

    /// A trilinear grid of cell `cell` meshed at `res`.
    pub fn grid(res: f32, cell: f32) -> Self {
        Self {
            res,
            step: cell.max(res * 0.25),
            creases: true,
            wide: true,
        }
    }
}

/// Tetrahedral 4-tap gradient (unnormalised, field units per meter).
pub fn gradient(f: &Field, p: Vec3, h: f32) -> Vec3 {
    const K: [Vec3; 4] = [
        Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(-1.0, -1.0, 1.0),
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::new(1.0, 1.0, 1.0),
    ];
    let mut g = Vec3::ZERO;
    for k in K {
        g += k * f(p + k * h);
    }
    g / (4.0 * h)
}

/// Central-difference gradient (6 taps): smoother over a trilinear grid.
fn gradient_central(f: &Field, p: Vec3, h: f32) -> Vec3 {
    let dx = Vec3::X * h;
    let dy = Vec3::Y * h;
    let dz = Vec3::Z * h;
    Vec3::new(
        f(p + dx) - f(p - dx),
        f(p + dy) - f(p - dy),
        f(p + dz) - f(p - dz),
    ) / (2.0 * h)
}

fn grad(f: &Field, p: Vec3, s: Sampling) -> Vec3 {
    if s.step >= s.res * 0.99 {
        gradient_central(f, p, s.step)
    } else {
        gradient(f, p, s.step)
    }
}

/// A smaller gradient step for an analytic field (normals right up to a
/// crease); a grid keeps its full-cell step.
fn fine(s: Sampling) -> Sampling {
    if s.step >= s.res * 0.99 {
        s
    } else {
        Sampling {
            step: s.res * 0.02,
            ..s
        }
    }
}

/// Newton-projects `p` onto the zero set: up to `steps` steps, each at most
/// half a cell.
fn project(f: &Field, mut p: Vec3, s: Sampling, steps: u32) -> (Vec3, Vec3) {
    let start = p;
    let mut g = Vec3::ZERO;
    for _ in 0..steps {
        let d = f(p);
        if d.abs() < s.res * 1e-4 && g != Vec3::ZERO {
            break;
        }
        g = grad(f, p, s);
        let g2 = g.length_squared();
        if g2 < 1e-8 {
            break;
        }
        let mut step = g * (d / g2);
        let l = step.length();
        if l > 0.5 * s.res {
            step *= 0.5 * s.res / l;
        }
        p -= step;
        // Converged: the last gradient is the normal there.
        if l < s.res * 1e-3 {
            break;
        }
    }
    // Never more than a cell from where the mesher put it.
    let moved = p - start;
    let p = if moved.length() > s.res {
        start + moved.normalize() * s.res
    } else {
        p
    };
    (p, g)
}

/// Welds `part`'s vertices by position (local space).
pub fn weld(part: &Part) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let (lo, hi) = part.verts.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), p| (a.min(Vec3::from(*p)), b.max(Vec3::from(*p))),
    );
    let inv = 1.0 / ((hi - lo).max_element() * 1e-6).clamp(1e-7, 1e-5);
    let mut map: HashMap<[i64; 3], u32> = HashMap::new();
    let mut v = Vec::new();
    let remap: Vec<u32> = part
        .verts
        .iter()
        .map(|p| {
            let p = Vec3::from(*p);
            let k = [
                (p.x * inv).round() as i64,
                (p.y * inv).round() as i64,
                (p.z * inv).round() as i64,
            ];
            *map.entry(k).or_insert_with(|| {
                v.push(p);
                (v.len() - 1) as u32
            })
        })
        .collect();
    let idx: Vec<u32> = if part.indices.is_empty() {
        (0..(part.verts.len() - part.verts.len() % 3) as u32).collect()
    } else {
        part.indices.clone()
    };
    let t = idx
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| t.map(|i| remap[i as usize]))
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    (v, t)
}

/// Projects every vertex onto the field (in place).
/// Returns the field normal at each projected vertex.
pub fn project_all(v: &mut [Vec3], f: &Field, s: Sampling) -> Vec<Vec3> {
    let sf = fine(s);
    v.par_iter_mut()
        .map(|p| {
            let (q, _) = project(f, *p, s, 3);
            *p = q;
            grad(f, q, sf).normalize_or_zero()
        })
        .collect()
}

/// Symmetric 3x3 eigen decomposition (Jacobi): eigenvalues and the
/// eigenvectors as columns.
fn eigen_sym(mut a: [[f32; 3]; 3]) -> ([f32; 3], [[f32; 3]; 3]) {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..12 {
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-12 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for row in &mut a {
                let (akp, akq) = (row[p], row[q]);
                row[p] = c * akp - s * akq;
                row[q] = s * akp + c * akq;
            }
            for k in 0..3 {
                let (apk, aqk) = (a[p][k], a[q][k]);
                a[p][k] = c * apk - s * aqk;
                a[q][k] = s * apk + c * aqk;
            }
            for row in &mut v {
                let (vp, vq) = (row[p], row[q]);
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}

/// The point nearest (least squares) to the planes through `p[i]` with
/// normals `n[i]`, with small eigenvalues truncated (so an edge, rank 2, takes
/// the point on the edge nearest `c`), clamped to within `r` of `c`.
fn qef(p: &[Vec3], n: &[Vec3], c: Vec3, r: f32, cut: f32) -> Vec3 {
    let mut a = [[0.0f32; 3]; 3];
    let mut b = Vec3::ZERO;
    for (pi, ni) in p.iter().zip(n) {
        let na = ni.to_array();
        let d = ni.dot(*pi - c);
        for i in 0..3 {
            for j in 0..3 {
                a[i][j] += na[i] * na[j];
            }
        }
        b += *ni * d;
    }
    let (w, v) = eigen_sym(a);
    let max = w.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let mut x = Vec3::ZERO;
    for k in 0..3 {
        if w[k].abs() < cut * max || max < 1e-12 {
            continue;
        }
        let e = Vec3::new(v[0][k], v[1][k], v[2][k]);
        x += e * (e.dot(b) / w[k]);
    }
    if x.length() > r {
        x = x.normalize() * r;
    }
    c + x
}

fn face_normal(v: &[Vec3], t: [u32; 3]) -> Vec3 {
    let [a, b, c] = t.map(|i| v[i as usize]);
    (b - a).cross(c - a)
}

/// Snaps vertices onto creases. An edge whose ends' field normals differ by
/// more than [`FEATURE_DEG`] crosses a crease; its end nearer the other
/// end's tangent plane moves to the point that best fits its own tangent
/// plane and those of its crossing neighbours (a truncated QEF solve, so an
/// edge, rank 2, takes the nearest point on the crease line and a corner,
/// rank 3, the corner), clamped to [`SNAP_RES`] cells. Every triangle that
/// crossed the crease then has its off-crease corners on one side, so it lies
/// in that side's surface and the crease is a polyline of mesh edges
/// through snapped vertices. Topology never changes (the mesh stays a closed
/// manifold); a snap that would flip an incident triangle is halved, then
/// dropped. Returns the snapped vertices.
fn reconstruct_features(
    v: &mut [Vec3],
    t: &[[u32; 3]],
    nrm: &[Vec3],
    f: &Field,
    s: Sampling,
    zone: Option<&Zone>,
) -> (Vec<bool>, Vec<bool>) {
    let cos_feature = FEATURE_DEG.to_radians().cos();
    let n = v.len();
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut faces: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (fi, tri) in t.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            nb[a as usize].push(b);
            nb[b as usize].push(a);
            faces[a as usize].push(fi as u32);
        }
    }
    for l in &mut nb {
        l.sort_unstable();
        l.dedup();
    }
    // An edge crosses a crease when its ends' normals differ a lot, or
    // (a shallow crease) by a few degrees that the field doesn't explain
    // smoothly: on a smooth surface the gradient at the edge's middle is
    // about the mean of the ends', on a hard min / max join it is one
    // side's (the gradient jumps there).
    let cos_shallow = SHALLOW_DEG.to_radians().cos();
    let cos_steep = 70f32.to_radians().cos();
    let sf = fine(s);
    // A smooth surface turns about as much across every edge round a
    // vertex; a crease edge turns far more than the vertex's quieter edges
    // (its 30th percentile), so only those take the midpoint test.
    let quiet: Vec<f32> = nb
        .par_iter()
        .enumerate()
        .map(|(a, list)| {
            let mut ang: Vec<f32> = list
                .iter()
                .map(|&b| nrm[a].dot(nrm[b as usize]).clamp(-1.0, 1.0).acos())
                .collect();
            if ang.is_empty() {
                return 0.0;
            }
            let k = (ang.len() * 3 / 10).min(ang.len() - 1);
            *ang.select_nth_unstable_by(k, f32::total_cmp).1
        })
        .collect();
    // Per vertex, per (sorted) neighbour above it: does that edge cross?
    let crossing: Vec<Vec<bool>> = nb
        .par_iter()
        .enumerate()
        .map(|(a, list)| {
            list.iter()
                .map(|&b| {
                    if (b as usize) <= a {
                        return false;
                    }
                    if zone.is_some_and(|z| !z((v[a] + v[b as usize]) * 0.5)) {
                        return false;
                    }
                    let (na, nb_) = (nrm[a], nrm[b as usize]);
                    let c = na.dot(nb_);
                    // A turn this steep is a crease outright; a less steep
                    // one may be a tight smooth bend (a fillet a cell or two
                    // wide): the midpoint test below tells.
                    if c < cos_steep {
                        return true;
                    }
                    if c >= cos_shallow || na == Vec3::ZERO || nb_ == Vec3::ZERO {
                        return false;
                    }
                    let angle = c.clamp(-1.0, 1.0).acos();
                    if c >= cos_feature && angle < 2.5 * quiet[a].max(quiet[b as usize]) {
                        return false;
                    }
                    let mid = (na + nb_).normalize_or_zero();
                    let m = (v[a] + v[b as usize]) * 0.5;
                    let g = grad(f, m, sf).normalize_or_zero();
                    // Fraction of the half-angle the middle is off the mean.
                    let half = (0.5 * c.clamp(-1.0, 1.0).acos()).max(1e-6);
                    let off = g.dot(mid).clamp(-1.0, 1.0).acos();
                    off > 0.6 * half
                })
                .collect()
        })
        .collect();
    let crosses = |a: usize, b: usize| {
        let (x, y) = (a.min(b), a.max(b) as u32);
        nb[x].binary_search(&y).is_ok_and(|i| crossing[x][i])
    };
    // Distance of `a` from `b`'s tangent plane.
    let off = |a: usize, b: usize| nrm[b].dot(v[a] - v[b]).abs();
    // Cover every crossing edge with one snapped end, nearest ends first,
    // so as few vertices as possible move and rarely both ends of an edge.
    let mut cross: Vec<(f32, u32, u32)> = Vec::new();
    for (a, list) in nb.iter().enumerate() {
        for &b in list {
            let b = b as usize;
            if a < b && crosses(a, b) {
                let (da, db) = (off(a, b), off(b, a));
                let (near, far) = if da <= db { (a, b) } else { (b, a) };
                cross.push((da.min(db), near as u32, far as u32));
            }
        }
    }
    cross.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut chosen = vec![false; n];
    for &(_, a, b) in &cross {
        if !chosen[a as usize] && !chosen[b as usize] {
            chosen[a as usize] = true;
        }
    }
    let targets: Vec<Option<Vec3>> = (0..n)
        .into_par_iter()
        .map(|a| {
            if !chosen[a] || nrm[a] == Vec3::ZERO {
                return None;
            }
            let steep = nb[a]
                .iter()
                .any(|&b| crosses(a, b as usize) && nrm[a].dot(nrm[b as usize]) < cos_feature);
            if steep {
                // Three sides in the ring (a face thinner than a couple of
                // cells between two creases, or a corner): snap to the
                // nearest of the three crease lines or the corner that is
                // really on the surface (the field is zero there), not to
                // the corner of the extended planes.
                let (ps, ns, gs, k) = side_groups(a, &nb[a], v, nrm);
                if k == 3 {
                    let pick = |sel: &[usize]| {
                        let (mut p2, mut n2) = (Vec::new(), Vec::new());
                        for i in 0..ps.len() {
                            if sel.contains(&gs[i]) {
                                p2.push(ps[i]);
                                n2.push(ns[i]);
                            }
                        }
                        qef(&p2, &n2, v[a], 4.0 * s.res, 0.02)
                    };
                    let tol = 0.05 * s.res;
                    let best = [&[0, 1][..], &[0, 2], &[1, 2], &[0, 1, 2]]
                        .iter()
                        .map(|sel| pick(sel))
                        .filter(|x| x.distance(v[a]) < SNAP_RES * s.res && f(*x).abs() < tol)
                        .min_by(|x, y| x.distance(v[a]).total_cmp(&y.distance(v[a])));
                    if best.is_some() {
                        return best;
                    }
                }
                // A clear crease: every crossing neighbour's plane, with
                // weak directions truncated.
                let mut ps = vec![v[a]];
                let mut ns = vec![nrm[a]];
                for &b in &nb[a] {
                    let b = b as usize;
                    if crosses(a, b) {
                        ps.push(v[b]);
                        ns.push(nrm[b]);
                    }
                }
                return Some(qef(&ps, &ns, v[a], SNAP_RES * s.res, 0.02));
            }
            // A shallow crease: one plane set per side, so the nearly
            // parallel planes are solved without mixed normals between.
            let (ps, ns) = side_planes(a, &nb[a], v, nrm);
            if ns.len() < 2 {
                return None;
            }
            Some(qef(&ps, &ns, v[a], 2.0 * SNAP_RES * s.res, 1e-3))
        })
        .collect();
    let mut sharp = vec![false; n];
    let tiny = (0.01 * s.res).powi(2);
    let close = (0.05 * s.res).powi(2);
    let orig: Vec<Vec3> = t.iter().map(|tri| face_normal(v, *tri)).collect();
    // A snap blocked by a neighbour's position may fit once that neighbour
    // has moved too: retry the blocked ones a few rounds.
    let mut todo: Vec<(usize, Vec3)> = targets
        .into_iter()
        .enumerate()
        .filter_map(|(a, x)| x.map(|x| (a, x)))
        .collect();
    let mut partial: Vec<(usize, Vec3, Vec3)> = Vec::new();
    for _round in 0..3 {
        let mut blocked = Vec::new();
        for &(a, x) in &todo {
            let from = v[a];
            for frac in [1.0, 0.5] {
                v[a] = from + (x - from) * frac;
                let ok = faces[a].iter().all(|&fi| {
                    let tri = t[fi as usize];
                    let fnn = face_normal(v, tri);
                    // Facing like the triangle did and like one of its corners'
                    // sides (a triangle across a knife edge may end up in
                    // either face).
                    let faces_out = tri.iter().any(|&i| fnn.dot(nrm[i as usize]) > 0.0);
                    fnn.length() < tiny || (fnn.dot(orig[fi as usize]) > 0.0 && faces_out)
                }) && nb[a].iter().all(|&b| {
                    // Never onto another vertex (two snapping to one corner).
                    std::iter::once(b)
                        .chain(nb[b as usize].iter().copied())
                        .all(|c| c as usize == a || v[c as usize].distance_squared(v[a]) > close)
                });
                if ok {
                    sharp[a] = true;
                    if frac < 1.0 {
                        partial.push((a, x, from));
                    }
                    break;
                }
                v[a] = from;
            }
            if !sharp[a] {
                blocked.push((a, x));
            }
        }
        // Halved snaps try the rest of the way too.
        for (a, x, _) in partial.drain(..) {
            blocked.push((a, x));
            sharp[a] = false;
        }
        if blocked.len() == todo.len() || blocked.is_empty() {
            break;
        }
        todo = blocked;
    }
    // Vertices touching a crease edge (snapped or not): their corners
    // take the side of their own face.
    let mut near = sharp.clone();
    for (a, list) in nb.iter().enumerate() {
        if list.iter().any(|&b| crosses(a, b as usize)) {
            near[a] = true;
        }
    }
    (sharp, near)
}

/// The tangent planes of `a`'s one ring that belong to a side of the
/// crease: the two most different normals are two sides, a normal well out of
/// their plane a third (a corner); planes within 2 degrees of a side are
/// kept, and normals in between (a vertex so near the crease that its
/// gradient mixes both sides) are dropped, since they would pull the solve
/// off the crease line.
fn side_planes(a: usize, ring: &[u32], v: &[Vec3], nrm: &[Vec3]) -> (Vec<Vec3>, Vec<Vec3>) {
    let (ps, ns, _, _) = side_groups(a, ring, v, nrm);
    (ps, ns)
}

/// [`side_planes`] with each plane's side (an index into the 2 or 3 side
/// normals) and the number of sides.
fn side_groups(
    a: usize,
    ring: &[u32],
    v: &[Vec3],
    nrm: &[Vec3],
) -> (Vec<Vec3>, Vec<Vec3>, Vec<usize>, usize) {
    let ids: Vec<usize> = std::iter::once(a)
        .chain(ring.iter().map(|&b| b as usize))
        .filter(|&i| nrm[i] != Vec3::ZERO)
        .collect();
    let mut best = (2.0f32, 0, 0);
    for (x, &i) in ids.iter().enumerate() {
        for &j in &ids[x + 1..] {
            let d = nrm[i].dot(nrm[j]);
            if d < best.0 {
                best = (d, i, j);
            }
        }
    }
    let mut reps = vec![nrm[best.1], nrm[best.2]];
    let axis = reps[0].cross(reps[1]).normalize_or_zero();
    let sin_out = 25f32.to_radians().sin();
    if let Some(&k) = ids
        .iter()
        .filter(|&&k| nrm[k].dot(axis).abs() > sin_out)
        .max_by(|&&x, &&y| nrm[x].dot(axis).abs().total_cmp(&nrm[y].dot(axis).abs()))
    {
        reps.push(nrm[k]);
    }
    let cos_keep = 2f32.to_radians().cos();
    let (mut ps, mut ns, mut gs) = (Vec::new(), Vec::new(), Vec::new());
    for &i in &ids {
        if let Some(g) = reps.iter().position(|r| r.dot(nrm[i]) > cos_keep) {
            ps.push(v[i]);
            ns.push(nrm[i]);
            gs.push(g);
        }
    }
    let k = reps.len();
    (ps, ns, gs, k)
}

/// Removes slivers (a triangle whose corners are nearly collinear, left
/// where snapping put three vertices on one crease line) by flipping their
/// longest edge, as long as the flip makes no duplicate edge and both new
/// triangles face the way the neighbour did. Topology stays a closed
/// manifold.
fn flip_slivers(v: &[Vec3], t: &mut [[u32; 3]]) {
    let mut edges: HashMap<(u32, u32), usize> = HashMap::with_capacity(t.len() * 3);
    for (i, tri) in t.iter().enumerate() {
        for k in 0..3 {
            edges.insert((tri[k], tri[(k + 1) % 3]), i);
        }
    }
    let sliver = |tri: [u32; 3]| -> Option<(u32, u32, u32)> {
        let p = tri.map(|i| v[i as usize]);
        let mut best = (0.0f32, 0usize);
        for k in 0..3 {
            let l = p[k].distance_squared(p[(k + 1) % 3]);
            if l > best.0 {
                best = (l, k);
            }
        }
        let area2 = (p[1] - p[0]).cross(p[2] - p[0]).length();
        (best.0 > 0.0 && area2 < 0.02 * best.0).then(|| {
            let k = best.1;
            (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3])
        })
    };
    for _ in 0..4 {
        let mut any = false;
        for t1 in 0..t.len() {
            let Some((a, b, c)) = sliver(t[t1]) else {
                continue;
            };
            let Some(&t2) = edges.get(&(b, a)) else {
                continue;
            };
            let Some(d) = t[t2].iter().copied().find(|&x| x != a && x != b) else {
                continue;
            };
            if d == c || edges.contains_key(&(c, d)) || edges.contains_key(&(d, c)) {
                continue;
            }
            // [a, b, c] and [b, a, d] become [a, d, c] and [d, b, c].
            let (n1, n2) = ([a, d, c], [d, b, c]);
            let f2 = face_normal(v, t[t2]);
            let good = |n: [u32; 3]| {
                let f = face_normal(v, n);
                f.length_squared() > 0.0 && f.dot(f2) > 0.0
            };
            if !good(n1) || !good(n2) {
                continue;
            }
            for tri in [t[t1], t[t2]] {
                for k in 0..3 {
                    edges.remove(&(tri[k], tri[(k + 1) % 3]));
                }
            }
            t[t1] = n1;
            t[t2] = n2;
            for (i, tri) in [(t1, n1), (t2, n2)] {
                for k in 0..3 {
                    edges.insert((tri[k], tri[(k + 1) % 3]), i);
                }
            }
            any = true;
        }
        if !any {
            break;
        }
    }
}

/// Field normals averaged over about a voxel: the gradient over a
/// central-difference stencil half a cell wide. A field built from a polyline or a
/// bilinear grid has a piecewise-constant gradient, and this shades its
/// shallow facet joins smooth; the stencil is symmetric, so a smooth field (a
/// sphere) keeps its normals. Crease vertices, and wide normals more than 25
/// degrees off the vertex's own (the stencil reached across a crease), keep
/// the fine one.
fn wide_normals(v: &[Vec3], nrm: &[Vec3], sharp: &[bool], f: &Field, s: Sampling) -> Vec<Vec3> {
    let cos = 5f32.to_radians().cos();
    let h = (0.5 * s.res).max(s.step);
    (0..v.len())
        .into_par_iter()
        .map(|i| {
            let ni = nrm[i];
            if sharp[i] || ni == Vec3::ZERO {
                return ni;
            }
            let g = gradient_central(f, v[i], h).normalize_or_zero();
            if g.dot(ni) > cos { g } else { ni }
        })
        .collect()
}

/// Field normals that the mesh can't follow. Where the surface bends
/// tighter than about a cell (a sharp inside bend of a chain, a narrow
/// valley between tubes) the gradient turns a lot within the cell, the
/// triangles chord across the bend, and a vertex's field normal can be far
/// off its faces: shaded, a speck. A vertex whose normal is more than 8
/// degrees off its faces' area-weighted normal is checked against the
/// gradient half a cell wide: if they differ by over 6 degrees (the field
/// bends within the cell), or the face normal is over 40 degrees off, it
/// takes the face normal (one normal, no split). Crease vertices are left
/// alone.
fn fit_faces(
    v: &[Vec3],
    t: &[[u32; 3]],
    nrm: Vec<Vec3>,
    sharp: &[bool],
    f: &Field,
    s: Sampling,
) -> Vec<Vec3> {
    let mut acc = vec![Vec3::ZERO; v.len()];
    for tri in t {
        let n = face_normal(v, *tri);
        for &i in tri {
            acc[i as usize] += n;
        }
    }
    let (cos_check, cos_far, cos_bend) = (
        8f32.to_radians().cos(),
        40f32.to_radians().cos(),
        6f32.to_radians().cos(),
    );
    let h = 0.5 * s.res;
    nrm.into_par_iter()
        .enumerate()
        .map(|(i, n)| {
            let a = acc[i].normalize_or_zero();
            if sharp[i] || a == Vec3::ZERO {
                return n;
            }
            let d = n.dot(a);
            if d >= cos_check {
                return n;
            }
            if d < cos_far {
                return a;
            }
            let w = gradient_central(f, v[i], h).normalize_or_zero();
            if w.dot(n) < cos_bend { a } else { n }
        })
        .collect()
}

/// The whole pass on a welded mesh: project, reconstruct creases, set field
/// normals and box uvs. `material` and `name` go on the result.
pub fn finish_mesh(
    name: &str,
    material: crate::Material,
    v: Vec<Vec3>,
    t: Vec<[u32; 3]>,
    f: &Field,
    s: Sampling,
) -> Part {
    finish_mesh_in(name, material, v, t, f, s, None)
}

/// [`finish_mesh`] with creases looked for only inside `zone`.
pub fn finish_mesh_in(
    name: &str,
    material: crate::Material,
    mut v: Vec<Vec3>,
    mut t: Vec<[u32; 3]>,
    f: &Field,
    s: Sampling,
    zone: Option<&Zone>,
) -> Part {
    let nrm = project_all(&mut v, f, s);
    let (sharp, near) = if s.creases {
        reconstruct_features(&mut v, &t, &nrm, f, s, zone)
    } else {
        (vec![false; v.len()], vec![false; v.len()])
    };
    flip_slivers(&v, &mut t);
    let nrm = if s.wide {
        wide_normals(&v, &nrm, &sharp, f, s)
    } else {
        nrm
    };
    let nrm = fit_faces(&v, &t, nrm, &sharp, f, s);
    // Corner normals: the field gradient at the vertex, or, on a crease,
    // the gradient a little inside the corner's own face (its smooth side);
    // corners of one vertex that agree share a vertex.
    // A vertex normal this far off one of its faces is mixed (the vertex
    // sits on a crease the edges didn't flag): its corners take sides too.
    let cos_mixed = 12f32.to_radians().cos();
    // Any vertex this far off a face is on the face's other side.
    let cos_side = FEATURE_DEG.to_radians().cos();
    // A side gradient this far off its face reached across the crease.
    let cos_face = 10f32.to_radians().cos();
    let corners: Vec<[Vec3; 3]> = t
        .par_iter()
        .map(|tri| {
            let ps = tri.map(|i| v[i as usize]);
            let c = (ps[0] + ps[1] + ps[2]) / 3.0;
            let fallback = face_normal(&v, *tri).normalize_or_zero();
            let mut out = [Vec3::ZERO; 3];
            // The face's side normal, evaluated once for its corners.
            let mut side: Option<Vec3> = None;
            for k in 0..3 {
                let i = tri[k] as usize;
                // The face's centre is its most interior point; a short
                // step keeps the stencil on this side of the crease.
                let mut side_n = || {
                    *side.get_or_insert_with(|| {
                        gradient(f, c, s.step.min(s.res * 0.01)).normalize_or_zero()
                    })
                };
                let ni = nrm[i];
                let off_face = ni.dot(fallback);
                // A snapped crease vertex, or one whose normal doesn't fit
                // this face at all, takes this face's side. One near a crease
                // or a little off its face takes it only where the field
                // really jumps between the vertex and the face (a tight
                // smooth bend turns gradually: the gradient half way is the
                // mean of the two).
                let take_side = if !s.creases {
                    // A smooth field: every vertex has one normal.
                    ni == Vec3::ZERO
                } else if sharp[i] || ni == Vec3::ZERO {
                    true
                } else if zone.is_some_and(|z| !z(ps[k])) {
                    false
                } else if off_face < cos_side {
                    true
                } else if near[i] || off_face < cos_mixed {
                    let g = side_n();
                    let mean = (ni + g).normalize_or_zero();
                    let half = 0.5 * ni.dot(g).clamp(-1.0, 1.0).acos();
                    let m = (ps[k] + c) * 0.5;
                    let gm = gradient(f, m, s.step.min(s.res * 0.01)).normalize_or_zero();
                    gm.dot(mean).clamp(-1.0, 1.0).acos() > 0.6 * half
                } else {
                    false
                };
                out[k] = if take_side {
                    let g = side_n();
                    if g.dot(fallback) > cos_face {
                        g
                    } else {
                        fallback
                    }
                } else {
                    ni
                };
            }
            out
        })
        .collect();
    let cos_group = GROUP_DEG.to_radians().cos();
    // Per original vertex: list of (accumulated normal, output index).
    let mut groups: Vec<Vec<(Vec3, u32)>> = vec![Vec::new(); v.len()];
    let mut out = Part::new(name);
    out.material = material;
    let mut corner_idx = Vec::with_capacity(t.len() * 3);
    for (tri, ns) in t.iter().zip(&corners) {
        for k in 0..3 {
            let i = tri[k] as usize;
            let n = ns[k];
            let g = &mut groups[i];
            let hit = g
                .iter_mut()
                .find(|(acc, _)| acc.normalize_or_zero().dot(n) > cos_group);
            let id = match hit {
                Some((acc, id)) => {
                    *acc += n;
                    *id
                }
                None => {
                    let id = out.verts.len() as u32;
                    out.verts.push(v[i].to_array());
                    out.normals.push([0.0; 3]);
                    g.push((n, id));
                    id
                }
            };
            corner_idx.push(id);
        }
    }
    for g in &groups {
        for (acc, id) in g {
            let n = acc.normalize_or_zero();
            out.normals[*id as usize] = if n == Vec3::ZERO {
                [0.0, 1.0, 0.0]
            } else {
                n.to_array()
            };
        }
    }
    out.indices = corner_idx;
    let _ = crate::uv::ensure_uvs(&mut out, Some("box"));
    out
}

/// [`finish_mesh`] on a meshed part (welded first; the part's transform is
/// assumed identity, as the mesher's output is).
pub fn finish(part: &Part, f: &Field, s: Sampling) -> Part {
    finish_in(part, f, s, None)
}

/// [`finish`] with creases looked for only inside `zone`.
pub fn finish_in(part: &Part, f: &Field, s: Sampling, zone: Option<&Zone>) -> Part {
    let (v, t) = weld(part);
    let mut out = finish_mesh_in(&part.name, part.material.clone(), v, t, f, s, zone);
    out.visible = part.visible;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qef_finds_an_edge_and_a_corner() {
        let p = [
            Vec3::new(0.0, 1.0, 0.2),
            Vec3::new(1.0, 0.0, 0.3),
            Vec3::new(1.0, 0.5, 0.1),
        ];
        let n = [Vec3::Y, Vec3::X, Vec3::X];
        let x = qef(&p, &n, Vec3::new(0.8, 0.8, 0.2), 1.0, 0.1);
        assert!((x - Vec3::new(1.0, 1.0, 0.2)).length() < 1e-4, "{x}");
        let n = [Vec3::Y, Vec3::X, Vec3::Z];
        let p = [
            Vec3::new(0.3, 1.0, 0.2),
            Vec3::new(1.0, 0.1, 0.3),
            Vec3::new(0.2, 0.5, 1.0),
        ];
        let x = qef(&p, &n, Vec3::new(0.8, 0.8, 0.8), 1.0, 0.1);
        assert!((x - Vec3::ONE).length() < 1e-4, "{x}");
    }
}
