//! `smooth`: surface fairing and remeshing for organic forms.
//!
//! - `relax`: Taubin smoothing (a shrink step then an inflate step per
//!   iteration), so lumps and pinches even out without the part shrinking
//!   the way plain Laplacian smoothing does.
//! - `remesh`: a closed solid resampled as a distance field and re-meshed at
//!   one edge length, then tangentially relaxed: even, well-shaped triangles
//!   everywhere (sharp edges round off to about the edge length).
//! - `subdiv`: Catmull-Clark subdivision of a coarse cage. Coplanar
//!   triangle pairs are first merged back into quads (a box primitive is six
//!   quads again), edges sharper than `crease` degrees and open borders stay
//!   sharp, and every level quadruples the faces.
//!
//! All three work in model space on the welded mesh, keep a closed solid
//! closed and outward, and re-crease normals afterwards.

use std::collections::{HashMap, HashSet};

use crate::{ModelerError, Part, quat_of};
use bevy::math::Vec3;

/// Most triangles a smooth may make.
const MAX_TRIS: usize = 400_000;

pub const MODES: &[&str] = &["relax", "remesh", "subdiv"];

/// A welded triangle mesh in model space.
struct Mesh {
    v: Vec<Vec3>,
    t: Vec<[u32; 3]>,
    closed: bool,
}

/// Welds `part` (baked to model space) by position. Closed solids go
/// through Manifold first so the result is one clean surface.
fn welded(part: &Part) -> Result<Mesh, ModelerError> {
    let (pos, idx, closed): (Vec<Vec3>, Vec<u32>, bool) = match crate::csg::part_to_manifold(part) {
        Ok(m) => {
            let mesh = m.to_meshgl64();
            let n = mesh.num_prop().max(3);
            let pos = mesh
                .vert_properties()
                .chunks(n)
                .map(|v| Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32))
                .collect();
            let idx = mesh.tri_verts().iter().map(|&i| i as u32).collect();
            (pos, idx, true)
        }
        Err(_) => {
            let rot = quat_of(part.rotation);
            let (sc, tr) = (Vec3::from(part.scale), Vec3::from(part.translation));
            let pos: Vec<Vec3> = part
                .verts
                .iter()
                .map(|v| rot * (Vec3::from(*v) * sc) + tr)
                .collect();
            let idx = if part.indices.is_empty() {
                (0..(pos.len() - pos.len() % 3) as u32).collect()
            } else {
                part.indices.clone()
            };
            (pos, idx, false)
        }
    };
    let (lo, hi) = pos.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), p| (a.min(*p), b.max(*p)),
    );
    let inv = 1.0 / ((hi - lo).max_element() * 1e-6).clamp(1e-7, 1e-5);
    let mut map: HashMap<[i64; 3], u32> = HashMap::new();
    let mut v = Vec::new();
    let remap: Vec<u32> = pos
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
    let t = idx
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| t.map(|i| remap[i as usize]))
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    Ok(Mesh { v, t, closed })
}

fn to_part(name: &str, src: &Part, v: &[Vec3], t: &[[u32; 3]], crease: f32) -> Part {
    let mut out = Part::new(name);
    out.material = src.material.clone();
    out.verts = v.iter().map(|p| p.to_array()).collect();
    out.indices = t.iter().flatten().copied().collect();
    crate::uv::crease_normals(&mut out, crease);
    let _ = crate::uv::ensure_uvs(&mut out, Some("box"));
    out
}

/// Vertex neighbours and the vertices on open borders.
fn neighbours(n: usize, t: &[[u32; 3]]) -> (Vec<Vec<u32>>, Vec<bool>) {
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for tri in t {
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut border = vec![false; n];
    for (&(a, b), &c) in &edges {
        nb[a as usize].push(b);
        nb[b as usize].push(a);
        if c == 1 {
            border[a as usize] = true;
            border[b as usize] = true;
        }
    }
    for l in &mut nb {
        l.sort_unstable();
    }
    (nb, border)
}

/// Taubin smoothing: per iteration a Laplacian step of `lambda` and an
/// inflating one of `mu` (< -lambda), so the shape doesn't shrink. Open
/// borders stay put.
fn taubin(v: &mut [Vec3], nb: &[Vec<u32>], pin: &[bool], iterations: u32, lambda: f32) {
    let mu = -(lambda / (1.0 - 0.1 * lambda)) - 0.01;
    let step = |v: &mut [Vec3], f: f32| {
        let delta: Vec<Vec3> = (0..v.len())
            .map(|i| {
                if pin[i] || nb[i].is_empty() {
                    return Vec3::ZERO;
                }
                let c = nb[i].iter().map(|&j| v[j as usize]).sum::<Vec3>() / nb[i].len() as f32;
                (c - v[i]) * f
            })
            .collect();
        for (p, d) in v.iter_mut().zip(delta) {
            *p += d;
        }
    };
    for _ in 0..iterations {
        step(v, lambda);
        step(v, mu);
    }
}

/// Moves vertices toward their neighbours' centroid within the tangent
/// plane only (evens out triangle sizes without changing the shape).
fn tangential(v: &mut [Vec3], t: &[[u32; 3]], nb: &[Vec<u32>], iterations: u32) {
    for _ in 0..iterations {
        let mut nrm = vec![Vec3::ZERO; v.len()];
        for tri in t {
            let [a, b, c] = tri.map(|i| v[i as usize]);
            let n = (b - a).cross(c - a);
            for &i in tri {
                nrm[i as usize] += n;
            }
        }
        let delta: Vec<Vec3> = (0..v.len())
            .map(|i| {
                if nb[i].is_empty() {
                    return Vec3::ZERO;
                }
                let n = nrm[i].normalize_or_zero();
                let c = nb[i].iter().map(|&j| v[j as usize]).sum::<Vec3>() / nb[i].len() as f32;
                let d = c - v[i];
                (d - n * n.dot(d)) * 0.5
            })
            .collect();
        for (p, d) in v.iter_mut().zip(delta) {
            *p += d;
        }
    }
}

fn mean_edge(v: &[Vec3], t: &[[u32; 3]]) -> f32 {
    let mut s = 0.0f64;
    for tri in t {
        for k in 0..3 {
            s += v[tri[k] as usize].distance(v[tri[(k + 1) % 3] as usize]) as f64;
        }
    }
    (s / (3 * t.len().max(1)) as f64) as f32
}

/// The parameters of a `smooth` op (see `SmoothSpec` for the docs).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct Smooth {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crease: Option<f32>,
}

impl Smooth {
    pub fn validate(&self) -> Result<(), String> {
        if !MODES.contains(&self.mode.as_str()) {
            return Err(format!(
                "mode: must be relax, remesh or subdiv, got `{}`",
                self.mode
            ));
        }
        let only = |f: &str, set: bool, modes: &[&str]| -> Result<(), String> {
            if set && !modes.contains(&self.mode.as_str()) {
                return Err(format!(
                    "{f}: only for {} (this is {})",
                    modes.join(" / "),
                    self.mode
                ));
            }
            Ok(())
        };
        only("strength", self.strength.is_some(), &["relax"])?;
        only("edge", self.edge.is_some(), &["remesh"])?;
        only("levels", self.levels.is_some(), &["subdiv"])?;
        only("crease", self.crease.is_some(), &["subdiv"])?;
        if let Some(s) = self.strength
            && !(s > 0.0 && s <= 1.0)
        {
            return Err(format!("strength: 0 < strength <= 1, got {s}"));
        }
        if let Some(e) = self.edge
            && !(e.is_finite() && e > 0.0)
        {
            return Err(format!("edge: must be > 0 meters, got {e}"));
        }
        if let Some(l) = self.levels
            && !(1..=5).contains(&l)
        {
            return Err(format!("levels: 1..5, got {l}"));
        }
        if let Some(i) = self.iterations
            && i > 200
        {
            return Err(format!("iterations: at most 200, got {i}"));
        }
        if let Some(c) = self.crease
            && !(0.0..=180.0).contains(&c)
        {
            return Err(format!("crease: 0..180 degrees, got {c}"));
        }
        Ok(())
    }

    pub fn summary(&self) -> String {
        let mut s = self.mode.clone();
        let mut f = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                s.push_str(&format!(" {k}={v}"));
            }
        };
        f("iterations", self.iterations.map(|v| v.to_string()));
        f("strength", self.strength.map(|v| v.to_string()));
        f("edge", self.edge.map(|v| v.to_string()));
        f("levels", self.levels.map(|v| v.to_string()));
        f("crease", self.crease.map(|v| v.to_string()));
        s
    }
}

/// A smoothed copy of `part`, named `name`, in model space.
pub fn smooth(name: &str, part: &Part, def: &Smooth) -> Result<Part, ModelerError> {
    let err = |m: String| ModelerError::Invalid(format!("smooth `{name}`: {m}"));
    def.validate().map_err(err)?;
    if part.verts.is_empty() {
        return Err(err(format!("`{}` has no geometry", part.name)));
    }
    match def.mode.as_str() {
        "relax" => {
            let mut m = welded(part)?;
            let (nb, border) = neighbours(m.v.len(), &m.t);
            taubin(
                &mut m.v,
                &nb,
                &border,
                def.iterations.unwrap_or(10),
                def.strength.unwrap_or(0.5),
            );
            Ok(to_part(name, part, &m.v, &m.t, 60.0))
        }
        "remesh" => {
            let m = welded(part)?;
            if !m.closed {
                return Err(err(format!(
                    "remesh needs a closed solid; `{}` is an open sheet (use relax or subdiv)",
                    part.name
                )));
            }
            let (lo, hi) = part.bounds();
            let size = (hi - lo).max_element();
            let edge = def
                .edge
                .unwrap_or_else(|| mean_edge(&m.v, &m.t))
                .max(size / 400.0);
            let cells = ((hi - lo) / edge + Vec3::splat(8.0)).to_array();
            let n = cells.iter().map(|c| *c as f64).product::<f64>();
            if n > 27e6 {
                return Err(err(format!(
                    "edge {edge} m would sample {:.0}M points (limit 27M); use edge >= {:.4}",
                    n / 1e6,
                    edge * (n / 27e6).cbrt() as f32 * 1.01
                )));
            }
            let g = crate::blob::sdf_grid(part, edge * 0.5, edge * 3.0)?;
            let pad = Vec3::splat(edge * 2.0);
            let (blo, bhi) = (lo - pad, hi + pad);
            let mf = manifold_csg::Manifold::from_sdf(
                |x, y, z| -g.sample(Vec3::new(x as f32, y as f32, z as f32)) as f64,
                (
                    [blo.x as f64, blo.y as f64, blo.z as f64],
                    [bhi.x as f64, bhi.y as f64, bhi.z as f64],
                ),
                edge as f64,
                0.0,
                -1.0,
            );
            if mf.is_empty() {
                return Err(err(format!(
                    "nothing left at edge {edge} m: the part is thinner than that"
                )));
            }
            let tmp = crate::csg::manifold_to_part(&mf, name, part.material.clone());
            let mut m2 = welded(&tmp)?;
            let (nb, _) = neighbours(m2.v.len(), &m2.t);
            tangential(&mut m2.v, &m2.t, &nb, def.iterations.unwrap_or(5));
            Ok(to_part(name, part, &m2.v, &m2.t, 60.0))
        }
        _ => {
            let m = welded(part)?;
            let levels = def.levels.unwrap_or(2);
            let faces = merge_quads(&m.v, &m.t);
            let corners: usize = faces.iter().map(Vec::len).sum();
            let est = corners * 2 * 4usize.pow(levels - 1);
            if est > MAX_TRIS {
                return Err(err(format!(
                    "{levels} level(s) of a {}-face cage make about {est} triangles (limit {MAX_TRIS}); use fewer levels or a coarser cage",
                    faces.len()
                )));
            }
            let sharp = crease_edges(&m.v, &faces, def.crease);
            let (v, f) = catmull_clark(m.v, faces, sharp, levels);
            let t = triangulate(&v, &f);
            let crease = def.crease.map_or(60.0, |c| c.clamp(20.0, 60.0));
            Ok(to_part(name, part, &v, &t, crease))
        }
    }
}

/// Faces of the cage: coplanar triangle pairs that form a quad across their
/// longest edge (a box face, a cylinder side) merged, the rest triangles.
fn merge_quads(v: &[Vec3], t: &[[u32; 3]]) -> Vec<Vec<u32>> {
    let normal = |tri: &[u32; 3]| {
        let [a, b, c] = tri.map(|i| v[i as usize]);
        (b - a).cross(c - a).normalize_or_zero()
    };
    let longest = |tri: &[u32; 3]| -> usize {
        (0..3)
            .max_by(|&i, &j| {
                let l =
                    |k: usize| v[tri[k] as usize].distance_squared(v[tri[(k + 1) % 3] as usize]);
                l(i).total_cmp(&l(j))
            })
            .unwrap_or(0)
    };
    let mut by_edge: HashMap<(u32, u32), usize> = HashMap::new();
    for (i, tri) in t.iter().enumerate() {
        let k = longest(tri);
        by_edge.insert((tri[k], tri[(k + 1) % 3]), i);
    }
    let mut used = vec![false; t.len()];
    let mut faces = Vec::with_capacity(t.len());
    for (i, tri) in t.iter().enumerate() {
        if used[i] {
            continue;
        }
        let k = longest(tri);
        let (a, b) = (tri[k], tri[(k + 1) % 3]);
        let c = tri[(k + 2) % 3];
        if let Some(&j) = by_edge.get(&(b, a))
            && !used[j]
            && j != i
            && normal(tri).dot(normal(&t[j])) > 0.9998
        {
            let o = &t[j];
            let d = o.iter().copied().find(|x| *x != a && *x != b).unwrap_or(a);
            used[i] = true;
            used[j] = true;
            // tri = (a, b, c) and the other (b, a, d): quad a, d, b, c.
            faces.push(vec![a, d, b, c]);
            continue;
        }
        used[i] = true;
        faces.push(tri.to_vec());
    }
    faces
}

fn newell(v: &[Vec3], f: &[u32]) -> Vec3 {
    let mut n = Vec3::ZERO;
    for i in 0..f.len() {
        let a = v[f[i] as usize];
        let b = v[f[(i + 1) % f.len()] as usize];
        n += a.cross(b);
    }
    n.normalize_or_zero()
}

/// Edges whose faces meet at more than `crease` degrees (none when unset).
fn crease_edges(v: &[Vec3], faces: &[Vec<u32>], crease: Option<f32>) -> HashSet<(u32, u32)> {
    let mut out = HashSet::new();
    let Some(c) = crease else {
        return out;
    };
    let cos = c.to_radians().cos();
    let mut by: HashMap<(u32, u32), Vec<Vec3>> = HashMap::new();
    for f in faces {
        let n = newell(v, f);
        for i in 0..f.len() {
            let (a, b) = (f[i], f[(i + 1) % f.len()]);
            by.entry((a.min(b), a.max(b))).or_default().push(n);
        }
    }
    for (e, ns) in by {
        if ns.len() == 2 && ns[0].dot(ns[1]) < cos {
            out.insert(e);
        }
    }
    out
}

/// Catmull-Clark with sharp edges (`sharp`, plus open borders and
/// non-manifold edges): sharp edges subdivide as curves, a vertex on two
/// sharp edges follows them, on three or more it stays a corner.
fn catmull_clark(
    mut v: Vec<Vec3>,
    mut faces: Vec<Vec<u32>>,
    mut sharp: HashSet<(u32, u32)>,
    levels: u32,
) -> (Vec<Vec3>, Vec<Vec<u32>>) {
    for _ in 0..levels {
        let nv = v.len();
        // Edges: id, endpoints, adjacent faces.
        let mut eid: HashMap<(u32, u32), usize> = HashMap::new();
        let mut ends: Vec<(u32, u32)> = Vec::new();
        let mut efaces: Vec<Vec<usize>> = Vec::new();
        for (fi, f) in faces.iter().enumerate() {
            for i in 0..f.len() {
                let (a, b) = (f[i], f[(i + 1) % f.len()]);
                let k = (a.min(b), a.max(b));
                let id = *eid.entry(k).or_insert_with(|| {
                    ends.push(k);
                    efaces.push(Vec::new());
                    ends.len() - 1
                });
                efaces[id].push(fi);
            }
        }
        let is_sharp: Vec<bool> = (0..ends.len())
            .map(|e| efaces[e].len() != 2 || sharp.contains(&ends[e]))
            .collect();
        let fp: Vec<Vec3> = faces
            .iter()
            .map(|f| f.iter().map(|&i| v[i as usize]).sum::<Vec3>() / f.len() as f32)
            .collect();
        let ep: Vec<Vec3> = (0..ends.len())
            .map(|e| {
                let (a, b) = (v[ends[e].0 as usize], v[ends[e].1 as usize]);
                if is_sharp[e] {
                    (a + b) * 0.5
                } else {
                    (a + b + fp[efaces[e][0]] + fp[efaces[e][1]]) * 0.25
                }
            })
            .collect();
        // Per vertex: adjacent faces and edges.
        let mut vf: Vec<Vec<usize>> = vec![Vec::new(); nv];
        for (fi, f) in faces.iter().enumerate() {
            for &i in f {
                vf[i as usize].push(fi);
            }
        }
        let mut ve: Vec<Vec<usize>> = vec![Vec::new(); nv];
        for (e, &(a, b)) in ends.iter().enumerate() {
            ve[a as usize].push(e);
            ve[b as usize].push(e);
        }
        let vp: Vec<Vec3> = (0..nv)
            .map(|i| {
                let p = v[i];
                let sharp_e: Vec<usize> = ve[i].iter().copied().filter(|&e| is_sharp[e]).collect();
                let other = |e: usize| {
                    let (a, b) = ends[e];
                    v[if a as usize == i { b } else { a } as usize]
                };
                match sharp_e.len() {
                    0 | 1 if !vf[i].is_empty() && !ve[i].is_empty() => {
                        let n = ve[i].len() as f32;
                        let f = vf[i].iter().map(|&fi| fp[fi]).sum::<Vec3>() / vf[i].len() as f32;
                        let r = ve[i]
                            .iter()
                            .map(|&e| (v[ends[e].0 as usize] + v[ends[e].1 as usize]) * 0.5)
                            .sum::<Vec3>()
                            / n;
                        (f + 2.0 * r + (n - 3.0) * p) / n
                    }
                    2 => (other(sharp_e[0]) + 6.0 * p + other(sharp_e[1])) / 8.0,
                    _ => p,
                }
            })
            .collect();
        let mut nvtx = vp;
        nvtx.extend(&ep);
        nvtx.extend(&fp);
        let e_of = |a: u32, b: u32| (nv + eid[&(a.min(b), a.max(b))]) as u32;
        let mut nf = Vec::with_capacity(faces.len() * 4);
        let mut nsharp = HashSet::new();
        for (fi, f) in faces.iter().enumerate() {
            let c = (nv + ends.len() + fi) as u32;
            let n = f.len();
            for i in 0..n {
                let (prev, cur, next) = (f[(i + n - 1) % n], f[i], f[(i + 1) % n]);
                nf.push(vec![cur, e_of(cur, next), c, e_of(prev, cur)]);
            }
        }
        for (e, &(a, b)) in ends.iter().enumerate() {
            if is_sharp[e] && efaces[e].len() == 2 {
                let m = (nv + e) as u32;
                nsharp.insert((a.min(m), a.max(m)));
                nsharp.insert((b.min(m), b.max(m)));
            }
        }
        v = nvtx;
        faces = nf;
        sharp = nsharp;
    }
    (v, faces)
}

/// Quads split along their shorter diagonal; other polygons fanned.
fn triangulate(v: &[Vec3], faces: &[Vec<u32>]) -> Vec<[u32; 3]> {
    let mut t = Vec::with_capacity(faces.len() * 2);
    for f in faces {
        if f.len() == 4 {
            let d0 = v[f[0] as usize].distance_squared(v[f[2] as usize]);
            let d1 = v[f[1] as usize].distance_squared(v[f[3] as usize]);
            if d0 <= d1 {
                t.push([f[0], f[1], f[2]]);
                t.push([f[0], f[2], f[3]]);
            } else {
                t.push([f[1], f[2], f[3]]);
                t.push([f[1], f[3], f[0]]);
            }
        } else {
            for i in 1..f.len().saturating_sub(1) {
                t.push([f[0], f[i], f[i + 1]]);
            }
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Part {
        let mut p = crate::primitives::primitive(crate::PrimitiveKind::Box, [0.2, 0.2, 0.2], "c")
            .unwrap_or_else(|e| panic!("{e}"));
        p.visible = true;
        p
    }

    fn volume(p: &Part) -> f64 {
        p.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]));
                a.dot(b.cross(c)) as f64 / 6.0
            })
            .sum()
    }

    fn closed(p: &Part) {
        assert_eq!(
            crate::verify::open_edge_count(p),
            0,
            "{} has open edges",
            p.name
        );
        assert!(volume(p) > 0.0, "{} inside out", p.name);
    }

    fn sm(mode: &str) -> Smooth {
        Smooth {
            mode: mode.into(),
            ..Smooth::default()
        }
    }

    #[test]
    fn subdividing_a_cube_cage_rounds_it_toward_a_sphere() {
        let c = cube();
        let mut def = sm("subdiv");
        def.levels = Some(3);
        let s = smooth("s", &c, &def).unwrap_or_else(|e| panic!("{e}"));
        closed(&s);
        assert_eq!(
            s.indices.len() / 3,
            6 * 4 * 16 * 2,
            "six quads, three levels"
        );
        let r: Vec<f32> = s.verts.iter().map(|v| Vec3::from(*v).length()).collect();
        let (lo, hi) = r
            .iter()
            .fold((f32::MAX, 0.0f32), |(a, b), x| (a.min(*x), b.max(*x)));
        assert!(hi / lo < 1.25, "roundish: radii {lo}..{hi}");
        // With creases at 30 degrees the cube stays a cube.
        def.crease = Some(30.0);
        let k = smooth("k", &c, &def).unwrap_or_else(|e| panic!("{e}"));
        closed(&k);
        let (lo, hi) = k.bounds();
        assert!(
            (hi.x - 0.1).abs() < 1e-4 && (lo.y + 0.1).abs() < 1e-4,
            "{lo} {hi}"
        );
    }

    #[test]
    fn relax_smooths_noise_without_shrinking() {
        let mut def = crate::deform::Deform {
            mode: "displace".into(),
            amount: Some(0.02),
            scale: Some(0.05),
            ..Default::default()
        };
        def.seed = Some(3);
        let rock = crate::deform::deform(
            "r",
            &crate::primitives::primitive(crate::PrimitiveKind::Sphere, [0.2, 0.0, 0.0], "s")
                .unwrap_or_else(|e| panic!("{e}")),
            &def,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let mut r = sm("relax");
        r.iterations = Some(20);
        let s = smooth("s", &rock, &r).unwrap_or_else(|e| panic!("{e}"));
        closed(&s);
        let spread = |p: &Part| {
            let r: Vec<f32> = p.verts.iter().map(|v| Vec3::from(*v).length()).collect();
            let m = r.iter().sum::<f32>() / r.len() as f32;
            (
                m,
                (r.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / r.len() as f32).sqrt(),
            )
        };
        let (m0, _) = spread(&rock);
        let (m1, _) = spread(&s);
        let bumpy = |p: &Part| {
            let k = crate::organic::vertex_curvature(p);
            let m = k.iter().sum::<f32>() / k.len() as f32;
            (k.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / k.len() as f32).sqrt()
        };
        let (b0, b1) = (bumpy(&rock), bumpy(&s));
        assert!(b1 < b0 * 0.5, "curvature spread {b0} -> {b1}");
        assert!((m1 - m0).abs() / m0 < 0.03, "radius {m0} -> {m1}");
    }

    #[test]
    fn remesh_makes_even_triangles() {
        let p =
            crate::primitives::primitive(crate::PrimitiveKind::Cylinder, [0.1, 0.4, 0.0], "cyl")
                .unwrap_or_else(|e| panic!("{e}"));
        let mut def = sm("remesh");
        def.edge = Some(0.02);
        let s = smooth("s", &p, &def).unwrap_or_else(|e| panic!("{e}"));
        closed(&s);
        let m = welded(&s).unwrap_or_else(|e| panic!("{e}"));
        let mut lens = Vec::new();
        for t in &m.t {
            for k in 0..3 {
                lens.push(m.v[t[k] as usize].distance(m.v[t[(k + 1) % 3] as usize]));
            }
        }
        lens.sort_by(f32::total_cmp);
        let (p10, p90) = (lens[lens.len() / 10], lens[lens.len() * 9 / 10]);
        assert!(p90 / p10 < 3.0, "edge spread {p10}..{p90}");
        assert!((volume(&s) - volume(&p)).abs() / volume(&p) < 0.08);
    }

    #[test]
    fn fields_are_checked_per_mode() {
        let mut d = sm("relax");
        d.levels = Some(2);
        let e = d.validate().err().unwrap_or_default();
        assert!(e.contains("levels: only for subdiv"), "{e}");
    }
}
