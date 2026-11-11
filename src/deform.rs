//! `deform`: bends, twists, tapers or noise-displaces a part's mesh, for
//! organic shapes (rocks from spheres, horns, tusks, curved blades, rough
//! cliffs). The input is baked into model space, refined so its edges are no
//! longer than `detail` (a low-poly sphere becomes a lumpy rock, not a spiky
//! one), then every vertex is moved. Positions only move, never tear: a closed
//! solid stays a closed, outward-wound solid (vertices split for UV seams
//! move together), UVs are interpolated through the refinement, and normals
//! are re-creased afterwards.

use crate::material::noise::{Lattice, fractal, gradient};
use crate::{ModelerError, Part, quat_of};
use bevy::math::Vec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Most triangles a deform refines to; `detail` grows to stay under it.
const MAX_TRIS: f64 = 250_000.0;

fn none<T>(v: &Option<T>) -> bool {
    v.is_none()
}

/// The parameters of one `deform` op (see `DeformSpec` for the docs).
/// Unset options are sized from the input when it is evaluated.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Deform {
    /// displace / bend / twist / taper.
    pub mode: String,
    /// Bend/twist/taper: the axis the effect runs along ("x", "y", "z").
    #[serde(default, skip_serializing_if = "none")]
    pub axis: Option<String>,
    /// Bend/twist: total angle over the part's extent along `axis`.
    #[serde(default, skip_serializing_if = "none")]
    pub degrees: Option<f32>,
    /// Bend: the direction the far end curls toward ("x", "-z", ...).
    #[serde(default, skip_serializing_if = "none")]
    pub toward: Option<String>,
    /// Taper: cross-section scale at the far end of `axis`, per cross axis.
    #[serde(default, skip_serializing_if = "none")]
    pub scale_end: Option<[f32; 2]>,
    /// Taper: exponent of the profile (1 linear, 2 = thins late, 0.5 early).
    #[serde(default, skip_serializing_if = "none")]
    pub power: Option<f32>,
    /// Displace: largest offset along the normal, meters.
    #[serde(default, skip_serializing_if = "none")]
    pub amount: Option<f32>,
    /// Displace: feature size of the noise, meters.
    #[serde(default, skip_serializing_if = "none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub octaves: Option<u32>,
    #[serde(default, skip_serializing_if = "none")]
    pub seed: Option<u32>,
    /// Displace: "fbm" (lumps), "ridged" (sharp creases: cliffs, bark) or
    /// "turbulence" (billowy).
    #[serde(default, skip_serializing_if = "none")]
    pub noise: Option<String>,
    /// Target edge length, meters: the input is refined to this first.
    #[serde(default, skip_serializing_if = "none")]
    pub detail: Option<f32>,
}

pub const MODES: &[&str] = &["displace", "bend", "twist", "taper"];

/// "x" / "+y" / "-z" or a direction word (right, left, up, down, front,
/// back) as (axis index, sign).
pub fn parse_dir(s: &str) -> Option<(usize, f32)> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "x" | "+x" | "right" => (0, 1.0),
        "-x" | "left" => (0, -1.0),
        "y" | "+y" | "up" | "top" => (1, 1.0),
        "-y" | "down" | "bottom" => (1, -1.0),
        "z" | "+z" | "front" => (2, 1.0),
        "-z" | "back" => (2, -1.0),
        _ => return None,
    })
}

impl Deform {
    fn axis(&self) -> Result<usize, String> {
        match self.axis.as_deref() {
            None => Ok(1),
            Some(a) => match parse_dir(a) {
                Some((k, _)) => Ok(k),
                None => Err(format!("axis must be x, y or z, got `{a}`")),
            },
        }
    }

    /// Checks the fields that don't depend on the input.
    pub fn validate(&self) -> Result<(), String> {
        if !MODES.contains(&self.mode.as_str()) {
            return Err(format!(
                "mode: must be displace, bend, twist or taper, got `{}`",
                self.mode
            ));
        }
        let a = self.axis()?;
        let pos = |v: Option<f32>, what: &str| match v {
            Some(x) if !x.is_finite() || x <= 0.0 => Err(format!("{what}: must be > 0, got {x}")),
            _ => Ok(()),
        };
        pos(self.detail, "detail")?;
        pos(self.scale, "scale")?;
        pos(self.power, "power")?;
        if let Some(x) = self.amount
            && !x.is_finite()
        {
            return Err(format!("amount: must be a number, got {x}"));
        }
        if let Some(n) = &self.noise
            && !["fbm", "ridged", "turbulence"].contains(&n.as_str())
        {
            return Err(format!(
                "noise: must be fbm, ridged or turbulence, got `{n}`"
            ));
        }
        if let Some(o) = self.octaves
            && !(1..=8).contains(&o)
        {
            return Err(format!("octaves: must be 1..8, got {o}"));
        }
        if let Some(s) = self.scale_end
            && s.iter().any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err(format!("scale_end: must be >= 0, got {s:?}"));
        }
        if let Some(t) = &self.toward {
            match parse_dir(t) {
                None => {
                    return Err(format!(
                        "toward: must be x, -x, y, -y, z or -z (or right/left/up/down/front/back), got `{t}`"
                    ));
                }
                Some((k, _)) if k == a => {
                    return Err(format!(
                        "toward: `{t}` is along the bend axis `{}`; bend toward one of the other two axes",
                        ["x", "y", "z"][a]
                    ));
                }
                _ => {}
            }
        }
        match self.mode.as_str() {
            "bend" | "twist" if self.degrees.is_none() => {
                Err(format!("degrees: {} needs an angle", self.mode))
            }
            "taper" if self.scale_end.is_none() => Err(
                "scale_end: taper needs the far end's scale (a number, or [a, b] per cross axis)"
                    .into(),
            ),
            _ => Ok(()),
        }
    }

    /// The parameters in one line (for `ops` / `describe`).
    pub fn summary(&self) -> String {
        let mut s = self.mode.clone();
        let mut f = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                s.push_str(&format!(" {k}={v}"));
            }
        };
        f("axis", self.axis.clone());
        f("degrees", self.degrees.map(|v| v.to_string()));
        f("toward", self.toward.clone());
        f("scale_end", self.scale_end.map(|v| format!("{v:?}")));
        f("power", self.power.map(|v| v.to_string()));
        f("amount", self.amount.map(|v| v.to_string()));
        f("scale", self.scale.map(|v| v.to_string()));
        f("octaves", self.octaves.map(|v| v.to_string()));
        f("seed", self.seed.map(|v| v.to_string()));
        f("noise", self.noise.clone());
        f("detail", self.detail.map(|v| v.to_string()));
        s
    }
}

/// A deformed copy of `part`, named `name`, in model space (identity
/// transform), with `part`'s material.
pub fn deform(name: &str, part: &Part, def: &Deform) -> Result<Part, ModelerError> {
    def.validate()
        .map_err(|e| ModelerError::Invalid(format!("deform `{name}`: {e}")))?;
    if part.verts.is_empty() {
        return Err(ModelerError::Invalid(format!(
            "deform `{name}`: `{}` has no geometry",
            part.name
        )));
    }
    // Bake to model space first so axes and sizes are the world's.
    let rot = quat_of(part.rotation);
    let (sc, tr) = (Vec3::from(part.scale), Vec3::from(part.translation));
    let world: Vec<Vec3> = part
        .verts
        .iter()
        .map(|v| rot * (Vec3::from(*v) * sc) + tr)
        .collect();
    let (lo, hi) = world.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), p| (a.min(*p), b.max(*p)),
    );
    let size = (hi - lo).max(Vec3::splat(1e-6));
    let axis = def.axis().map_err(ModelerError::Invalid)?;
    let scale = def.scale.unwrap_or(0.45 * size.max_element());
    let detail = def.detail.unwrap_or(match def.mode.as_str() {
        "displace" => (scale / 8.0).min(size.max_element() / 32.0),
        _ => size[axis] / 32.0,
    });

    // Refine: a closed solid goes through Manifold (adaptive, welded, UVs
    // interpolated); an open sheet is split by midpoints.
    let (mut verts, mut uvs, indices, solid) = match crate::csg::part_to_manifold(part) {
        Ok(m) => {
            let area = m.surface_area();
            let d = (detail as f64).max((area / (0.433 * MAX_TRIS)).sqrt());
            let m = m.refine_to_length(d);
            let mesh = m.to_meshgl64();
            let n = mesh.num_prop().max(3);
            let mut verts = Vec::new();
            let mut uvs = Vec::new();
            for v in mesh.vert_properties().chunks(n) {
                verts.push(Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32));
                if n >= 5 {
                    uvs.push([v[3] as f32, v[4] as f32]);
                }
            }
            let idx: Vec<u32> = mesh.tri_verts().iter().map(|&i| i as u32).collect();
            (verts, uvs, idx, true)
        }
        Err(_) => {
            let idx: Vec<u32> = if part.indices.is_empty() {
                (0..(world.len() - world.len() % 3) as u32).collect()
            } else {
                part.indices.clone()
            };
            let uvs = if part.uvs.len() == part.verts.len() {
                part.uvs.clone()
            } else {
                Vec::new()
            };
            let (v, u, i) = subdivide(world, uvs, idx, detail);
            (v, u, i, false)
        }
    };

    match def.mode.as_str() {
        "displace" => displace(&mut verts, &indices, def, scale, size),
        mode => {
            let (lo, hi) = verts.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(a, b), p| (a.min(*p), b.max(*p)),
            );
            let len = (hi[axis] - lo[axis]).max(1e-6);
            let c = (lo + hi) * 0.5;
            let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
            // Cross axes in x, y, z order, as scale_end lists them.
            let (cu, cw) = if u < w { (u, w) } else { (w, u) };
            for p in &mut verts {
                let t = (p[axis] - lo[axis]) / len;
                match mode {
                    "twist" => {
                        let a = def.degrees.unwrap_or(0.0).to_radians() * t;
                        let (s, co) = a.sin_cos();
                        let (x, y) = (p[cu] - c[cu], p[cw] - c[cw]);
                        p[cu] = c[cu] + x * co - y * s;
                        p[cw] = c[cw] + x * s + y * co;
                    }
                    "taper" => {
                        let e = def.scale_end.unwrap_or([1.0; 2]).map(|v| v.max(1e-3));
                        let k = t.powf(def.power.unwrap_or(1.0));
                        p[cu] = c[cu] + (p[cu] - c[cu]) * (1.0 + (e[0] - 1.0) * k);
                        p[cw] = c[cw] + (p[cw] - c[cw]) * (1.0 + (e[1] - 1.0) * k);
                    }
                    _ => {
                        // bend: the axis line through the centre becomes an
                        // arc of the same length curling toward `toward`.
                        let theta = def.degrees.unwrap_or(0.0).to_radians();
                        if theta.abs() < 1e-6 {
                            continue;
                        }
                        let (d, sign) = def
                            .toward
                            .as_deref()
                            .and_then(parse_dir)
                            .unwrap_or(if axis == 0 { (1, 1.0) } else { (0, 1.0) });
                        let r = len / theta;
                        let phi = t * theta;
                        let off = (p[d] - c[d]) * sign;
                        let (s, co) = phi.sin_cos();
                        p[axis] = lo[axis] + (r - off) * s;
                        p[d] = c[d] + sign * (r - (r - off) * co);
                    }
                }
            }
        }
    }

    let mut out = Part::new(name);
    out.material = part.material.clone();
    out.verts = verts.iter().map(|v| v.to_array()).collect();
    out.indices = indices;
    if uvs.len() == out.verts.len() {
        out.uvs = std::mem::take(&mut uvs);
    }
    // Displaced surfaces are smooth at the detail scale: crease only real
    // ridges. Bent/twisted boxes keep their edges hard.
    let crease = if def.mode == "displace" { 60.0 } else { 40.0 };
    crate::uv::crease_normals(&mut out, crease);
    if out.uvs.len() != out.verts.len() {
        out.uvs.clear();
        let _ = crate::uv::ensure_uvs(&mut out, Some("box"));
    }
    if solid && out.indices.len() / 3 == 0 {
        return Err(ModelerError::Csg(format!(
            "deform `{name}` produced nothing"
        )));
    }
    Ok(out)
}

/// Grid key for welding coincident positions (seam-split vertices).
fn key(p: Vec3, inv: f32) -> [i64; 3] {
    [
        (p.x * inv).round() as i64,
        (p.y * inv).round() as i64,
        (p.z * inv).round() as i64,
    ]
}

/// Moves each vertex along its (welded, area-weighted) normal by fractal
/// noise of its position. Coincident vertices get the same normal and noise,
/// so seams never open.
fn displace(verts: &mut [Vec3], indices: &[u32], def: &Deform, scale: f32, size: Vec3) {
    let amount = def.amount.unwrap_or(0.12 * size.min_element());
    let octaves = def.octaves.unwrap_or(4);
    let seed = def.seed.unwrap_or(0);
    let mode = def.noise.as_deref().unwrap_or("fbm");
    let inv = 1.0 / (size.max_element() * 1e-6).clamp(1e-7, 1e-5);
    let mut normals: HashMap<[i64; 3], Vec3> = HashMap::new();
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| verts[i as usize]);
        let n = (b - a).cross(c - a); // length = 2 x area
        for p in [a, b, c] {
            *normals.entry(key(p, inv)).or_insert(Vec3::ZERO) += n;
        }
    }
    // Keep the noise lattice off integer coordinates (the noise is 0.5 at
    // every lattice point) and vary it per seed.
    let shift = Vec3::new(17.13, 31.71, 7.37) * scale + Vec3::splat(seed as f32 * 0.618 * scale);
    let lattice = Lattice::new(scale, 0.0);
    let base = |p: [f32; 3], l: Lattice, s: u32| gradient(p, 3, l, s);
    for v in verts.iter_mut() {
        let n = normals
            .get(&key(*v, inv))
            .copied()
            .unwrap_or(Vec3::ZERO)
            .normalize_or_zero();
        let q = (*v + shift).to_array();
        let f = fractal(base, q, lattice, seed, octaves, 2.0, 0.5, mode);
        // fbm of gradient noise sits near 0.5 (+-0.25 is already rare), so
        // spread it to roughly -1..1; ridged/turbulence span 0..1.
        let h = match mode {
            "fbm" => ((f - 0.5) * 3.5).clamp(-1.0, 1.0),
            _ => (f - 0.5) * 2.0,
        };
        *v += n * (h * amount);
    }
}

/// Midpoint subdivision (1 -> 4) until no edge is longer than `detail`, for
/// open sheets Manifold can't refine. Stops before passing [`MAX_TRIS`].
fn subdivide(
    mut verts: Vec<Vec3>,
    mut uvs: Vec<[f32; 2]>,
    mut idx: Vec<u32>,
    detail: f32,
) -> (Vec<Vec3>, Vec<[f32; 2]>, Vec<u32>) {
    let has_uv = uvs.len() == verts.len();
    for _ in 0..8 {
        let longest = idx
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|t| {
                let [a, b, c] = t.map(|i| verts[i as usize]);
                [a.distance(b), b.distance(c), c.distance(a)]
            })
            .fold(0.0f32, f32::max);
        if longest <= detail || (idx.len() / 3 * 4) as f64 > MAX_TRIS {
            break;
        }
        let mut mids: HashMap<(u32, u32), u32> = HashMap::new();
        let mut next = Vec::with_capacity(idx.len() * 4);
        for t in idx.as_chunks::<3>().0 {
            let mut mid = |a: u32, b: u32| -> u32 {
                *mids.entry((a.min(b), a.max(b))).or_insert_with(|| {
                    verts.push((verts[a as usize] + verts[b as usize]) * 0.5);
                    if has_uv {
                        let (ua, ub) = (uvs[a as usize], uvs[b as usize]);
                        uvs.push([(ua[0] + ub[0]) * 0.5, (ua[1] + ub[1]) * 0.5]);
                    }
                    (verts.len() - 1) as u32
                })
            };
            let [a, b, c] = *t;
            let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
            next.extend([a, ab, ca, ab, b, bc, ca, bc, c, ab, bc, ca]);
        }
        idx = next;
    }
    if !has_uv {
        uvs.clear();
    }
    (verts, uvs, idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere() -> Part {
        crate::primitives::primitive(crate::PrimitiveKind::Sphere, [0.5, 0.0, 0.0], "ball")
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn check_solid(p: &Part) {
        assert_eq!(crate::verify::open_edge_count(p), 0, "open edges");
        let m = crate::csg::part_to_manifold(p).unwrap_or_else(|e| panic!("{e}"));
        assert!(m.volume() > 0.0, "outward wound");
    }

    #[test]
    fn displaced_sphere_is_a_closed_lumpy_solid() {
        let def = Deform {
            mode: "displace".into(),
            amount: Some(0.08),
            seed: Some(3),
            ..Default::default()
        };
        let p = deform("rock", &sphere(), &def).unwrap_or_else(|e| panic!("{e}"));
        check_solid(&p);
        let radii: Vec<f32> = p.verts.iter().map(|v| Vec3::from(*v).length()).collect();
        let (lo, hi) = radii
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), r| (a.min(*r), b.max(*r)));
        assert!(hi - lo > 0.05, "lumps: radius {lo}..{hi}");
        assert!(hi < 0.5 + 0.08 + 1e-3 && lo > 0.5 - 0.08 - 1e-3);
    }

    #[test]
    fn deform_is_deterministic_and_seeded() {
        let def = |seed| Deform {
            mode: "displace".into(),
            seed: Some(seed),
            ..Default::default()
        };
        let a = deform("r", &sphere(), &def(1)).unwrap_or_else(|e| panic!("{e}"));
        let b = deform("r", &sphere(), &def(1)).unwrap_or_else(|e| panic!("{e}"));
        let c = deform("r", &sphere(), &def(2)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.verts, b.verts);
        assert_ne!(a.verts, c.verts);
    }

    fn bar() -> Part {
        let mut p = crate::primitives::primitive(crate::PrimitiveKind::Box, [0.1, 1.0, 0.1], "bar")
            .unwrap_or_else(|e| panic!("{e}"));
        p.translation = [0.0, 0.5, 0.0];
        p
    }

    #[test]
    fn twist_turns_the_top_and_keeps_the_bottom() {
        let def = Deform {
            mode: "twist".into(),
            degrees: Some(90.0),
            ..Default::default()
        };
        let p = deform("tw", &bar(), &def).unwrap_or_else(|e| panic!("{e}"));
        check_solid(&p);
        let (lo, hi) = p.bounds();
        assert!((hi.y - 1.0).abs() < 1e-4 && lo.y.abs() < 1e-4);
        // Halfway up the bar is turned 45°: its corner reaches 0.05·√2.
        let mid = p
            .verts
            .iter()
            .filter(|v| (v[1] - 0.5).abs() < 0.01)
            .map(|v| v[0].abs())
            .fold(0.0f32, f32::max);
        assert!((mid - 0.05 * 2f32.sqrt()).abs() < 2e-3, "{mid}");
    }

    #[test]
    fn taper_shrinks_the_far_end() {
        let def = Deform {
            mode: "taper".into(),
            scale_end: Some([0.2, 0.2]),
            ..Default::default()
        };
        let p = deform("tp", &bar(), &def).unwrap_or_else(|e| panic!("{e}"));
        check_solid(&p);
        let top = p
            .verts
            .iter()
            .filter(|v| v[1] > 0.999)
            .map(|v| v[0].abs())
            .fold(0.0f32, f32::max);
        assert!((top - 0.01).abs() < 1e-4, "{top}");
    }

    #[test]
    fn bend_curls_the_far_end_toward_the_direction() {
        let def = Deform {
            mode: "bend".into(),
            degrees: Some(90.0),
            toward: Some("-z".into()),
            ..Default::default()
        };
        let p = deform("bn", &bar(), &def).unwrap_or_else(|e| panic!("{e}"));
        check_solid(&p);
        // A quarter circle of length 1: radius 2/π, the tip at y ≈ 0.64,
        // z ≈ -0.64, its face turned to -z.
        let r = 2.0 / std::f32::consts::PI;
        let (lo, hi) = p.bounds();
        assert!((hi.y - (r + 0.05)).abs() < 0.01, "{hi:?}");
        assert!((lo.z + r).abs() < 0.06, "{lo:?}");
        assert!(lo.y.abs() < 1e-4);
    }

    #[test]
    fn sheets_are_subdivided_and_bent() {
        let p = crate::primitives::primitive(crate::PrimitiveKind::Plane, [1.0, 1.0, 0.0], "sheet")
            .unwrap_or_else(|e| panic!("{e}"));
        // Bend along x toward the sheet's normal (its thinnest axis).
        let (lo, hi) = p.bounds();
        let d = hi - lo;
        let n = if d.y < d.z { 1 } else { 2 };
        let def = Deform {
            mode: "bend".into(),
            axis: Some("x".into()),
            degrees: Some(60.0),
            toward: Some(["x", "y", "z"][n].into()),
            ..Default::default()
        };
        let out = deform("s", &p, &def).unwrap_or_else(|e| panic!("{e}"));
        assert!(out.indices.len() > p.indices.len() * 16);
        let (lo, hi) = out.bounds();
        assert!(hi[n] - lo[n] > 0.1, "curved: {lo:?} {hi:?}");
    }

    #[test]
    fn validation_names_the_field() {
        let bad = Deform {
            mode: "bend".into(),
            ..Default::default()
        };
        assert!(bad.validate().is_err_and(|e| e.starts_with("degrees")));
        let bad = Deform {
            mode: "bend".into(),
            degrees: Some(10.0),
            toward: Some("y".into()),
            ..Default::default()
        };
        assert!(bad.validate().is_err_and(|e| e.starts_with("toward")));
        let bad = Deform {
            mode: "melt".into(),
            ..Default::default()
        };
        assert!(bad.validate().is_err_and(|e| e.starts_with("mode")));
    }
}
