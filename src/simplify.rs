//! Quadric-error-metric mesh simplification (Garland–Heckbert), used for LODs.
//!
//! Half-edge collapses `u -> v` (u moves onto the existing vertex v), so no
//! attribute is ever interpolated and UVs/normals stay exactly as authored.
//! Positions are welded for topology; a position whose split vertices differ
//! in UV or normal (a seam or hard edge) is locked, as is every position on
//! a border or non-manifold edge, so seams never tear. Collapses that flip a
//! triangle, break the link condition or leave a needle are rejected.
//! Deterministic: no hashing order or threads are involved.

use crate::Part;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

type Quadric = [f64; 10];

fn quadric_from_plane(n: [f64; 3], d: f64, w: f64) -> Quadric {
    let [a, b, c] = n;
    [
        w * a * a,
        w * a * b,
        w * a * c,
        w * a * d,
        w * b * b,
        w * b * c,
        w * b * d,
        w * c * c,
        w * c * d,
        w * d * d,
    ]
}

fn quadric_add(q: &mut Quadric, o: &Quadric) {
    for (a, b) in q.iter_mut().zip(o) {
        *a += b;
    }
}

fn quadric_eval(q: &Quadric, p: [f64; 3]) -> f64 {
    let [x, y, z] = p;
    q[0] * x * x
        + 2.0 * q[1] * x * y
        + 2.0 * q[2] * x * z
        + 2.0 * q[3] * x
        + q[4] * y * y
        + 2.0 * q[5] * y * z
        + 2.0 * q[6] * y
        + q[7] * z * z
        + 2.0 * q[8] * z
        + q[9]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// A collapse candidate, ordered cheapest first (BinaryHeap is a max-heap).
struct Cand {
    cost: f64,
    u: u32,
    v: u32,
    ver_u: u32,
    ver_v: u32,
}
impl PartialEq for Cand {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Cand {}
impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Cand {
    fn cmp(&self, o: &Self) -> Ordering {
        o.cost
            .total_cmp(&self.cost)
            .then_with(|| o.u.cmp(&self.u))
            .then_with(|| o.v.cmp(&self.v))
    }
}

/// Two split vertices carry the same attributes (so a position holding only
/// such vertices is no seam).
fn same_attrs(part: &Part, a: usize, b: usize) -> bool {
    let uv_eq = match (part.uvs.get(a), part.uvs.get(b)) {
        (Some(x), Some(y)) => (x[0] - y[0]).abs() < 1e-5 && (x[1] - y[1]).abs() < 1e-5,
        _ => true,
    };
    let n_eq = match (part.normals.get(a), part.normals.get(b)) {
        (Some(x), Some(y)) => (0..3).all(|i| (x[i] - y[i]).abs() < 1e-3),
        _ => true,
    };
    uv_eq && n_eq
}

struct State<'a> {
    part: &'a Part,
    /// wedge (original vertex) -> welded position id
    wpos: Vec<u32>,
    /// position id -> coordinates
    pos: Vec<[f64; 3]>,
    tris: Vec<[u32; 3]>,
    alive: Vec<bool>,
    /// position id -> triangles touching it (may hold dead ones)
    ptris: Vec<Vec<u32>>,
    quad: Vec<Quadric>,
    locked: Vec<bool>,
    ver: Vec<u32>,
}

impl State<'_> {
    fn tri_pos(&self, t: usize) -> [u32; 3] {
        let [a, b, c] = self.tris[t];
        [
            self.wpos[a as usize],
            self.wpos[b as usize],
            self.wpos[c as usize],
        ]
    }

    fn neighbors(&self, p: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for &t in &self.ptris[p as usize] {
            if !self.alive[t as usize] {
                continue;
            }
            for q in self.tri_pos(t as usize) {
                if q != p {
                    out.push(q);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn cost(&self, u: u32, v: u32) -> f64 {
        let pu = self.pos[u as usize];
        let pv = self.pos[v as usize];
        let e = sub(pu, pv);
        // A tiny length term breaks ties on flat regions toward short edges.
        quadric_eval(&self.quad[u as usize], pv).max(0.0) + 1e-9 * dot(e, e)
    }

    fn push_edges(&self, heap: &mut BinaryHeap<Cand>, p: u32) {
        for q in self.neighbors(p) {
            for (u, v) in [(p, q), (q, p)] {
                if !self.locked[u as usize] {
                    heap.push(Cand {
                        cost: self.cost(u, v),
                        u,
                        v,
                        ver_u: self.ver[u as usize],
                        ver_v: self.ver[v as usize],
                    });
                }
            }
        }
    }

    /// Try collapsing position u onto v; returns triangles removed.
    fn collapse(&mut self, u: u32, v: u32) -> usize {
        let (ui, vi) = (u as usize, v as usize);
        if self.locked[ui] {
            return 0;
        }
        let mut shared = Vec::new();
        let mut moving = Vec::new();
        for &t in &self.ptris[ui] {
            if !self.alive[t as usize] {
                continue;
            }
            if self.tri_pos(t as usize).contains(&v) {
                shared.push(t);
            } else {
                moving.push(t);
            }
        }
        if shared.is_empty() {
            return 0;
        }
        // Link condition: u and v share exactly the vertices opposite edge uv.
        let nu = self.neighbors(u);
        let nv = self.neighbors(v);
        let common = nu.iter().filter(|x| nv.binary_search(x).is_ok()).count();
        if common != shared.len() {
            return 0;
        }
        // The wedge of v that u's corners become: all shared triangles agree.
        let mut target: Option<u32> = None;
        for &t in &shared {
            for w in self.tris[t as usize] {
                if self.wpos[w as usize] == v {
                    match target {
                        None => target = Some(w),
                        Some(o) if o == w || same_attrs(self.part, o as usize, w as usize) => {}
                        Some(_) => return 0,
                    }
                }
            }
        }
        let Some(target) = target else { return 0 };
        // No flips, no needles.
        let pv = self.pos[vi];
        for &t in &moving {
            let p = self.tri_pos(t as usize).map(|q| self.pos[q as usize]);
            let n_old = cross(sub(p[1], p[0]), sub(p[2], p[0]));
            let np = self
                .tri_pos(t as usize)
                .map(|q| if q == u { pv } else { self.pos[q as usize] });
            let n_new = cross(sub(np[1], np[0]), sub(np[2], np[0]));
            let (lo, ln) = (len(n_old), len(n_new));
            if ln <= 1e-12 * (1.0 + lo) {
                return 0;
            }
            if lo > 1e-20 && dot(n_old, n_new) < 0.2 * lo * ln {
                return 0;
            }
        }
        for &t in &shared {
            self.alive[t as usize] = false;
        }
        for &t in &moving {
            for w in self.tris[t as usize].iter_mut() {
                if self.wpos[*w as usize] == u {
                    *w = target;
                }
            }
        }
        let qu = self.quad[ui];
        quadric_add(&mut self.quad[vi], &qu);
        let moved = std::mem::take(&mut self.ptris[ui]);
        self.ptris[vi].extend(moved.into_iter().filter(|&t| self.alive[t as usize]));
        let alive = &self.alive;
        self.ptris[vi].retain(|&t| alive[t as usize]);
        self.ptris[vi].sort_unstable();
        self.ptris[vi].dedup();
        self.ver[ui] += 1;
        self.ver[vi] += 1;
        shared.len()
    }
}

/// A copy of `part` with about `ratio` (clamped 0.01..1) of its triangles.
/// Locked seams, borders and hard edges can stop it short of the target.
pub fn simplify(part: &Part, ratio: f32) -> Part {
    let ratio = ratio.clamp(0.01, 1.0);
    let n_tris = part.indices.len() / 3;
    let target = ((n_tris as f32 * ratio).round() as usize).max(1);
    if target >= n_tris || part.verts.is_empty() {
        return part.clone();
    }

    // Weld positions by exact bits (with -0 folded onto 0).
    let mut ids: BTreeMap<[u32; 3], u32> = BTreeMap::new();
    let mut wpos = Vec::with_capacity(part.verts.len());
    let mut pos = Vec::new();
    for v in &part.verts {
        let key = v.map(|c| (c + 0.0).to_bits());
        let next = pos.len() as u32;
        let id = *ids.entry(key).or_insert(next);
        if id == next {
            pos.push(v.map(f64::from));
        }
        wpos.push(id);
    }
    let np = pos.len();

    let tris: Vec<[u32; 3]> = part
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .copied()
        .filter(|t| t.iter().all(|&i| (i as usize) < part.verts.len()))
        .collect();
    let mut ptris = vec![Vec::new(); np];
    let mut quad = vec![[0.0; 10]; np];
    let mut edges: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    for (ti, t) in tris.iter().enumerate() {
        let p = t.map(|w| wpos[w as usize]);
        let c = p.map(|q| pos[q as usize]);
        let n = cross(sub(c[1], c[0]), sub(c[2], c[0]));
        let l = len(n);
        if l > 0.0 {
            let un = n.map(|x| x / l);
            let q = quadric_from_plane(un, -dot(un, c[0]), l * 0.5);
            for &pi in &p {
                quadric_add(&mut quad[pi as usize], &q);
            }
        }
        for k in 0..3 {
            ptris[p[k] as usize].push(ti as u32);
            let (a, b) = (p[k], p[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    let mut locked = vec![false; np];
    for (&(a, b), &count) in &edges {
        if count != 2 {
            locked[a as usize] = true;
            locked[b as usize] = true;
        }
    }
    // Seams and hard edges: a position whose wedges differ.
    let mut first: Vec<Option<usize>> = vec![None; np];
    for (w, &p) in wpos.iter().enumerate() {
        match first[p as usize] {
            None => first[p as usize] = Some(w),
            Some(f) if !same_attrs(part, f, w) => locked[p as usize] = true,
            Some(_) => {}
        }
    }
    // Degenerate triangles (repeated positions) would confuse the topology.
    for t in &tris {
        let p = t.map(|w| wpos[w as usize]);
        if p[0] == p[1] || p[1] == p[2] || p[0] == p[2] {
            for q in p {
                locked[q as usize] = true;
            }
        }
    }

    let mut st = State {
        part,
        wpos,
        pos,
        alive: vec![true; tris.len()],
        tris,
        ptris,
        quad,
        locked,
        ver: vec![0; np],
    };
    let mut live = st.tris.len();

    // Passes: a candidate rejected now may become valid after its
    // neighbourhood changes, so rebuild the queue until nothing moves.
    loop {
        let before = live;
        let mut heap = BinaryHeap::new();
        for p in 0..np as u32 {
            if !st.locked[p as usize] {
                for q in st.neighbors(p) {
                    heap.push(Cand {
                        cost: st.cost(p, q),
                        u: p,
                        v: q,
                        ver_u: st.ver[p as usize],
                        ver_v: st.ver[q as usize],
                    });
                }
            }
        }
        while live > target {
            let Some(c) = heap.pop() else { break };
            if st.ver[c.u as usize] != c.ver_u || st.ver[c.v as usize] != c.ver_v {
                continue;
            }
            let removed = st.collapse(c.u, c.v);
            if removed > 0 {
                live -= removed;
                st.push_edges(&mut heap, c.v);
            }
        }
        if live <= target || live == before {
            break;
        }
    }

    // Compact: keep used vertices in first-use order.
    let mut remap = vec![u32::MAX; part.verts.len()];
    let mut out = part.clone();
    out.verts.clear();
    out.normals.clear();
    out.uvs.clear();
    out.indices.clear();
    for (t, _) in st.tris.iter().zip(&st.alive).filter(|(_, a)| **a) {
        for &w in t {
            let wi = w as usize;
            if remap[wi] == u32::MAX {
                remap[wi] = out.verts.len() as u32;
                out.verts.push(part.verts[wi]);
                if let Some(n) = part.normals.get(wi) {
                    out.normals.push(*n);
                }
                if let Some(uv) = part.uvs.get(wi) {
                    out.uvs.push(*uv);
                }
            }
            out.indices.push(remap[wi]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri_normal(p: &Part, t: &[u32; 3]) -> [f64; 3] {
        let c = t.map(|i| p.verts[i as usize].map(f64::from));
        cross(sub(c[1], c[0]), sub(c[2], c[0]))
    }

    /// A UV sphere with a duplicated seam column and per-sector pole vertices.
    fn uv_sphere(sectors: u32, stacks: u32) -> Part {
        let mut p = Part::new("sphere");
        for j in 0..=stacks {
            let phi = std::f32::consts::PI * j as f32 / stacks as f32;
            for i in 0..=sectors {
                let i_wrap = if i == sectors { 0 } else { i };
                let thw = std::f32::consts::TAU * i_wrap as f32 / sectors as f32;
                let (x, y, z) = (phi.sin() * thw.cos(), phi.cos(), phi.sin() * thw.sin());
                // Exact positions: seam column reuses i = 0's coordinates.
                let v = if j == 0 {
                    [0.0, 1.0, 0.0]
                } else if j == stacks {
                    [0.0, -1.0, 0.0]
                } else {
                    [x, y, z]
                };
                p.verts.push(v);
                p.normals.push(v);
                p.uvs
                    .push([i as f32 / sectors as f32, j as f32 / stacks as f32]);
            }
        }
        let row = sectors + 1;
        for j in 0..stacks {
            for i in 0..sectors {
                let a = j * row + i;
                let b = a + row;
                // Outward winding (counter-clockwise seen from outside).
                if j != 0 {
                    p.indices.extend([a, a + 1, b]);
                }
                if j != stacks - 1 {
                    p.indices.extend([a + 1, b + 1, b]);
                }
            }
        }
        p
    }

    fn grid(n: u32) -> Part {
        let mut p = Part::new("grid");
        for j in 0..=n {
            for i in 0..=n {
                let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
                p.verts.push([u, 0.0, v]);
                p.normals.push([0.0, 1.0, 0.0]);
                p.uvs.push([u, v]);
            }
        }
        for j in 0..n {
            for i in 0..n {
                let a = j * (n + 1) + i;
                let b = a + n + 1;
                p.indices.extend([a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        p
    }

    fn cube() -> Part {
        let mut p = Part::new("cube");
        let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
            ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
            ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
            ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
            ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ];
        for (n, a, b) in faces {
            let base = p.verts.len() as u32;
            for (s, t) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                p.verts
                    .push([0, 1, 2].map(|k| 0.5 * (n[k] + s * a[k] + t * b[k])));
                p.normals.push(n);
                p.uvs.push([(s + 1.0) / 2.0, (t + 1.0) / 2.0]);
            }
            p.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        p
    }

    fn assert_valid(p: &Part) {
        assert_eq!(p.indices.len() % 3, 0);
        assert_eq!(p.normals.len(), p.verts.len());
        assert_eq!(p.uvs.len(), p.verts.len());
        assert!(p.indices.iter().all(|&i| (i as usize) < p.verts.len()));
    }

    /// Every simplified triangle faces the same way as the original surface
    /// at that spot (for a sphere, outward).
    fn assert_outward(p: &Part) {
        for t in p.indices.as_chunks::<3>().0 {
            let n = tri_normal(p, t);
            let c = t.iter().fold([0.0; 3], |acc, &i| {
                let v = p.verts[i as usize].map(f64::from);
                [acc[0] + v[0], acc[1] + v[1], acc[2] + v[2]]
            });
            assert!(dot(n, c) > 0.0, "flipped triangle {t:?}");
        }
    }

    #[test]
    fn simplify_sphere_hits_ratio() {
        let s = uv_sphere(48, 32);
        let n = s.tri_count() as f32;
        assert_outward(&s);
        for ratio in [0.5, 0.25] {
            let out = simplify(&s, ratio);
            assert_valid(&out);
            let got = out.tri_count() as f32 / n;
            assert!(
                (got - ratio).abs() <= 0.1 * ratio,
                "ratio {ratio}: got {got} ({} of {n})",
                out.tri_count()
            );
            assert_outward(&out);
        }
    }

    #[test]
    fn simplify_grid_hits_ratio_without_flips() {
        let g = grid(40);
        let n = g.tri_count() as f32;
        for ratio in [0.5, 0.2] {
            let out = simplify(&g, ratio);
            assert_valid(&out);
            let got = out.tri_count() as f32 / n;
            assert!(
                (got - ratio).abs() <= 0.1 * ratio,
                "ratio {ratio}: got {got}"
            );
            for t in out.indices.as_chunks::<3>().0 {
                let nn = tri_normal(&out, t);
                assert!(nn[1] > 1e-9, "flipped or degenerate triangle {t:?}");
            }
            // The border stays exactly where it was.
            let (lo, hi) = out.bounds();
            assert!(lo.x.abs() < 1e-5 && lo.z.abs() < 1e-5);
            assert!((hi.x - 1.0).abs() < 1e-5 && (hi.z - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn simplify_cube_degrades_gracefully() {
        let c = cube();
        assert_outward(&c);
        let out = simplify(&c, 0.01);
        assert_valid(&out);
        assert_eq!(out.tri_count(), 12);
        assert_outward(&out);
        assert_eq!(simplify(&c, 1.0).indices, c.indices);
    }

    #[test]
    fn simplify_keeps_uv_seam() {
        let s = uv_sphere(48, 32);
        let out = simplify(&s, 0.3);
        assert_valid(&out);
        assert!(out.tri_count() < s.tri_count() / 2);
        // No triangle wraps across the seam (u jumping 0 <-> 1).
        for t in out.indices.as_chunks::<3>().0 {
            let us = t.map(|i| out.uvs[i as usize][0]);
            let span = us.iter().cloned().fold(f32::MIN, f32::max)
                - us.iter().cloned().fold(f32::MAX, f32::min);
            assert!(span < 0.5, "triangle {t:?} spans the seam (u {us:?})");
        }
        // Both sides of the seam survive at every original seam position.
        let seam = |u: f32| {
            out.uvs
                .iter()
                .filter(|uv| (uv[0] - u).abs() < 1e-6 && uv[1] > 1e-6 && uv[1] < 1.0 - 1e-6)
                .count()
        };
        assert_eq!(seam(0.0), 31);
        assert_eq!(seam(1.0), 31);
        // Determinism.
        assert_eq!(simplify(&s, 0.3).indices, out.indices);
    }
}
