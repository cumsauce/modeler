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

fn boxes_near(a: &Body, b: &Body, tol: f32) -> bool {
    let ((alo, ahi), (blo, bhi)) = (a.bounds, b.bounds);
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
    /// World bounds, from the triangles (once, not per pair).
    bounds: (Vec3, Vec3),
    /// The solid's volume (once: Manifold recomputes it on every call).
    volume: f64,
}

impl<'a> Body<'a> {
    pub fn new(part: &'a Part) -> Self {
        let tris = world_tris(part);
        // An empty part's bounds (MAX..MIN) are near nothing, as before.
        let bounds = part.bounds();
        let solid = crate::csg::part_to_manifold(part).ok();
        let volume = solid.as_ref().map_or(0.0, |m| m.volume().abs());
        Self {
            part,
            solid,
            tris,
            bounds,
            volume,
        }
    }
}

/// How `a` and `b` meet, if they do.
pub fn contact(a: &Body, b: &Body) -> Option<Contact> {
    let tol = tolerance(a.part, b.part);
    if !boxes_near(a, b, tol) {
        return None;
    }
    if let (Some(ma), Some(mb)) = (&a.solid, &b.solid) {
        // Boxes that only meet within the tolerance can't share volume:
        // skip the intersection, go straight to the gap.
        if boxes_near(a, b, 0.0) {
            let ov = ma.intersection(mb).volume().abs();
            let small = a.volume.min(b.volume).max(1e-30);
            if ov > small * 1e-6 {
                return Some(Contact::Overlap);
            }
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
    /// How far the smaller part is sunk in, m: the shared region's extent
    /// along the axis where it covers the least of the smaller part (a rod
    /// pushed 2 cm into a wall is 2 cm deep, not its 2 mm thickness).
    pub depth: f64,
}

/// One `collisions.allow` entry: a pair of name patterns (`*` globs, either
/// order), optionally only while the overlap stays shallow.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AllowRule {
    Pair([String; 2]),
    Limited {
        pair: [String; 2],
        /// Allowed only while the penetration depth is at most this (m).
        #[serde(default)]
        depth: Option<f64>,
        /// Allowed only while at most this share of the smaller part is inside.
        #[serde(default)]
        max: Option<f64>,
    },
}

impl AllowRule {
    pub fn pair(&self) -> &[String; 2] {
        match self {
            AllowRule::Pair(p) | AllowRule::Limited { pair: p, .. } => p,
        }
    }

    /// Whether the names match (either order), ignoring the limits.
    pub fn names_match(&self, o: &Overlap) -> bool {
        let m = crate::name_matches;
        let [x, y] = self.pair();
        (m(x, &o.a) && m(y, &o.b)) || (m(x, &o.b) && m(y, &o.a))
    }

    /// Whether this rule excuses the overlap (names match, within limits).
    pub fn covers(&self, o: &Overlap) -> bool {
        if !self.names_match(o) {
            return false;
        }
        match self {
            AllowRule::Pair(_) => true,
            AllowRule::Limited { depth, max, .. } => {
                depth.is_none_or(|d| o.depth <= d) && max.is_none_or(|m| o.fraction <= m)
            }
        }
    }

    /// True when one side is a bare `*`: excuses the other side against
    /// anything, which hides real collisions.
    pub fn is_catch_all(&self) -> bool {
        self.pair().iter().any(|p| p == "*")
    }

    pub fn describe(&self) -> String {
        let [x, y] = self.pair();
        match self {
            AllowRule::Pair(_) => format!("[{x}, {y}]"),
            AllowRule::Limited { depth, max, .. } => {
                let mut s = format!("[{x}, {y}");
                if let Some(d) = depth {
                    s += &format!(", depth <= {d} m");
                }
                if let Some(m) = max {
                    s += &format!(", max {:.0}%", m * 100.0);
                }
                s + "]"
            }
        }
    }
}

/// What a collision check found: the failing pairs, the joints (shallow or
/// excused), and per allow rule the deep pairs it excused.
pub struct CollisionReport {
    pub bad: Vec<Overlap>,
    pub joints: Vec<Overlap>,
    pub excused: Vec<(AllowRule, Vec<Overlap>)>,
}

/// Every pair of visible closed parts that share volume, deepest first.
pub fn overlaps(model: &Model) -> Vec<Overlap> {
    use rayon::prelude::*;
    let bodies = bodies_of(model);
    let n = bodies.len();
    let pairs: Vec<(usize, usize)> = (0..n)
        .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
        .filter(|&(i, j)| boxes_near(&bodies[i], &bodies[j], 0.0))
        .collect();
    let mut out: Vec<Overlap> = pairs
        .par_iter()
        .filter_map(|&(i, j)| overlap_between(&bodies[i], &bodies[j]))
        .collect();
    out.sort_by(|x, y| y.fraction.total_cmp(&x.fraction));
    out
}

/// The volume `a` and `b` share, if any.
pub fn overlap_between(a: &Body, b: &Body) -> Option<Overlap> {
    if !boxes_near(a, b, 0.0) {
        return None;
    }
    let (Some(ma), Some(mb)) = (&a.solid, &b.solid) else {
        return None;
    };
    let inter = ma.intersection(mb);
    let ov = inter.volume().abs();
    let small = a.volume.min(b.volume).max(1e-30);
    if ov <= small * 1e-6 {
        return None;
    }
    let smaller = if a.volume <= b.volume { a.part } else { b.part };
    let (slo, shi) = smaller.bounds();
    let (at, depth) = inter.bounding_box().map_or(([0.0; 3], 0.0), |bb| {
        let (lo, hi) = (bb.min(), bb.max());
        // The axis where the shared box covers the least of the smaller
        // part's extent is the direction it was pushed in along.
        let mut best = (f64::MAX, 0.0);
        for k in 0..3 {
            let ext = hi[k] - lo[k];
            let part_ext = f64::from(shi[k] - slo[k]).max(1e-9);
            let ratio = ext / part_ext;
            if ratio < best.0 {
                best = (ratio, ext);
            }
        }
        (
            [
                (lo[0] + hi[0]) * 0.5,
                (lo[1] + hi[1]) * 0.5,
                (lo[2] + hi[2]) * 0.5,
            ],
            best.1,
        )
    });
    Some(Overlap {
        a: a.part.name.clone(),
        b: b.part.name.clone(),
        volume: ov,
        fraction: (ov / small).min(1.0),
        at,
        depth,
    })
}

impl Overlap {
    pub fn describe(&self) -> String {
        format!(
            "`{}` / `{}`: {:.0}% of the smaller inside the other, {:.1} mm deep ({:.3e} m³ shared, centred at [{:.3}, {:.3}, {:.3}])",
            self.a,
            self.b,
            self.fraction * 100.0,
            self.depth * 1000.0,
            self.volume,
            self.at[0],
            self.at[1],
            self.at[2]
        )
    }

    /// Whether any allow rule covers it.
    pub fn allowed_by(&self, allow: &[AllowRule]) -> bool {
        allow.iter().any(|r| r.covers(self))
    }

    /// Whether `name` is one side of the pair.
    pub fn involves(&self, name: &str) -> bool {
        self.a == name || self.b == name
    }
}

/// Overlaps split into collisions (more than `max` of the smaller part
/// buried, and not allowed) and joints (shallow or allowed), with what each
/// allow rule excused beyond `max`. `scope` keeps pairs with at least one
/// side matching (empty = all).
pub fn collisions(
    model: &Model,
    allow: &[AllowRule],
    max: f64,
    scope: &[String],
) -> CollisionReport {
    let in_scope = |o: &Overlap| {
        scope.is_empty()
            || scope
                .iter()
                .any(|p| crate::name_matches(p, &o.a) || crate::name_matches(p, &o.b))
    };
    let mut excused: Vec<(AllowRule, Vec<Overlap>)> =
        allow.iter().map(|r| (r.clone(), Vec::new())).collect();
    let (mut bad, mut joints) = (Vec::new(), Vec::new());
    for o in overlaps(model).into_iter().filter(|o| in_scope(o)) {
        if o.fraction <= max {
            joints.push(o);
            continue;
        }
        match excused.iter_mut().find(|(r, _)| r.covers(&o)) {
            Some((_, list)) => {
                list.push(o.clone());
                joints.push(o);
            }
            None => bad.push(o),
        }
    }
    CollisionReport {
        bad,
        joints,
        excused,
    }
}

/// Parts that stay grounded only through deep overlaps: grounded normally,
/// but floating once overlaps burying more than `deep` of the smaller part
/// stop counting as contacts. Each with the parts it passes through.
/// Every visible part's [`Body`] and every touching pair (each tested once,
/// in parallel), shared by the checks of one `verify` call.
pub struct Contacts<'a> {
    bodies: Vec<Body<'a>>,
    /// (i, j, how they meet, overlap fraction when they share volume).
    pairs: Vec<(usize, usize, Contact, Option<f64>)>,
}

impl<'a> Contacts<'a> {
    pub fn new(model: &'a Model) -> Contacts<'a> {
        use rayon::prelude::*;
        let bodies = bodies_of(model);
        let n = bodies.len();
        let near: Vec<(usize, usize)> = (0..n)
            .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
            .filter(|&(i, j)| {
                boxes_near(
                    &bodies[i],
                    &bodies[j],
                    tolerance(bodies[i].part, bodies[j].part),
                )
            })
            .collect();
        let pairs = near
            .par_iter()
            .filter_map(|&(i, j)| {
                let c = contact(&bodies[i], &bodies[j])?;
                let f = matches!(c, Contact::Overlap)
                    .then(|| overlap_between(&bodies[i], &bodies[j]).map_or(0.0, |o| o.fraction));
                Some((i, j, c, f))
            })
            .collect();
        Contacts { bodies, pairs }
    }
}

/// The `grounded` check in one pass: the floating parts, and the parts held
/// up only by passing more than `deep` of the smaller part into another.
/// Each solid is built and each pair tested once.
pub fn grounding(model: &Model, deep: f64) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    grounding_in(&Contacts::new(model), model, deep)
}

/// [`grounding`] over contacts already found.
pub fn grounding_in(
    c: &Contacts,
    model: &Model,
    deep: f64,
) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    let bodies = &c.bodies;
    let n = bodies.len();
    let edges: Vec<(usize, usize, Option<f64>)> =
        c.pairs.iter().map(|&(i, j, _, f)| (i, j, f)).collect();
    let ground = ground_level(model);
    let on_ground: Vec<bool> = bodies
        .iter()
        .map(|b| b.bounds.0.y <= ground + ground_tolerance(b.part))
        .collect();
    let reach = |skip_deep: bool| -> Vec<bool> {
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for &(i, j, f) in &edges {
            if skip_deep && f.is_some_and(|f| f > deep) {
                continue;
            }
            adj[i].push(j);
            adj[j].push(i);
        }
        let mut reached = on_ground.clone();
        let mut frontier: Vec<usize> = (0..n).filter(|&i| reached[i]).collect();
        while let Some(i) = frontier.pop() {
            for &j in &adj[i] {
                if !reached[j] {
                    reached[j] = true;
                    frontier.push(j);
                }
            }
        }
        reached
    };
    let normal = reach(false);
    let floating = (0..n)
        .filter(|&i| !normal[i])
        .map(|i| bodies[i].part.name.clone())
        .collect();
    let held = if edges.iter().any(|e| e.2.is_some_and(|f| f > deep)) {
        let strict = reach(true);
        (0..n)
            .filter(|&i| normal[i] && !strict[i])
            .map(|i| {
                let through = edges
                    .iter()
                    .filter(|e| e.2.is_some_and(|f| f > deep) && (e.0 == i || e.1 == i))
                    .map(|e| bodies[if e.0 == i { e.1 } else { e.0 }].part.name.clone())
                    .collect();
                (bodies[i].part.name.clone(), through)
            })
            .collect()
    } else {
        Vec::new()
    };
    (floating, held)
}

pub fn held_by_deep_overlaps(model: &Model, deep: f64) -> Vec<(String, Vec<String>)> {
    let deep_pairs: Vec<Overlap> = overlaps(model)
        .into_iter()
        .filter(|o| o.fraction > deep)
        .collect();
    if deep_pairs.is_empty() {
        return Vec::new();
    }
    let normal = floating(model);
    let strict = floating_skipping(model, &|a, b| {
        deep_pairs
            .iter()
            .any(|o| (o.a == a && o.b == b) || (o.a == b && o.b == a))
    });
    strict
        .into_iter()
        .filter(|n| !normal.contains(n))
        .map(|n| {
            let through: Vec<String> = deep_pairs
                .iter()
                .filter(|o| o.involves(&n))
                .map(|o| if o.a == n { o.b.clone() } else { o.a.clone() })
                .collect();
            (n, through)
        })
        .collect()
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
    floating_skipping(model, &|_, _| false)
}

/// [`floating`] ignoring the contacts `skip(a, b)` names.
fn floating_skipping(model: &Model, skip: &dyn Fn(&str, &str) -> bool) -> Vec<String> {
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
            if !reached[j]
                && !skip(&bodies[i].part.name, &bodies[j].part.name)
                && contact(&bodies[i], &bodies[j]).is_some()
            {
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

/// Visible parts that nothing holds up: off the ground, sharing no volume
/// with another part, and with no surface contact below their centre of
/// mass. Catches what [`floating`] cannot: a part only touching something
/// ABOVE it (wedged under a shelf) or only touching beside it at its top.
/// Each entry is `(name, what it touches, if anything)`.
pub fn unsupported(model: &Model) -> Vec<(String, Vec<String>)> {
    unsupported_in(&Contacts::new(model), model)
}

/// [`unsupported`] over contacts already found: each pair was tested once
/// (contact is symmetric), not once from each side.
pub fn unsupported_in(c: &Contacts, model: &Model) -> Vec<(String, Vec<String>)> {
    let ground = ground_level(model);
    let mut touching: Vec<Vec<(usize, &Contact)>> = vec![Vec::new(); c.bodies.len()];
    for (i, j, k, _) in &c.pairs {
        touching[*i].push((*j, k));
        touching[*j].push((*i, k));
    }
    let mut out = Vec::new();
    for (i, a) in c.bodies.iter().enumerate() {
        let (lo, hi) = a.bounds;
        if lo.y <= ground + ground_tolerance(a.part) {
            continue;
        }
        let cy = com_y(a.part);
        let margin = tolerance(a.part, a.part);
        let flat = hi.y - lo.y <= margin * 2.0;
        // Same rule as support_of: shared volume holds it; so does surface
        // contact below its centre of mass (any contact, for a flat part).
        let held = touching[i].iter().any(|(_, k)| match k {
            Contact::Overlap => true,
            Contact::Surface(pts) => flat || pts.iter().any(|p| p.y < cy - margin),
        });
        if !held {
            let mut idx: Vec<usize> = touching[i].iter().map(|(j, _)| *j).collect();
            idx.sort_unstable();
            out.push((
                a.part.name.clone(),
                idx.iter().map(|&j| c.bodies[j].part.name.clone()).collect(),
            ));
        }
    }
    out
}

/// Every visible part's [`Body`] (solid, triangles, bounds), built in
/// parallel: the solid conversion is the expensive part.
fn bodies_of(model: &Model) -> Vec<Body<'_>> {
    use rayon::prelude::*;
    let parts: Vec<&Part> = visible(model).collect();
    parts.par_iter().map(|p| Body::new(p)).collect()
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

/// Triangles binned on a 2D grid across one axis, for rays that travel
/// along that axis (`place`): a ray tests only its own cell's triangles
/// instead of all of them.
pub(crate) struct AxisGrid<'a> {
    tris: &'a [[Vec3; 3]],
    u: usize,
    w: usize,
    lo: [f32; 2],
    cell: [f32; 2],
    n: [usize; 2],
    bins: Vec<Vec<u32>>,
}

impl<'a> AxisGrid<'a> {
    /// Bins `tris` over the cross axes `u` and `w`.
    pub(crate) fn new(tris: &'a [[Vec3; 3]], u: usize, w: usize) -> AxisGrid<'a> {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for t in tris {
            for v in t {
                lo = [lo[0].min(v[u]), lo[1].min(v[w])];
                hi = [hi[0].max(v[u]), hi[1].max(v[w])];
            }
        }
        // About one triangle per cell on average, capped.
        let side = ((tris.len() as f32).sqrt().ceil() as usize).clamp(1, 256);
        let n = [side, side];
        let cell = [
            ((hi[0] - lo[0]) / side as f32).max(1e-6),
            ((hi[1] - lo[1]) / side as f32).max(1e-6),
        ];
        let mut g = AxisGrid {
            tris,
            u,
            w,
            lo,
            cell,
            n,
            bins: vec![Vec::new(); side * side],
        };
        if tris.is_empty() {
            return g;
        }
        for (i, t) in tris.iter().enumerate() {
            let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
            for v in t {
                a = [a[0].min(v[u]), a[1].min(v[w])];
                b = [b[0].max(v[u]), b[1].max(v[w])];
            }
            let (i0, j0) = g.index(a[0] - 1e-6, a[1] - 1e-6);
            let (i1, j1) = g.index(b[0] + 1e-6, b[1] + 1e-6);
            for x in i0..=i1 {
                for z in j0..=j1 {
                    g.bins[x * g.n[1] + z].push(i as u32);
                }
            }
        }
        g
    }

    fn index(&self, a: f32, b: f32) -> (usize, usize) {
        let f = |x: f32, k: usize| {
            (((x - self.lo[k]) / self.cell[k]).max(0.0) as usize).min(self.n[k] - 1)
        };
        (f(a, 0), f(b, 1))
    }

    /// Nearest hit of the ray from `o` along `d` (parallel to the grid's
    /// axis), testing only the triangles binned at `o`'s cell.
    pub(crate) fn cast(&self, o: Vec3, d: Vec3) -> Option<f32> {
        if self.tris.is_empty() {
            return None;
        }
        let (a, b) = (o[self.u], o[self.w]);
        let (lo, hi) = (
            self.lo,
            [
                self.lo[0] + self.cell[0] * self.n[0] as f32,
                self.lo[1] + self.cell[1] * self.n[1] as f32,
            ],
        );
        if a < lo[0] - 1e-5 || b < lo[1] - 1e-5 || a > hi[0] + 1e-5 || b > hi[1] + 1e-5 {
            return None;
        }
        let (i, j) = self.index(a, b);
        let best = self.bins[i * self.n[1] + j]
            .iter()
            .filter_map(|&t| ray_tri(o, d, &self.tris[t as usize]))
            .fold(f32::MAX, f32::min);
        (best < f32::MAX).then_some(best)
    }
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
