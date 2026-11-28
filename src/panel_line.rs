//! `panel_line`: a line drawn in an orthographic view, projected onto a
//! part along the view and cut into it as a groove (shut lines, panel
//! seams), or, with `fill`, a closed outline drawn in the view turned into
//! a cutter that reaches just under the surface (a window or lamp outline
//! drawn where it is seen, for `split` or a `boolean`).
//!
//! View coordinates are the world's two axes in that view, in meters:
//! right / left `[z, y]`, front / back `[x, y]`, top / bottom `[x, z]`
//! (the same as `silhouette`'s side / front / top outlines). Rays come
//! from the viewer: right looks along -x, left +x, front -z, back +z, top
//! -y, bottom +y.

use bevy::math::Vec3;
use manifold_csg::{CrossSection, Manifold};
use serde::{Deserialize, Serialize};

use crate::organic::{TriGrid, World};
use crate::{ModelerError, Part};

/// The `panel_line` op's settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PanelLineDef {
    pub input: String,
    pub view: String,
    pub points: Vec<[f32; 2]>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub smooth: bool,
    /// Groove width (meters).
    #[serde(default)]
    pub width: f32,
    /// Groove depth under the surface, or with `fill` how far under the
    /// deepest surface point the cutter reaches (meters).
    pub depth: f32,
    /// false: the output is the cutter itself (hide it), not the cut part.
    #[serde(
        default = "crate::loft::yes",
        skip_serializing_if = "crate::loft::is_yes"
    )]
    pub cut: bool,
    /// A closed outline's whole area as one cutter (a prism along the
    /// view) instead of a groove along the line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fill: bool,
}

pub const VIEWS: &[&str] = &["right", "left", "front", "back", "top", "bottom"];

/// The view's (u, v, toward-viewer) axes as world unit vectors.
fn axes(view: &str) -> Option<(Vec3, Vec3, Vec3)> {
    Some(match view {
        "right" => (Vec3::Z, Vec3::Y, Vec3::X),
        "left" => (Vec3::Z, Vec3::Y, Vec3::NEG_X),
        "front" => (Vec3::X, Vec3::Y, Vec3::Z),
        "back" => (Vec3::X, Vec3::Y, Vec3::NEG_Z),
        "top" => (Vec3::X, Vec3::Z, Vec3::Y),
        "bottom" => (Vec3::X, Vec3::Z, Vec3::NEG_Y),
        _ => return None,
    })
}

impl PanelLineDef {
    pub fn validate(&self) -> Result<(), String> {
        if axes(&self.view).is_none() {
            return Err(format!(
                "view: one of {}, got `{}`",
                VIEWS.join(", "),
                self.view
            ));
        }
        let closed = self.closed || self.fill;
        let need = if closed { 3 } else { 2 };
        if self.points.len() < need {
            return Err(format!(
                "points: at least {need} [u, v] points, got {}",
                self.points.len()
            ));
        }
        if self.points.iter().flatten().any(|c| !c.is_finite()) {
            return Err("points: every coordinate must be a number".into());
        }
        if !(self.depth.is_finite() && self.depth > 0.0) {
            return Err(format!("depth: must be > 0 meters, got {}", self.depth));
        }
        if self.fill {
            if self.width != 0.0 {
                return Err(
                    "width: a filled outline has no width (drop it, or fill: false for a groove)"
                        .into(),
                );
            }
        } else if !(self.width.is_finite() && self.width > 0.0) {
            return Err(format!(
                "width: the groove's width must be > 0 meters, got {}",
                self.width
            ));
        }
        Ok(())
    }

    pub fn summary(&self) -> String {
        format!(
            "<- `{}` view={} points={}{}{}{} depth={}{}",
            self.input,
            self.view,
            self.points.len(),
            if self.closed { " closed" } else { "" },
            if self.smooth { " smooth" } else { "" },
            if self.fill {
                " fill".to_string()
            } else {
                format!(" width={}", self.width)
            },
            self.depth,
            if self.cut { "" } else { " (cutter only)" }
        )
    }
}

/// The input's triangles in world space and a grid to cast rays through.
fn target(part: &Part) -> (World, TriGrid, Vec3, Vec3) {
    let v: Vec<Vec3> = (0..part.verts.len()).map(|i| part.world_vert(i)).collect();
    let n = v.len();
    let idx: Vec<u32> = if part.indices.is_empty() {
        (0..(n - n % 3) as u32).collect()
    } else {
        part.indices.clone()
    };
    let t: Vec<[u32; 3]> = idx
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|t| t.iter().all(|&i| (i as usize) < n))
        .copied()
        .collect();
    let (lo, hi) = v.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), p| (a.min(*p), b.max(*p)),
    );
    let w = World::raw(v, t);
    let g = TriGrid::new(&w);
    (w, g, lo, hi)
}

/// The first surface hit seen from the viewer at view point (u, v): the
/// point and the face normal turned toward the viewer.
fn hit(
    w: &World,
    g: &TriGrid,
    (ua, va, wa): (Vec3, Vec3, Vec3),
    (lo, hi): (Vec3, Vec3),
    uv: [f32; 2],
) -> Option<(Vec3, Vec3)> {
    let pad = (hi - lo).max_element() * 0.01 + 1e-3;
    // Start just outside the box on the viewer's side.
    let top = [lo, hi].iter().map(|b| b.dot(wa)).fold(f32::MIN, f32::max) + pad;
    let o = ua * uv[0] + va * uv[1] + wa * top;
    let span = (hi - lo).length() + 2.0 * pad;
    let (t, tri) = g.ray_hit(w, o, -wa, 0.0, span, u32::MAX)?;
    let [a, b, c] = w.t[tri as usize].map(|i| w.v[i as usize]);
    let mut n = (b - a).cross(c - a).normalize_or_zero();
    if n.dot(wa) < 0.0 {
        n = -n;
    }
    Some((o - wa * t, n))
}

/// The line's sample points in view coordinates.
fn samples(def: &PanelLineDef, step: f32) -> Vec<[f32; 2]> {
    let closed = def.closed || def.fill;
    let ctrl: Vec<[f32; 3]> = def.points.iter().map(|p| [p[0], p[1], 0.0]).collect();
    let path = crate::loft::sample_path(&ctrl, closed, def.smooth, 0);
    let mut out: Vec<[f32; 2]> = Vec::new();
    let m = path.len();
    let segs = if closed { m } else { m - 1 };
    for i in 0..segs {
        let (a, b) = (Vec3::from(path[i]), Vec3::from(path[(i + 1) % m]));
        let k = ((a.distance(b) / step).ceil() as usize).max(1);
        for j in 0..k {
            let p = a.lerp(b, j as f32 / k as f32);
            out.push([p.x, p.y]);
        }
    }
    if !closed {
        out.push([path[m - 1][0], path[m - 1][1]]);
    }
    out
}

/// A closed solid swept along the surface points: a rectangle `width`
/// across, from `out` above the surface to `depth` below it, square to
/// the surface normal at every point.
fn groove(pts: &[(Vec3, Vec3)], closed: bool, width: f32, depth: f32) -> Part {
    let n = pts.len();
    let out = (width * 1.5).max(depth).max(0.002);
    let mut verts: Vec<[f32; 3]> = Vec::with_capacity(n * 4);
    for i in 0..n {
        let (prev, next) = if closed {
            (pts[(i + n - 1) % n].0, pts[(i + 1) % n].0)
        } else {
            (pts[i.saturating_sub(1)].0, pts[(i + 1).min(n - 1)].0)
        };
        let t = (next - prev).normalize_or(Vec3::X);
        let (p, nrm) = pts[i];
        let nrm = (nrm - t * t.dot(nrm)).normalize_or(nrm);
        let s = nrm.cross(t).normalize_or_zero() * (width * 0.5);
        for c in [
            p + s + nrm * out,
            p - s + nrm * out,
            p - s - nrm * depth,
            p + s - nrm * depth,
        ] {
            verts.push(c.to_array());
        }
    }
    let mut idx: Vec<u32> = Vec::new();
    let rings = if closed { n } else { n - 1 };
    for i in 0..rings {
        let (a, b) = (i as u32 * 4, ((i + 1) % n) as u32 * 4);
        for k in 0..4u32 {
            let (k1, k2) = (k, (k + 1) % 4);
            idx.extend([a + k1, b + k1, b + k2, a + k1, b + k2, a + k2]);
        }
    }
    if !closed {
        let e = (n as u32 - 1) * 4;
        idx.extend([0, 1, 2, 0, 2, 3, e, e + 2, e + 1, e, e + 3, e + 2]);
    }
    let mut p = Part::new("groove");
    p.verts = verts;
    p.indices = idx;
    // Wound inside out? Flip.
    let vol: f32 = p
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]));
            a.dot(b.cross(c))
        })
        .sum();
    if vol < 0.0 {
        for t in p.indices.as_chunks_mut::<3>().0 {
            t.swap(1, 2);
        }
    }
    p
}

/// The `panel_line` op: the input with the groove (or filled outline) cut
/// out, or with `cut: false` the cutter itself.
pub fn build(name: &str, part: &Part, def: &PanelLineDef) -> Result<Part, ModelerError> {
    let err = |s: String| ModelerError::Csg(format!("panel_line `{name}`: {s}"));
    def.validate().map_err(err)?;
    let Some(ax) = axes(&def.view) else {
        return Err(err("bad view".into()));
    };
    let (w, g, lo, hi) = target(part);
    if w.t.is_empty() {
        return Err(err(format!("`{}` has no triangles", part.name)));
    }
    let size = (hi - lo).max_element();
    let cutter = if def.fill {
        // A prism along the view: from above the outline's highest surface
        // point to `depth` under its deepest one (sampled inside too).
        let ring: Vec<[f64; 2]> = samples(def, size / 200.0)
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64])
            .collect();
        let cs = CrossSection::from_polygons(std::slice::from_ref(&ring));
        let area = cs.area();
        if area.abs() < 1e-12 {
            return Err(err("the outline has no area".into()));
        }
        let (mut near, mut far) = (f32::MIN, f32::MAX);
        let (blo, bhi) = ring
            .iter()
            .fold(([f64::MAX; 2], [f64::MIN; 2]), |(a, b), p| {
                (
                    [a[0].min(p[0]), a[1].min(p[1])],
                    [b[0].max(p[0]), b[1].max(p[1])],
                )
            });
        let k = 24;
        let mut any = false;
        for i in 0..=k {
            for j in 0..=k {
                let uv = [
                    (blo[0] + (bhi[0] - blo[0]) * i as f64 / k as f64) as f32,
                    (blo[1] + (bhi[1] - blo[1]) * j as f64 / k as f64) as f32,
                ];
                if !inside(&ring, uv) {
                    continue;
                }
                if let Some((p, _)) = hit(&w, &g, ax, (lo, hi), uv) {
                    near = near.max(p.dot(ax.2));
                    far = far.min(p.dot(ax.2));
                    any = true;
                }
            }
        }
        for uv in &ring {
            if let Some((p, _)) = hit(&w, &g, ax, (lo, hi), [uv[0] as f32, uv[1] as f32]) {
                near = near.max(p.dot(ax.2));
                far = far.min(p.dot(ax.2));
                any = true;
            }
        }
        if !any {
            return Err(err(format!(
                "the outline misses `{}` seen from the {}",
                part.name, def.view
            )));
        }
        let (bottom, top) = (far - def.depth, near + def.depth.max(size * 0.01));
        let prism = Manifold::extrude(&cs, f64::from(top - bottom));
        let mut p = crate::csg::manifold_to_part(&prism, name, part.material.clone());
        let flip = ax.0.cross(ax.1).dot(ax.2) < 0.0;
        for v in &mut p.verts {
            let q = ax.0 * v[0] + ax.1 * v[1] + ax.2 * (v[2] + bottom);
            *v = q.to_array();
        }
        if flip {
            for t in p.indices.as_chunks_mut::<3>().0 {
                t.swap(1, 2);
            }
        }
        p.normals.clear();
        p
    } else {
        let step = (def.width * 0.75).clamp(0.0005, 0.02).min(size / 50.0);
        let uvs = samples(def, step);
        let mut pts = Vec::with_capacity(uvs.len());
        for (i, uv) in uvs.iter().enumerate() {
            let Some(h) = hit(&w, &g, ax, (lo, hi), *uv) else {
                return Err(err(format!(
                    "the line leaves `{}` seen from the {} near [{:.4}, {:.4}] (sample {i})",
                    part.name, def.view, uv[0], uv[1]
                )));
            };
            pts.push(h);
        }
        groove(&pts, def.closed, def.width, def.depth)
    };
    let cut_m = crate::csg::part_to_manifold(&cutter).map_err(|e| {
        err(format!(
            "the cutter is not a closed solid ({e}): the line bends tighter than its width somewhere"
        ))
    })?;
    if !def.cut {
        let mut c = crate::csg::manifold_to_part(&cut_m, name, part.material.clone());
        c.visible = true;
        return Ok(c);
    }
    let a = crate::csg::part_to_manifold(part)?;
    let out = a.difference(&cut_m);
    out.status().map_err(|e| err(format!("failed: {e}")))?;
    let mut p = crate::csg::manifold_to_part(&out, name, part.material.clone());
    p.visible = true;
    Ok(p)
}

/// Even-odd point in polygon.
fn inside(ring: &[[f64; 2]], p: [f32; 2]) -> bool {
    let (x, y) = (p[0] as f64, p[1] as f64);
    let mut c = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + n - 1) % n]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Part {
        let mut b =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0, 1.0, 2.0], "body")
                .unwrap_or_else(|e| panic!("{e}"));
        b.translation = [0.0, 0.5, 0.0];
        b
    }

    fn line(points: Vec<[f32; 2]>) -> PanelLineDef {
        PanelLineDef {
            input: "body".into(),
            view: "right".into(),
            points,
            width: 0.008,
            depth: 0.004,
            cut: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_groove_is_cut_into_the_side_seen() {
        let b = body();
        let v0 = crate::csg::part_to_manifold(&b)
            .map(|m| m.volume())
            .unwrap_or(0.0);
        let def = line(vec![[-0.5, 0.2], [0.5, 0.8]]);
        let p = build("g", &b, &def).unwrap_or_else(|e| panic!("{e}"));
        let m = crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}"));
        // About length x width x depth removed, from the +x face only.
        let len = (1.0f64 + 0.36).sqrt();
        let want = len * 0.008 * 0.004;
        let lost = v0 - m.volume();
        assert!((lost - want).abs() < want * 0.2, "{lost} vs {want}");
        let (lo, _) = p.bounds();
        assert!((lo.x + 0.5).abs() < 1e-5, "the far side is untouched");
        // A line off the part names where it leaves.
        let e = build("g", &b, &line(vec![[0.0, 0.5], [3.0, 0.5]])).err();
        assert!(e.is_some_and(|e| e.to_string().contains("leaves")));
    }

    #[test]
    fn a_filled_outline_is_a_cutter_just_under_the_surface() {
        let b = body();
        let mut def = line(vec![[-0.3, 0.4], [0.3, 0.4], [0.3, 0.7], [-0.3, 0.7]]);
        def.fill = true;
        def.width = 0.0;
        def.depth = 0.01;
        def.cut = false;
        let c = build("win", &b, &def).unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = c.bounds();
        // Seen from the right: it spans z and y of the outline and reaches
        // 1 cm under the +x face, no further.
        assert!(
            (lo.z + 0.3).abs() < 1e-4 && (hi.z - 0.3).abs() < 1e-4,
            "{lo} {hi}"
        );
        assert!(
            (lo.y - 0.4).abs() < 1e-4 && (hi.y - 0.7).abs() < 1e-4,
            "{lo} {hi}"
        );
        assert!((lo.x - 0.49).abs() < 1e-4 && hi.x > 0.5, "{lo} {hi}");
        assert!(crate::csg::part_to_manifold(&c).is_ok());
    }

    #[test]
    fn a_closed_smooth_loop_follows_a_curved_body() {
        let b = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0, 1.0, 2.0], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let sm = crate::smooth::Smooth {
            mode: "subdiv".into(),
            levels: Some(4),
            ..Default::default()
        };
        let body = crate::smooth::smooth("body", &b, &sm).unwrap_or_else(|e| panic!("{e}"));
        let mut def = line(vec![[-0.4, -0.2], [0.4, -0.2], [0.4, 0.2], [-0.4, 0.2]]);
        def.closed = true;
        def.smooth = true;
        let p = build("door", &body, &def).unwrap_or_else(|e| panic!("{e}"));
        let m = crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}"));
        let v0 = crate::csg::part_to_manifold(&body)
            .map(|m| m.volume())
            .unwrap_or(0.0);
        let lost = v0 - m.volume();
        // About the loop's length (2.4 m round the rectangle) x width x depth.
        let want = 2.4 * 0.008 * 0.004;
        assert!((lost - want).abs() < 0.3 * want, "{lost} vs {want}");
    }
}
