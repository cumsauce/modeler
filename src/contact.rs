//! Geometric contact between parts: who actually touches whom (not just
//! whose bounding boxes do), where, and whether a part is held up from below.
//! Backs `verify` `grounded` / `supported` / `rests`.

use crate::{Model, Part, Vec3};
use manifold_csg::Manifold;

/// How two parts meet.
#[derive(Debug, Clone)]
pub enum Contact {
    /// They share volume (a joint, a glued or sunk part): attached.
    Overlap,
    /// Surfaces within the tolerance: the world points where they meet.
    Surface(Vec<Vec3>),
}

/// The touching tolerance for two parts: 0.2% of the smaller part's
/// diagonal, between 10 µm and 1 mm. A pencil and a house get different
/// slack, so a 0.8 mm gap under a laptop screen counts as a gap.
pub fn tolerance(a: &Part, b: &Part) -> f32 {
    let d = |p: &Part| {
        let (lo, hi) = p.bounds();
        (hi - lo).length()
    };
    (d(a).min(d(b)) * 0.002).clamp(1e-5, 1e-3)
}

fn ground_tolerance(p: &Part) -> f32 {
    let (lo, hi) = p.bounds();
    ((hi - lo).length() * 0.002).clamp(1e-5, 1e-3)
}

pub(crate) fn world_tris(p: &Part) -> Vec<[Vec3; 3]> {
    let w: Vec<Vec3> = (0..p.verts.len()).map(|i| p.world_vert(i)).collect();
    p.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [w[t[0] as usize], w[t[1] as usize], w[t[2] as usize]])
        .collect()
}

/// Closest point on triangle `t` to `p` (Ericson, Real-Time Collision Detection).
pub(crate) fn closest_on_tri(p: Vec3, t: &[Vec3; 3]) -> Vec3 {
    let [a, b, c] = *t;
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

fn boxes_near(a: &Part, b: &Part, tol: f32) -> bool {
    let (alo, ahi) = a.bounds();
    let (blo, bhi) = b.bounds();
    (0..3).all(|k| alo[k] <= bhi[k] + tol && blo[k] <= ahi[k] + tol)
}

/// Vertices of `a` within `tol` of `b`'s surface.
fn near_points(a: &Part, b_tris: &[[Vec3; 3]], tol: f32) -> Vec<Vec3> {
    let tol2 = tol * tol;
    (0..a.verts.len())
        .map(|i| a.world_vert(i))
        .filter(|&v| {
            b_tris
                .iter()
                .any(|t| (closest_on_tri(v, t) - v).length_squared() <= tol2)
        })
        .collect()
}

/// A part plus its solid (when it is a closed mesh), built once per check.
pub struct Body<'a> {
    pub part: &'a Part,
    solid: Option<Manifold>,
    tris: Vec<[Vec3; 3]>,
}

impl<'a> Body<'a> {
    pub fn new(part: &'a Part) -> Self {
        Self {
            part,
            solid: crate::csg::part_to_manifold(part).ok(),
            tris: world_tris(part),
        }
    }
}

/// How `a` and `b` meet, if they do.
pub fn contact(a: &Body, b: &Body) -> Option<Contact> {
    let tol = tolerance(a.part, b.part);
    if !boxes_near(a.part, b.part, tol) {
        return None;
    }
    if let (Some(ma), Some(mb)) = (&a.solid, &b.solid) {
        let ov = ma.intersection(mb).volume().abs();
        let small = ma.volume().abs().min(mb.volume().abs()).max(1e-30);
        if ov > small * 1e-6 {
            return Some(Contact::Overlap);
        }
        // Manifold's min_gap is exact for the solids.
        if ma.min_gap(mb, f64::from(tol) * 2.0) > f64::from(tol) {
            return None;
        }
    }
    let mut pts = near_points(a.part, &b.tris, tol);
    pts.extend(near_points(b.part, &a.tris, tol));
    if pts.is_empty() {
        // Surfaces meet between vertices (two curved faces, a face on an
        // edge): the solids say they touch, so take the closest vertex pair
        // region as the contact point.
        let closest = |p: &Part, tris: &[[Vec3; 3]]| {
            (0..p.verts.len())
                .map(|i| p.world_vert(i))
                .map(|v| {
                    let d = tris
                        .iter()
                        .map(|t| (closest_on_tri(v, t) - v).length_squared())
                        .fold(f32::MAX, f32::min);
                    (d, v)
                })
                .min_by(|x, y| x.0.total_cmp(&y.0))
        };
        let (Some((da, va)), Some((db, vb))) = (closest(a.part, &b.tris), closest(b.part, &a.tris))
        else {
            return None;
        };
        let near = if da <= db { va } else { vb };
        // The solids already proved contact within `tol`; only sheets
        // (no solid) need the vertices themselves to be close.
        if a.solid.is_none() || b.solid.is_none() {
            return None;
        }
        pts.push(near);
    }
    Some(Contact::Surface(pts))
}

/// Two visible parts sharing volume.
#[derive(Debug, Clone)]
pub struct Overlap {
    pub a: String,
    pub b: String,
    /// Shared volume, m³.
    pub volume: f64,
    /// Share of the smaller part's volume inside the other (0..1).
    pub fraction: f64,
    /// Centre of the shared region's box.
    pub at: [f64; 3],
}

/// Every pair of visible closed parts that share volume, deepest first.
pub fn overlaps(model: &Model) -> Vec<Overlap> {
    let bodies: Vec<Body> = visible(model).map(Body::new).collect();
    let mut out = Vec::new();
    for (i, a) in bodies.iter().enumerate() {
        for b in &bodies[i + 1..] {
            if let Some(o) = overlap_between(a, b) {
                out.push(o);
            }
        }
    }
    out.sort_by(|x, y| y.fraction.total_cmp(&x.fraction));
    out
}

/// The volume `a` and `b` share, if any.
pub fn overlap_between(a: &Body, b: &Body) -> Option<Overlap> {
    if !boxes_near(a.part, b.part, 0.0) {
        return None;
    }
    let (Some(ma), Some(mb)) = (&a.solid, &b.solid) else {
        return None;
    };
    let inter = ma.intersection(mb);
    let ov = inter.volume().abs();
    let small = ma.volume().abs().min(mb.volume().abs()).max(1e-30);
    if ov <= small * 1e-6 {
        return None;
    }
    let at = inter.bounding_box().map_or([0.0; 3], |bb| {
        let (lo, hi) = (bb.min(), bb.max());
        [
            (lo[0] + hi[0]) * 0.5,
            (lo[1] + hi[1]) * 0.5,
            (lo[2] + hi[2]) * 0.5,
        ]
    });
    Some(Overlap {
        a: a.part.name.clone(),
        b: b.part.name.clone(),
        volume: ov,
        fraction: (ov / small).min(1.0),
        at,
    })
}

impl Overlap {
    pub fn describe(&self) -> String {
        format!(
            "`{}` / `{}`: {:.0}% of the smaller inside the other ({:.3e} m³ shared, centred at [{:.3}, {:.3}, {:.3}])",
            self.a,
            self.b,
            self.fraction * 100.0,
            self.volume,
            self.at[0],
            self.at[1],
            self.at[2]
        )
    }

    /// Whether an `[x, y]` allow pair (`*` patterns, either order) covers it.
    pub fn allowed_by(&self, allow: &[[String; 2]]) -> bool {
        let m = crate::name_matches;
        allow
            .iter()
            .any(|[x, y]| (m(x, &self.a) && m(y, &self.b)) || (m(x, &self.b) && m(y, &self.a)))
    }
}

/// Overlaps split into collisions (more than `max` of the smaller part
/// buried, and not allowed) and joints (shallow or allowed).
pub fn collisions(model: &Model, allow: &[[String; 2]], max: f64) -> (Vec<Overlap>, Vec<Overlap>) {
    overlaps(model)
        .into_iter()
        .partition(|o| o.fraction > max && !o.allowed_by(allow))
}

/// Whether `a` feeds the op graph that makes `b` (directly or through
/// other ops): a source, cutter or blank left visible next to its result.
pub fn derives_from(model: &Model, b: &str, a: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![b.to_string()];
    while let Some(n) = stack.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        for op in &model.ops {
            if crate::graph::outputs(op).contains(&n) {
                for i in crate::graph::inputs(op) {
                    if i == a && n != a {
                        return true;
                    }
                    if i != n {
                        stack.push(i.to_string());
                    }
                }
            }
        }
    }
    false
}

/// Why an overlap is most likely wrong, when the graph or the numbers say.
pub fn diagnose(model: &Model, o: &Overlap) -> Option<String> {
    if derives_from(model, &o.b, &o.a) {
        return Some(format!(
            "`{}` is an input of `{}` and still visible: hide it",
            o.a, o.b
        ));
    }
    if derives_from(model, &o.a, &o.b) {
        return Some(format!(
            "`{}` is an input of `{}` and still visible: hide it",
            o.b, o.a
        ));
    }
    (o.fraction >= 0.99).then(|| "the smaller part is entirely hidden inside the other".to_string())
}

/// The world points of `p` touching the ground plane y = 0.
pub fn ground_points(p: &Part) -> Vec<Vec3> {
    let tol = ground_tolerance(p);
    (0..p.verts.len())
        .map(|i| p.world_vert(i))
        .filter(|v| v.y <= tol)
        .collect()
}

fn com_y(p: &Part) -> f32 {
    let (v, c) = crate::inspect::volume_centroid(p);
    if v.abs() > 1e-15 {
        c[1] as f32
    } else {
        let (lo, hi) = p.bounds();
        (lo.y + hi.y) * 0.5
    }
}

/// Visible parts that touch neither the ground nor any other visible part,
/// by real geometry (shared volume or surfaces within [`tolerance`]).
pub fn floating(model: &Model) -> Vec<String> {
    let bodies: Vec<Body> = visible(model).map(Body::new).collect();
    let ground = ground_level(model);
    // Grounded = connected to the ground through a chain of contacts, so two
    // parts touching only each other in mid-air still float.
    let mut reached: Vec<bool> = bodies
        .iter()
        .map(|b| b.part.bounds().0.y <= ground + ground_tolerance(b.part))
        .collect();
    let mut frontier: Vec<usize> = (0..bodies.len()).filter(|&i| reached[i]).collect();
    while let Some(i) = frontier.pop() {
        for j in 0..bodies.len() {
            if !reached[j] && contact(&bodies[i], &bodies[j]).is_some() {
                reached[j] = true;
                frontier.push(j);
            }
        }
    }
    bodies
        .iter()
        .zip(&reached)
        .filter(|(_, r)| !**r)
        .map(|(b, _)| b.part.name.clone())
        .collect()
}

fn visible(model: &Model) -> impl Iterator<Item = &Part> {
    model
        .parts
        .iter()
        .map(|p| &**p)
        .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty())
}

/// The floor: y = 0, or the model's lowest point if it sits below that.
fn ground_level(model: &Model) -> f32 {
    model.bounds().map_or(0.0, |(lo, _)| lo.y.min(0.0))
}

/// Why a part is held up, or `None` when nothing holds it.
fn support_of(a: &Body, bodies: &[Body], ground: f32) -> Option<String> {
    let (lo, _) = a.part.bounds();
    if lo.y <= ground + ground_tolerance(a.part) {
        return Some("on the ground".into());
    }
    let cy = com_y(a.part);
    let margin = tolerance(a.part, a.part);
    let mut above = Vec::new();
    for b in bodies {
        if std::ptr::eq(b.part, a.part) {
            continue;
        }
        match contact(a, b) {
            Some(Contact::Overlap) => return Some(format!("attached to `{}`", b.part.name)),
            Some(Contact::Surface(pts)) => {
                // A flat part (a sheet lying down) is held by contact in its
                // own plane; anything else needs contact below its centre.
                let (plo, phi) = a.part.bounds();
                let flat = phi.y - plo.y <= margin * 2.0;
                if flat || pts.iter().any(|p| p.y < cy - margin) {
                    return Some(format!("rests on `{}`", b.part.name));
                }
                above.push(b.part.name.clone());
            }
            None => {}
        }
    }
    let _ = above;
    None
}

/// Visible parts that nothing holds up: off the ground, sharing no volume
/// with another part, and with no surface contact below their centre of
/// mass. Catches what [`floating`] cannot: a part only touching something
/// ABOVE it (wedged under a shelf) or only touching beside it at its top.
/// Each entry is `(name, what it touches, if anything)`.
pub fn unsupported(model: &Model) -> Vec<(String, Vec<String>)> {
    let bodies: Vec<Body> = visible(model).map(Body::new).collect();
    let ground = ground_level(model);
    let mut out = Vec::new();
    for a in &bodies {
        if support_of(a, &bodies, ground).is_none() {
            let touching: Vec<String> = bodies
                .iter()
                .filter(|b| !std::ptr::eq(b.part, a.part) && contact(a, b).is_some())
                .map(|b| b.part.name.clone())
                .collect();
            out.push((a.part.name.clone(), touching));
        }
    }
    out
}

/// Whether `a` rests on (or is attached to) `b`: they share volume, or they
/// touch at points below `a`'s centre of mass. Returns a description either way.
pub fn rests_on(model: &Model, a: &str, b: &str) -> Result<String, String> {
    let find = |n: &str| {
        model
            .parts
            .iter()
            .find(|p| p.name == n)
            .map(|p| &**p)
            .ok_or_else(|| format!("no part `{n}`"))
    };
    let (pa, pb) = (find(a)?, find(b)?);
    let (ba, bb) = (Body::new(pa), Body::new(pb));
    let cy = com_y(pa);
    match contact(&ba, &bb) {
        Some(Contact::Overlap) => Ok(format!("`{a}` is attached to `{b}` (shared volume)")),
        Some(Contact::Surface(pts)) => {
            let low = pts.iter().map(|p| p.y).fold(f32::MAX, f32::min);
            if low < cy - tolerance(pa, pa) {
                Ok(format!(
                    "`{a}` rests on `{b}` ({} contact point(s), lowest y {low:.4}, centre of mass y {cy:.4})",
                    pts.len()
                ))
            } else {
                Err(format!(
                    "`{a}` touches `{b}` only at or above its centre of mass (contact y >= {low:.4}, centre y {cy:.4}): it leans or is wedged, it does not rest on it"
                ))
            }
        }
        None => {
            let gap = match (&ba.solid, &bb.solid) {
                (Some(x), Some(y)) => {
                    format!(" (closest surfaces {:.4} m apart)", x.min_gap(y, 10.0))
                }
                _ => String::new(),
            };
            Err(format!("`{a}` does not touch `{b}`{gap}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::Session;

    fn session(items: serde_json::Value) -> Session {
        let mut s = Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("array");
        };
        s.apply_values_as(None, items, true)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn wedged_part_is_unsupported_and_resting_one_is_not() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "shelf", "kind": "box", "size": [1, 0.02, 0.3], "translation": [0, 1.01, 0]},
            {"type": "primitive", "name": "leg", "kind": "box", "size": [0.05, 1, 0.05], "translation": [0, 0.5, 0]},
            // Touches only the underside of the shelf: wedged.
            {"type": "primitive", "name": "wedged", "kind": "box", "size": [0.1, 0.3, 0.1], "translation": [0.3, 0.85, 0]},
            // Sits on the shelf.
            {"type": "primitive", "name": "book", "kind": "box", "size": [0.1, 0.3, 0.1], "translation": [-0.3, 1.17, 0]}
        ]));
        let m = &s.model;
        let names: Vec<String> = super::unsupported(m).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["wedged"]);
        assert!(super::rests_on(m, "book", "shelf").is_ok());
        assert!(super::rests_on(m, "wedged", "shelf").is_err());
        assert!(super::floating(m).is_empty());
    }

    #[test]
    fn parts_touching_only_each_other_in_mid_air_float() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "base", "kind": "box", "size": 1, "translation": [0, 0.5, 0]},
            {"type": "primitive", "name": "door", "kind": "box", "size": [0.5, 0.5, 0.02], "translation": [3, 1, 0]},
            {"type": "primitive", "name": "knob", "kind": "box", "size": 0.04, "translation": [3, 1, 0.03]}
        ]));
        assert_eq!(super::floating(&s.model), ["door", "knob"]);
    }

    #[test]
    fn small_gap_on_a_small_part_is_a_gap() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "base", "kind": "box", "size": [0.3, 0.015, 0.2], "translation": [0, 0.0075, 0]},
            {"type": "primitive", "name": "screen", "kind": "box", "size": [0.3, 0.2, 0.008], "translation": [0, 0.1158, -0.09]}
        ]));
        assert_eq!(super::floating(&s.model), ["screen"]);
    }
}

/// One ray hit: part name, distance along the (normalised) ray, world point.
#[derive(Debug, Clone)]
pub struct RayHit {
    pub part: String,
    pub t: f32,
    pub point: Vec3,
}

/// Möller–Trumbore ray/triangle, two-sided, in f64; the hit distance if any.
/// Triangles within ~0.5° of parallel to the ray are skipped: a grazing hit
/// on a steep facet is numerically meaningless (and would let `place` think
/// a curved side hangs lower than the flat bottom next to it).
fn ray_tri(o: Vec3, d: Vec3, t: &[Vec3; 3]) -> Option<f32> {
    let v = |a: Vec3| bevy::math::DVec3::new(f64::from(a.x), f64::from(a.y), f64::from(a.z));
    let (o, d) = (v(o), v(d));
    let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
    let (e1, e2) = (b - a, c - a);
    let n = e1.cross(e2);
    let nl = n.length();
    if nl < 1e-18 || n.dot(d).abs() < 0.01 * nl * d.length() {
        return None;
    }
    let p = d.cross(e2);
    let det = e1.dot(p);
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let w = d.dot(q) * inv;
    if w < -1e-9 || u + w > 1.0 + 1e-9 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist > 0.0).then_some(dist as f32)
}

/// Nearest hit distance of a ray against a triangle list.
pub(crate) fn ray_tris(o: Vec3, d: Vec3, tris: &[[Vec3; 3]]) -> Option<f32> {
    let best = tris
        .iter()
        .filter_map(|t| ray_tri(o, d, t))
        .fold(f32::MAX, f32::min);
    (best < f32::MAX).then_some(best)
}

/// Casts a ray against the visible parts (or only `only`, when given): the
/// nearest hit per part, nearest first. CPU only, so it works headless.
pub fn ray_cast(model: &Model, origin: Vec3, dir: Vec3, only: Option<&[String]>) -> Vec<RayHit> {
    let d = dir.normalize_or_zero();
    if d == Vec3::ZERO {
        return Vec::new();
    }
    let mut hits: Vec<RayHit> = visible(model)
        .filter(|p| only.is_none_or(|o| o.iter().any(|n| crate::name_matches(n, &p.name))))
        .filter_map(|p| {
            let best = world_tris(p)
                .iter()
                .filter_map(|t| ray_tri(origin, d, t))
                .fold(f32::MAX, f32::min);
            (best < f32::MAX).then(|| RayHit {
                part: p.name.clone(),
                t: best,
                point: origin + d * best,
            })
        })
        .collect();
    hits.sort_by(|a, b| a.t.total_cmp(&b.t));
    hits
}

/// The `probe_ray` report.
pub fn probe_text(
    model: &Model,
    origin: [f32; 3],
    dir: [f32; 3],
    all_hits: bool,
) -> Result<String, String> {
    if Vec3::from(dir).length() < 1e-12 {
        return Err("ray direction is zero".into());
    }
    let hits = ray_cast(model, Vec3::from(origin), Vec3::from(dir), None);
    let describe = |h: &RayHit| {
        format!(
            "hit `{}` at t={:.4}, point [{:.4}, {:.4}, {:.4}]",
            h.part, h.t, h.point.x, h.point.y, h.point.z
        )
    };
    Ok(match hits.first() {
        None => "no hit — the ray passes clean through".to_string(),
        Some(h) if !all_hits => describe(h),
        Some(_) => {
            let lines: Vec<String> = hits
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{}. {}", i + 1, describe(h)))
                .collect();
            format!(
                "{} hit(s) along the ray, nearest first:\n{}",
                lines.len(),
                lines.join("\n")
            )
        }
    })
}

#[cfg(test)]
mod place_tests {
    #[test]
    fn paperweight_lands_flush() {
        let mut s = crate::commands::Session::default();
        let items: Vec<serde_json::Value> = serde_json::from_value(serde_json::json!([
            {"type": "primitive", "name": "top_raw", "kind": "box", "size": [1.4, 0.03, 0.7], "translation": [0, 0.735, 0]},
            {"type": "round", "name": "desk_top", "input": "top_raw", "mode": "fillet", "radius": 0.006},
            {"type": "primitive", "name": "cube_raw", "kind": "box", "size": 0.05, "translation": [-0.2, 0.775, -0.15]},
            {"type": "primitive", "name": "ball_raw", "kind": "sphere", "size": 0.034, "translation": [-0.2, 0.775, -0.15]},
            {"type": "boolean", "name": "paperweight", "op": "intersection", "a": "cube_raw", "b": "ball_raw"},
            {"type": "hide_part", "part": "*_raw"},
            {"type": "place", "name": "w", "parts": ["paperweight"], "on": "desk_top", "at": [-0.22, -0.15]}
        ])).unwrap_or_default();
        s.apply_values_as(None, items, true)
            .unwrap_or_else(|e| panic!("{e}"));
        let lo = s
            .model
            .part("paperweight")
            .map(|p| p.bounds().0)
            .unwrap_or_default();
        assert!((lo.y - 0.75).abs() < 1e-5, "{lo:?}");
    }
}
