//! `cage`: a polygon cage for `smooth subdiv`, given as vertices and
//! polygon faces (quads, mostly) with per-edge crease weights.
//!
//! A part's triangles cannot say which pairs were one quad: `subdiv` merges
//! only coplanar pairs back, so a curved cage (a car body's sections, a
//! hull) reaches Catmull-Clark as triangles and leaves lumps in the
//! reflections. A part made by `cage` (or a `loft` with `quads: true`)
//! carries its polygons along as a [`Cage`], and `subdiv` reads those
//! instead. The part itself is the cage triangulated, so it can be looked
//! at, measured and cut like any other.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::math::Vec3;
use serde::{Deserialize, Serialize};

use crate::{ModelerError, Part};

/// Crease weights at or above this are sharp at every level.
pub const SHARP: f32 = 10.0;

/// The polygons a part was built from, in the part's own coordinates (the
/// same space as `Part::verts`). Derived by the op; never saved.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Cage {
    pub verts: Vec<[f32; 3]>,
    pub faces: Vec<Vec<u32>>,
    /// (a, b, weight): the edge a-b stays sharp for `weight` levels
    /// (fractions blend), [`SHARP`] or more at every level.
    pub creases: Vec<(u32, u32, f32)>,
}

/// The `cage` op's settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CageDef {
    pub verts: Vec<[f32; 3]>,
    pub faces: Vec<Vec<u32>>,
    /// `[a, b, weight]` per creased edge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creases: Vec<[f32; 3]>,
    /// "x", "y" or "z": the cage is one half (every vertex on the + side or
    /// on the plane), mirrored across that axis' zero plane and joined
    /// along it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
}

fn axis_of(s: &str) -> Option<usize> {
    match s {
        "x" => Some(0),
        "y" => Some(1),
        "z" => Some(2),
        _ => None,
    }
}

impl CageDef {
    pub fn validate(&self) -> Result<(), String> {
        if self.verts.len() < 3 {
            return Err(format!(
                "verts: a cage needs at least 3 vertices, got {}",
                self.verts.len()
            ));
        }
        if let Some(i) = self
            .verts
            .iter()
            .position(|v| v.iter().any(|c| !c.is_finite()))
        {
            return Err(format!("verts[{i}]: not a finite point"));
        }
        if self.faces.is_empty() {
            return Err("faces: give at least one face (a list of vertex indices)".into());
        }
        let n = self.verts.len() as u32;
        for (k, f) in self.faces.iter().enumerate() {
            if f.len() < 3 {
                return Err(format!(
                    "faces[{k}]: a face needs at least 3 vertices, got {}",
                    f.len()
                ));
            }
            if let Some(i) = f.iter().find(|i| **i >= n) {
                return Err(format!(
                    "faces[{k}]: vertex {i} does not exist (there are {n})"
                ));
            }
            let mut s = f.clone();
            s.sort_unstable();
            s.dedup();
            if s.len() != f.len() {
                return Err(format!("faces[{k}]: a vertex appears twice in {f:?}"));
            }
        }
        // Every edge in at most two faces, used once each way (consistent
        // winding: a face's a -> b is its neighbour's b -> a).
        let mut dir: HashMap<(u32, u32), usize> = HashMap::new();
        for (k, f) in self.faces.iter().enumerate() {
            for i in 0..f.len() {
                let e = (f[i], f[(i + 1) % f.len()]);
                if let Some(o) = dir.insert(e, k) {
                    return Err(format!(
                        "faces[{k}] and faces[{o}] both run {} -> {}: wind every face the same way (counter-clockwise seen from outside)",
                        e.0, e.1
                    ));
                }
            }
        }
        for (k, c) in self.creases.iter().enumerate() {
            let (a, b, w) = (c[0], c[1], c[2]);
            if a.fract() != 0.0 || b.fract() != 0.0 || a < 0.0 || b < 0.0 {
                return Err(format!(
                    "creases[{k}]: [a, b, weight] with vertex indices a and b"
                ));
            }
            let (a, b) = (a as u32, b as u32);
            if !dir.contains_key(&(a, b)) && !dir.contains_key(&(b, a)) {
                return Err(format!(
                    "creases[{k}]: {a} - {b} is not an edge of any face"
                ));
            }
            if !(w.is_finite() && w >= 0.0) {
                return Err(format!(
                    "creases[{k}]: weight must be >= 0 (levels it stays sharp; {SHARP} = always), got {w}"
                ));
            }
        }
        if let Some(m) = &self.mirror {
            let Some(ax) = axis_of(m) else {
                return Err(format!("mirror: x, y or z, got `{m}`"));
            };
            let eps = self.eps();
            if let Some(i) = self.verts.iter().position(|v| v[ax] < -eps) {
                return Err(format!(
                    "mirror {m}: give the + half only; verts[{i}] has {m} = {}",
                    self.verts[i][ax]
                ));
            }
        }
        Ok(())
    }

    fn eps(&self) -> f32 {
        let (lo, hi) = self.verts.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(a, b), v| (a.min(Vec3::from(*v)), b.max(Vec3::from(*v))),
        );
        ((hi - lo).max_element() * 1e-6).max(1e-7)
    }

    pub fn summary(&self) -> String {
        let quads = self.faces.iter().filter(|f| f.len() == 4).count();
        let mut s = format!(
            "{} verts, {} faces ({quads} quads)",
            self.verts.len(),
            self.faces.len()
        );
        if !self.creases.is_empty() {
            s.push_str(&format!(", {} creases", self.creases.len()));
        }
        if let Some(m) = &self.mirror {
            s.push_str(&format!(", mirror {m}"));
        }
        s
    }

    /// The whole cage: the given half and, with `mirror`, its reflection
    /// joined along the plane.
    pub fn cage(&self) -> Cage {
        let creases: Vec<(u32, u32, f32)> = self
            .creases
            .iter()
            .map(|c| (c[0] as u32, c[1] as u32, c[2]))
            .collect();
        let mut out = Cage {
            verts: self.verts.clone(),
            faces: self.faces.clone(),
            creases,
        };
        let Some(ax) = self.mirror.as_deref().and_then(axis_of) else {
            return out;
        };
        let eps = self.eps();
        let n = self.verts.len();
        // Vertices on the plane are shared; the rest get a mirrored twin.
        let mut twin = vec![0u32; n];
        for (i, v) in self.verts.iter().enumerate() {
            if v[ax].abs() <= eps {
                out.verts[i][ax] = 0.0;
                twin[i] = i as u32;
            } else {
                let mut m = *v;
                m[ax] = -m[ax];
                twin[i] = out.verts.len() as u32;
                out.verts.push(m);
            }
        }
        for f in &self.faces {
            out.faces
                .push(f.iter().rev().map(|&i| twin[i as usize]).collect());
        }
        let extra: Vec<(u32, u32, f32)> = out
            .creases
            .iter()
            .map(|&(a, b, w)| (twin[a as usize], twin[b as usize], w))
            .filter(|&(a, b, _)| {
                !out.creases
                    .iter()
                    .any(|&(c, d, _)| (c, d) == (a, b) || (c, d) == (b, a))
            })
            .collect();
        out.creases.extend(extra);
        out
    }
}

/// Splits each polygon into triangles: quads across their shorter
/// diagonal, larger polygons by ear clipping in their own plane.
pub fn triangulate(v: &[Vec3], faces: &[Vec<u32>]) -> Vec<[u32; 3]> {
    let mut out = Vec::with_capacity(faces.len() * 2);
    for f in faces {
        match f.len() {
            3 => out.push([f[0], f[1], f[2]]),
            4 => {
                let p = |k: usize| v[f[k] as usize];
                if p(0).distance_squared(p(2)) <= p(1).distance_squared(p(3)) {
                    out.push([f[0], f[1], f[2]]);
                    out.push([f[0], f[2], f[3]]);
                } else {
                    out.push([f[0], f[1], f[3]]);
                    out.push([f[1], f[2], f[3]]);
                }
            }
            _ => {
                // Project onto the polygon's plane (Newell normal).
                let mut nrm = Vec3::ZERO;
                for i in 0..f.len() {
                    let (a, b) = (v[f[i] as usize], v[f[(i + 1) % f.len()] as usize]);
                    nrm += a.cross(b);
                }
                let nrm = nrm.normalize_or(Vec3::Y);
                let u = nrm.any_orthonormal_vector();
                let w = nrm.cross(u);
                let poly: Vec<[f64; 2]> = f
                    .iter()
                    .map(|&i| {
                        let p = v[i as usize];
                        [p.dot(u) as f64, p.dot(w) as f64]
                    })
                    .collect();
                match manifold_csg::triangulate_polygons(std::slice::from_ref(&poly), 1e-9) {
                    Some(tris) if tris.len() == f.len() - 2 => {
                        for t in tris {
                            out.push(t.map(|k| f[k as usize]));
                        }
                    }
                    _ => {
                        for k in 1..f.len() - 1 {
                            out.push([f[0], f[k], f[k + 1]]);
                        }
                    }
                }
            }
        }
    }
    out
}

/// The `cage` op: the polygons triangulated as the part, the polygons
/// themselves carried along for `smooth subdiv`.
pub fn build(name: &str, def: &CageDef) -> Result<Part, ModelerError> {
    def.validate()
        .map_err(|e| ModelerError::Invalid(format!("cage `{name}`: {e}")))?;
    let cage = def.cage();
    let v: Vec<Vec3> = cage.verts.iter().map(|p| Vec3::from(*p)).collect();
    let mut part = Part::new(name);
    part.verts = cage.verts.clone();
    part.indices = triangulate(&v, &cage.faces).into_iter().flatten().collect();
    crate::uv::crease_normals(&mut part, 30.0);
    crate::uv::ensure_uvs(&mut part, Some("box"))?;
    part.cage = Some(Arc::new(cage));
    Ok(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn cube() -> CageDef {
        let verts = vec![
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
        ];
        let faces = vec![
            vec![0, 3, 2, 1],
            vec![4, 5, 6, 7],
            vec![0, 1, 5, 4],
            vec![2, 3, 7, 6],
            vec![1, 2, 6, 5],
            vec![0, 4, 7, 3],
        ];
        CageDef {
            verts,
            faces,
            ..Default::default()
        }
    }

    fn volume(p: &Part) -> f32 {
        p.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]));
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    #[test]
    fn a_cube_cage_is_a_closed_outward_solid() {
        let p = build("c", &cube()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.tri_count(), 12);
        assert!((volume(&p) - 1.0).abs() < 1e-5, "{}", volume(&p));
        assert!(crate::csg::part_to_manifold(&p).is_ok());
        assert_eq!(p.cage.as_ref().map(|c| c.faces.len()), Some(6));
    }

    #[test]
    fn bad_cages_name_the_face() {
        let mut d = cube();
        d.faces[1] = vec![4, 7, 6, 5];
        let e = d.validate().err().unwrap_or_default();
        assert!(e.contains("wind every face the same way"), "{e}");
        let mut d = cube();
        d.faces[0] = vec![0, 3, 9];
        let e = d.validate().err().unwrap_or_default();
        assert!(e.contains("faces[0]") && e.contains("9"), "{e}");
        let mut d = cube();
        d.creases = vec![[0.0, 6.0, 1.0]];
        let e = d.validate().err().unwrap_or_default();
        assert!(e.contains("not an edge"), "{e}");
    }

    #[test]
    fn a_mirrored_half_cage_joins_along_the_plane() {
        // The +x half of the cube: x from 0 to 0.5, open at x = 0.
        let mut half = cube();
        for v in &mut half.verts {
            v[0] = v[0].max(0.0);
        }
        half.faces.remove(5); // the -x face, now on the plane
        half.mirror = Some("x".into());
        // Vertices on the plane are shared, so the result is closed.
        let p = build("m", &half).unwrap_or_else(|e| panic!("{e}"));
        assert!(crate::csg::part_to_manifold(&p).is_ok());
        let c = p.cage.as_ref().map(|c| (c.verts.len(), c.faces.len()));
        assert_eq!(c, Some((12, 10)));
        let (lo, hi) = p.bounds();
        assert!((lo.x + 0.5).abs() < 1e-6 && (hi.x - 0.5).abs() < 1e-6);
        let mut d = cube();
        d.mirror = Some("x".into());
        let e = d.validate().err().unwrap_or_default();
        assert!(e.contains("+ half"), "{e}");
    }
}
