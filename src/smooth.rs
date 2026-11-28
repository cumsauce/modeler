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
    /// subdiv: a triangle budget instead of `levels`: subdivide just past
    /// it, then decimate to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tris: Option<u32>,
    /// subdiv with tris: decimate symmetrically about this axis' zero
    /// plane (see `decimate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symmetric: Option<String>,
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
        only("tris", self.tris.is_some(), &["subdiv"])?;
        if self.tris.is_some() && self.levels.is_some() {
            return Err("give levels or tris, not both".into());
        }
        if self.tris.is_some_and(|t| t < 8) {
            return Err("tris: at least 8".into());
        }
        if let Some(s) = &self.symmetric {
            if self.tris.is_none() {
                return Err("symmetric: only with tris (it applies to the decimation)".into());
            }
            if !["x", "y", "z"].contains(&s.as_str()) {
                return Err(format!("symmetric: x, y or z, got `{s}`"));
            }
        }
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
        f("tris", self.tris.map(|v| v.to_string()));
        f("symmetric", self.symmetric.clone());
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
            // Relaxation drifts off the surface: project back onto the
            // field, with field normals and sharp creases restored.
            let field = |p: Vec3| g.sample(p);
            Ok(crate::sdf_finish::finish_mesh(
                name,
                part.material.clone(),
                m2.v,
                m2.t,
                &field,
                crate::sdf_finish::Sampling::grid(edge, edge * 0.5),
            ))
        }
        _ => {
            let (v, faces, mut sharp) = match &part.cage {
                // A cage carries its own polygons and crease weights.
                Some(c) => {
                    let rot = quat_of(part.rotation);
                    let (sc, tr) = (Vec3::from(part.scale), Vec3::from(part.translation));
                    let v: Vec<Vec3> = c
                        .verts
                        .iter()
                        .map(|p| rot * (Vec3::from(*p) * sc) + tr)
                        .collect();
                    let sharp: Sharpness = c
                        .creases
                        .iter()
                        .map(|&(a, b, w)| ((a.min(b), a.max(b)), weight(w)))
                        .collect();
                    (v, c.faces.clone(), sharp)
                }
                None => {
                    let m = welded(part)?;
                    let faces = merge_quads(&m.v, &m.t);
                    (m.v, faces, Sharpness::new())
                }
            };
            let corners: usize = faces.iter().map(Vec::len).sum();
            let est_of = |l: u32| corners * 2 * 4usize.pow(l - 1);
            // A budget: the fewest levels that reach it (within the limit).
            let levels = match def.tris {
                Some(t) => (1..=5)
                    .find(|&l| est_of(l) >= t as usize || l == 5 || est_of(l + 1) > MAX_TRIS)
                    .unwrap_or(1),
                None => def.levels.unwrap_or(2),
            };
            let est = est_of(levels);
            if est > MAX_TRIS {
                return Err(err(format!(
                    "{levels} level(s) of a {}-face cage make about {est} triangles (limit {MAX_TRIS}); use fewer levels or a coarser cage",
                    faces.len()
                )));
            }
            for e in crease_edges(&v, &faces, def.crease) {
                sharp.insert(e, f32::INFINITY);
            }
            let (v, f) = catmull_clark(v, faces, sharp, levels);
            let t = triangulate(&v, &f);
            let crease = def.crease.map_or(60.0, |c| c.clamp(20.0, 60.0));
            let out = to_part(name, part, &v, &t, crease);
            match def.tris {
                Some(tr) if out.tri_count() as f32 > tr as f32 * 1.02 => {
                    let d = crate::decimate::Decimate {
                        tris: Some(tr),
                        symmetric: def.symmetric.clone(),
                        ..Default::default()
                    };
                    let mut low = crate::decimate::decimate(name, &out, &d).map_err(err)?;
                    low.material = part.material.clone();
                    Ok(low)
                }
                _ => Ok(out),
            }
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

/// Where a cage's own vertices land after `levels` of Catmull-Clark (no
/// creases), in the cage's vertex order: what a fit compares with.
pub(crate) fn cage_images(v: Vec<Vec3>, faces: Vec<Vec<u32>>, levels: u32) -> Vec<Vec3> {
    let n = v.len();
    let (mut out, _) = catmull_clark(v, faces, Sharpness::new(), levels);
    out.truncate(n);
    out
}

/// Edge sharpness by (low, high) vertex: levels it stays sharp, infinite
/// for a hard crease.
type Sharpness = HashMap<(u32, u32), f32>;

/// A cage crease weight as a sharpness: [`crate::cage::SHARP`] and above
/// is sharp at every level.
fn weight(w: f32) -> f32 {
    if w >= crate::cage::SHARP {
        f32::INFINITY
    } else {
        w.max(0.0)
    }
}

/// Catmull-Clark with sharp edges (`sharp`, plus open borders and
/// non-manifold edges): sharp edges subdivide as curves, a vertex on two
/// sharp edges follows them, on three or more it stays a corner. A
/// finite sharpness s is sharp for s levels and smooth after (semi-sharp
/// creases: a fraction blends the smooth and sharp rules, and each level
/// takes 1 off the edges it makes).
fn catmull_clark(
    mut v: Vec<Vec3>,
    mut faces: Vec<Vec<u32>>,
    mut sharp: Sharpness,
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
        let sharpness: Vec<f32> = (0..ends.len())
            .map(|e| {
                if efaces[e].len() != 2 {
                    f32::INFINITY
                } else {
                    sharp.get(&ends[e]).copied().unwrap_or(0.0)
                }
            })
            .collect();
        let is_sharp: Vec<bool> = sharpness.iter().map(|s| *s > 0.0).collect();
        let fp: Vec<Vec3> = faces
            .iter()
            .map(|f| f.iter().map(|&i| v[i as usize]).sum::<Vec3>() / f.len() as f32)
            .collect();
        let ep: Vec<Vec3> = (0..ends.len())
            .map(|e| {
                let (a, b) = (v[ends[e].0 as usize], v[ends[e].1 as usize]);
                let s = sharpness[e];
                if s >= 1.0 {
                    (a + b) * 0.5
                } else {
                    let smooth = (a + b + fp[efaces[e][0]] + fp[efaces[e][1]]) * 0.25;
                    smooth.lerp((a + b) * 0.5, s)
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
                let smooth = || {
                    let n = ve[i].len() as f32;
                    let f = vf[i].iter().map(|&fi| fp[fi]).sum::<Vec3>() / vf[i].len() as f32;
                    let r = ve[i]
                        .iter()
                        .map(|&e| (v[ends[e].0 as usize] + v[ends[e].1 as usize]) * 0.5)
                        .sum::<Vec3>()
                        / n;
                    (f + 2.0 * r + (n - 3.0) * p) / n
                };
                let has_faces = !vf[i].is_empty() && !ve[i].is_empty();
                let hard = match sharp_e.len() {
                    0 | 1 if has_faces => return smooth(),
                    0 | 1 => return p,
                    2 => (other(sharp_e[0]) + 6.0 * p + other(sharp_e[1])) / 8.0,
                    _ => p,
                };
                // Semi-sharp: the mean sharpness of its creases blends the
                // smooth rule into the crease or corner rule.
                let s = sharp_e.iter().map(|&e| sharpness[e]).sum::<f32>() / sharp_e.len() as f32;
                if s >= 1.0 || !has_faces {
                    hard
                } else {
                    smooth().lerp(hard, s)
                }
            })
            .collect();
        let mut nvtx = vp;
        nvtx.extend(&ep);
        nvtx.extend(&fp);
        let e_of = |a: u32, b: u32| (nv + eid[&(a.min(b), a.max(b))]) as u32;
        let mut nf = Vec::with_capacity(faces.len() * 4);
        let mut nsharp = Sharpness::new();
        for (fi, f) in faces.iter().enumerate() {
            let c = (nv + ends.len() + fi) as u32;
            let n = f.len();
            for i in 0..n {
                let (prev, cur, next) = (f[(i + n - 1) % n], f[i], f[(i + 1) % n]);
                nf.push(vec![cur, e_of(cur, next), c, e_of(prev, cur)]);
            }
        }
        for (e, &(a, b)) in ends.iter().enumerate() {
            let s = sharpness[e] - 1.0;
            if s > 0.0 && efaces[e].len() == 2 {
                let m = (nv + e) as u32;
                nsharp.insert((a.min(m), a.max(m)), s);
                nsharp.insert((b.min(m), b.max(m)), s);
            }
        }
        v = nvtx;
        faces = nf;
        sharp = nsharp;
    }
    (v, faces)
}

/// One Catmull-Clark level of a polygon cage as linear stencils, so any
/// per-vertex attribute (positions, skin weights, morph deltas) can ride
/// through the same subdivision: new vertex `i` is `sum(w * old[j])` over
/// `rows[offsets[i]..offsets[i + 1]]`. The rules are the ones
/// [`catmull_clark`] uses with no creases: open borders and non-manifold
/// edges subdivide as curves. New vertices are the old vertex points, then
/// one per edge, then one per face; each face becomes one quad per corner
/// (`parent` gives the face it came from), in the same order as
/// [`catmull_clark`].
pub struct Stencil {
    pub offsets: Vec<u32>,
    pub rows: Vec<(u32, f32)>,
    pub faces: Vec<Vec<u32>>,
    pub parent: Vec<u32>,
    /// Per new face corner: the old face corner (`corner`, the edge to the
    /// next corner `edge`, or the face centre `centre`) it came from, for
    /// face-varying data (uvs): (old face, old corner index, kind 0 corner /
    /// 1 edge after it / 2 centre).
    pub from: Vec<[(u32, u8, u8); 4]>,
}

impl Stencil {
    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Applies the stencil to per-vertex data of any linear type.
    pub fn apply<T>(&self, old: &[T]) -> Vec<T>
    where
        T: Copy
            + Default
            + std::ops::Add<Output = T>
            + std::ops::Mul<f32, Output = T>
            + Send
            + Sync,
    {
        use rayon::prelude::*;
        (0..self.len())
            .into_par_iter()
            .map(|i| {
                let (a, b) = (self.offsets[i] as usize, self.offsets[i + 1] as usize);
                self.rows[a..b]
                    .iter()
                    .fold(T::default(), |acc, &(j, w)| acc + old[j as usize] * w)
            })
            .collect()
    }
}

pub fn catmull_clark_stencil(nv: usize, faces: &[Vec<u32>]) -> Stencil {
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
    let border: Vec<bool> = efaces.iter().map(|f| f.len() != 2).collect();
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
    let mut offsets = vec![0u32];
    let mut rows: Vec<(u32, f32)> = Vec::new();
    let mut push = |acc: &mut Vec<(u32, f32)>| {
        acc.sort_by_key(|a| a.0);
        let mut last: Option<(u32, f32)> = None;
        for &(j, w) in acc.iter() {
            match &mut last {
                Some(l) if l.0 == j => l.1 += w,
                _ => {
                    if let Some(l) = last.take() {
                        rows.push(l);
                    }
                    last = Some((j, w));
                }
            }
        }
        if let Some(l) = last {
            rows.push(l);
        }
        offsets.push(rows.len() as u32);
        acc.clear();
    };
    let face_pt = |fi: usize, k: f32, acc: &mut Vec<(u32, f32)>| {
        let f = &faces[fi];
        let w = k / f.len() as f32;
        acc.extend(f.iter().map(|&i| (i, w)));
    };
    let mut acc: Vec<(u32, f32)> = Vec::new();
    for i in 0..nv {
        let be: Vec<usize> = ve[i].iter().copied().filter(|&e| border[e]).collect();
        let other = |e: usize| {
            let (a, b) = ends[e];
            if a as usize == i { b } else { a }
        };
        match be.len() {
            0 if !vf[i].is_empty() && !ve[i].is_empty() => {
                let n = ve[i].len() as f32;
                let nf = vf[i].len() as f32;
                for &fi in &vf[i] {
                    face_pt(fi, 1.0 / (nf * n), &mut acc);
                }
                for &e in &ve[i] {
                    let (a, b) = ends[e];
                    acc.push((a, 1.0 / (n * n)));
                    acc.push((b, 1.0 / (n * n)));
                }
                acc.push((i as u32, (n - 3.0) / n));
            }
            2 => {
                acc.push((other(be[0]), 0.125));
                acc.push((other(be[1]), 0.125));
                acc.push((i as u32, 0.75));
            }
            _ => acc.push((i as u32, 1.0)),
        }
        push(&mut acc);
    }
    for (e, &(a, b)) in ends.iter().enumerate() {
        if border[e] {
            acc.push((a, 0.5));
            acc.push((b, 0.5));
        } else {
            acc.push((a, 0.25));
            acc.push((b, 0.25));
            face_pt(efaces[e][0], 0.25, &mut acc);
            face_pt(efaces[e][1], 0.25, &mut acc);
        }
        push(&mut acc);
    }
    for fi in 0..faces.len() {
        face_pt(fi, 1.0, &mut acc);
        push(&mut acc);
    }
    let e_of = |a: u32, b: u32| (nv + eid[&(a.min(b), a.max(b))]) as u32;
    let mut nf = Vec::with_capacity(faces.len() * 4);
    let mut parent = Vec::with_capacity(faces.len() * 4);
    let mut from = Vec::with_capacity(faces.len() * 4);
    for (fi, f) in faces.iter().enumerate() {
        let c = (nv + ends.len() + fi) as u32;
        let n = f.len();
        for i in 0..n {
            let (prev, cur, next) = (f[(i + n - 1) % n], f[i], f[(i + 1) % n]);
            nf.push(vec![cur, e_of(cur, next), c, e_of(prev, cur)]);
            parent.push(fi as u32);
            let p = ((i + n - 1) % n) as u8;
            from.push([
                (fi as u32, i as u8, 0),
                (fi as u32, i as u8, 1),
                (fi as u32, 0, 2),
                (fi as u32, p, 1),
            ]);
        }
    }
    Stencil {
        offsets,
        rows,
        faces: nf,
        parent,
        from,
    }
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

    fn cage_cube(creases: Vec<[f32; 3]>) -> Part {
        let mut d = crate::cage::CageDef {
            verts: vec![
                [-0.1, -0.1, -0.1],
                [0.1, -0.1, -0.1],
                [0.1, 0.1, -0.1],
                [-0.1, 0.1, -0.1],
                [-0.1, -0.1, 0.1],
                [0.1, -0.1, 0.1],
                [0.1, 0.1, 0.1],
                [-0.1, 0.1, 0.1],
            ],
            faces: vec![
                vec![0, 3, 2, 1],
                vec![4, 5, 6, 7],
                vec![0, 1, 5, 4],
                vec![2, 3, 7, 6],
                vec![1, 2, 6, 5],
                vec![0, 4, 7, 3],
            ],
            ..Default::default()
        };
        d.creases = creases;
        crate::cage::build("cc", &d).unwrap_or_else(|e| panic!("{e}"))
    }

    fn subdiv(p: &Part, levels: u32) -> Part {
        let mut def = sm("subdiv");
        def.levels = Some(levels);
        smooth("s", p, &def).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn subdiv_meets_a_triangle_budget() {
        let mut def = sm("subdiv");
        def.tris = Some(1000);
        def.symmetric = Some("x".into());
        let s = smooth("s", &cube(), &def).unwrap_or_else(|e| panic!("{e}"));
        closed(&s);
        let n = s.tri_count();
        assert!((900..=1100).contains(&n), "{n} triangles");
        // Already under budget: no decimation, the level that fits.
        def.tris = Some(768);
        def.symmetric = None;
        let s = smooth("s", &cube(), &def).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.tri_count(), 768);
        def.levels = Some(2);
        assert!(def.validate().is_err(), "levels and tris");
    }

    #[test]
    fn a_cage_subdivides_its_own_quads() {
        // The cage op's cube is the box primitive's subdivision exactly.
        let a = subdiv(&cage_cube(Vec::new()), 3);
        let b = subdiv(&cube(), 3);
        closed(&a);
        assert_eq!(a.tri_count(), b.tri_count());
        assert!(
            (volume(&a) - volume(&b)).abs() < 1e-7,
            "{} vs {}",
            volume(&a),
            volume(&b)
        );
        // A twisted (non-planar) quad cage: as triangles each quad would be
        // two faces, so the quad count shows the polygons were used.
        let mut c = cage_cube(Vec::new());
        let cg = c.cage.as_ref().map(|c| (**c).clone()).unwrap_or_default();
        let mut cg = cg;
        cg.verts[6] = [0.13, 0.12, 0.1];
        c.verts = cg.verts.clone();
        c.cage = Some(std::sync::Arc::new(cg));
        let t = subdiv(&c, 2);
        closed(&t);
        assert_eq!(t.tri_count(), 6 * 16 * 2, "six quads, two levels");
    }

    #[test]
    fn crease_weights_sharpen_one_edge_by_degree() {
        // The cube's bottom ring creased: hard (10), one level (1), none.
        let ring = |w: f32| vec![[0.0, 1.0, w], [1.0, 5.0, w], [5.0, 4.0, w], [4.0, 0.0, w]];
        let low = |p: &Part| p.bounds().0.y;
        let hard = subdiv(&cage_cube(ring(10.0)), 3);
        let semi = subdiv(&cage_cube(ring(1.0)), 3);
        let soft = subdiv(&cage_cube(Vec::new()), 3);
        closed(&hard);
        // A hard crease keeps the bottom face flat on its cage plane; the
        // corner vertices sit on three creased edges... here two (the ring)
        // plus none vertical, so the ring follows its own curve.
        assert!((low(&hard) + 0.1).abs() < 1e-4, "hard {}", low(&hard));
        assert!(
            low(&soft) > low(&semi) + 1e-3,
            "soft {} semi {}",
            low(&soft),
            low(&semi)
        );
        assert!(
            low(&semi) > low(&hard) - 1e-6,
            "semi {} hard {}",
            low(&semi),
            low(&hard)
        );
        // Every edge hard: the cube stays a cube.
        let mut all = ring(10.0);
        all.extend([
            [2.0, 3.0, 10.0],
            [3.0, 7.0, 10.0],
            [7.0, 6.0, 10.0],
            [6.0, 2.0, 10.0],
        ]);
        all.extend([
            [0.0, 3.0, 10.0],
            [1.0, 2.0, 10.0],
            [5.0, 6.0, 10.0],
            [4.0, 7.0, 10.0],
        ]);
        let box_ = subdiv(&cage_cube(all), 2);
        assert!((volume(&box_) - 0.008).abs() < 1e-6, "{}", volume(&box_));
    }

    #[test]
    fn a_fitted_quad_loft_spans_its_outlines_after_subdiv() {
        let ring: Vec<[f32; 2]> = (0..12)
            .map(|i| {
                let a = i as f32 / 12.0 * std::f32::consts::TAU;
                [0.3 * a.cos(), 0.2 * a.sin()]
            })
            .collect();
        // Sections square to z: profile [x, y] is world x, y.
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let sec = |z: f32| crate::loft::LoftSection {
            profile: ring.clone(),
            at: [0.0, 0.0, z],
            t: None,
            rotation: Some([-h, 0.0, 0.0, h]),
            scale: [1.0, 1.0],
        };
        let secs: Vec<_> = [-1.2, -0.6, 0.0, 0.6, 1.2].into_iter().map(sec).collect();
        let fit = crate::loft::LoftFit {
            side: vec![[-1.5, 0.2], [1.5, 0.2], [1.5, 1.2], [-1.5, 1.2]],
            top: vec![[0.0, -1.5], [0.9, -1.5], [0.9, 1.5], [0.0, 1.5]],
            levels: 2,
        };
        let o = crate::loft::LoftOpts {
            path: &[],
            smooth: false,
            samples: 0,
            steps: 0,
            round: 0.0,
            quads: true,
            fit: Some(&fit),
        };
        let p = crate::loft::loft("body", &secs, &o).unwrap_or_else(|e| panic!("{e}"));
        let s = subdiv(&p, 2);
        closed(&s);
        // The middle station's subdivided ring spans the outlines.
        let mid: Vec<Vec3> = s
            .verts
            .iter()
            .map(|v| Vec3::from(*v))
            .filter(|v| v.z.abs() < 0.02)
            .collect();
        let (lo, hi) = mid
            .iter()
            .fold((Vec3::MAX, Vec3::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        for (got, want) in [(lo.x, -0.9), (hi.x, 0.9), (lo.y, 0.2), (hi.y, 1.2)] {
            assert!((got - want).abs() < 0.003, "{lo} {hi}");
        }
        // Without levels the cage itself spans them (the surface sits inside).
        let fit0 = crate::loft::LoftFit {
            levels: 0,
            ..fit.clone()
        };
        let o0 = crate::loft::LoftOpts {
            fit: Some(&fit0),
            ..o
        };
        let c = crate::loft::loft("body", &secs, &o0).unwrap_or_else(|e| panic!("{e}"));
        let (clo, chi) = c.bounds();
        assert!(
            (chi.x - 0.9).abs() < 1e-4 && (clo.y - 0.2).abs() < 1e-4,
            "{clo} {chi}"
        );
        let s0 = subdiv(&c, 2);
        assert!(s0.bounds().1.x < 0.9 - 0.005, "subdivision pulls in");
    }

    #[test]
    fn a_quad_loft_hands_its_rings_to_subdiv() {
        let ring = |r: f32| -> Vec<[f32; 2]> {
            (0..8)
                .map(|i| {
                    let a = i as f32 / 8.0 * std::f32::consts::TAU;
                    [r * a.cos(), 0.7 * r * a.sin()]
                })
                .collect()
        };
        let sec = |r: f32, y: f32| crate::loft::LoftSection {
            profile: ring(r),
            at: [0.0, y, 0.0],
            t: None,
            rotation: None,
            scale: [1.0, 1.0],
        };
        let secs = [sec(0.3, 0.0), sec(0.5, 0.5), sec(0.25, 1.0)];
        let mut o = crate::loft::LoftOpts {
            path: &[],
            smooth: false,
            samples: 0,
            steps: 0,
            round: 0.0,
            quads: true,
            fit: None,
        };
        let p = crate::loft::loft("l", &secs, &o).unwrap_or_else(|e| panic!("{e}"));
        let c = p.cage.as_ref().map(|c| (c.verts.len(), c.faces.len()));
        assert_eq!(c, Some((24, 16 + 2)), "two spans of 8 quads, two caps");
        let s = subdiv(&p, 2);
        closed(&s);
        // 16 quads -> 16 x 16 quads; each 8-gon cap -> 8 x 4 quads.
        assert_eq!(s.tri_count(), (16 * 16 + 2 * 8 * 4) * 2);
        // Sections of different point counts are refused.
        let mut bad = secs.clone();
        bad[1].profile.pop();
        let e = crate::loft::loft("l", &bad, &o)
            .err()
            .map(|e| e.to_string());
        assert!(e.is_some_and(|e| e.contains("same number of points")));
        o.quads = false;
        let p = crate::loft::loft("l", &secs, &o).unwrap_or_else(|e| panic!("{e}"));
        assert!(p.cage.is_none());
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
            // One value per position: split copies at creases would
            // otherwise weight the spread by how the normals were split.
            let mut seen = std::collections::HashSet::new();
            let k: Vec<f32> = crate::organic::vertex_curvature(p)
                .into_iter()
                .zip(&p.verts)
                .filter(|(_, v)| seen.insert(v.map(f32::to_bits)))
                .map(|(k, _)| k)
                .collect();
            let m = k.iter().sum::<f32>() / k.len() as f32;
            (k.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / k.len() as f32).sqrt()
        };
        let (b0, b1) = (bumpy(&rock), bumpy(&s));
        assert!(b1 < b0 * 0.6, "curvature spread {b0} -> {b1}");
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
