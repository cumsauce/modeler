//! `sdf` and `skin`: organic solids modeled as signed distance fields.
//!
//! An `sdf` item lists analytic shapes (sphere, ellipsoid, rounded box,
//! capsule / rounded cone, cylinder, torus, a radius-varying chain, a
//! mesh-sampled part, a nested group), each combined into the running field
//! by a smooth union, subtract or intersect of radius `blend`. Field-level
//! warps (twist, bend, taper of the space, noise displacement of the
//! distance) and a mirror fold act on the whole field, which Manifold's
//! level-set mesher turns into one closed, outward solid at the end.
//!
//! A `skin` is the same engine fed by a skeleton: chains of points with
//! radii (shared joints by name) become smooth tubes of rounded cones,
//! smooth-blended where chains meet: creatures, tentacles, roots, hands.
//!
//! Evaluation is bucketed: the shapes' boxes go into a coarse grid, so a
//! sample only evaluates the shapes near it (a skin of hundreds of segments
//! stays fast). Far from every shape the field is clamped to `band`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::deform::{Deform, parse_dir};
use crate::material::noise::{Lattice, fractal, gradient};
use crate::{ModelerError, Part};
use bevy::math::{Quat, Vec3, Vec4};
use manifold_csg::Manifold;
use serde::{Deserialize, Serialize};

/// Most samples a default resolution meshes (about 230^3); an explicit
/// `resolution` may go up to [`MAX_SAMPLES`].
const AUTO_SAMPLES: f64 = 12e6;
const MAX_SAMPLES: f64 = 27e6;

fn none<T>(v: &Option<T>) -> bool {
    v.is_none()
}

/// One shape of an `sdf` item, as stored in the op (rotation as a quat).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Shape {
    /// sphere / ellipsoid / box / capsule / cylinder / torus / chain / part / group.
    pub shape: String,
    /// union (default) / subtract / intersect: how it joins the shapes before it.
    #[serde(default, skip_serializing_if = "none")]
    pub op: Option<String>,
    /// Smooth radius of this join (default: the item's blend).
    #[serde(default, skip_serializing_if = "none")]
    pub blend: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub at: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "none")]
    pub rotation: Option<[f32; 4]>,
    /// sphere / cylinder radius; capsule `[r]` or `[r_from, r_to]`.
    #[serde(default, skip_serializing_if = "none")]
    pub radius: Option<Vec<f32>>,
    /// ellipsoid radii [x, y, z]; box full size [x, y, z]; torus [ring, tube].
    #[serde(default, skip_serializing_if = "none")]
    pub size: Option<Vec<f32>>,
    /// cylinder full height (along its local y).
    #[serde(default, skip_serializing_if = "none")]
    pub height: Option<f32>,
    /// box / cylinder edge rounding radius.
    #[serde(default, skip_serializing_if = "none")]
    pub round: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub from: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "none")]
    pub to: Option<[f32; 3]>,
    /// chain points [x, y, z, radius].
    #[serde(default, skip_serializing_if = "none")]
    pub points: Option<Vec<[f32; 4]>>,
    /// chain: a smooth curve through the points (default true).
    #[serde(default, skip_serializing_if = "none")]
    pub smooth: Option<bool>,
    /// part: a closed solid of the model, sampled as a distance field.
    #[serde(default, skip_serializing_if = "none")]
    pub part: Option<String>,
    /// group: shapes combined on their own first, then joined as one.
    #[serde(default, skip_serializing_if = "none")]
    pub shapes: Option<Vec<Shape>>,
}

pub const SHAPES: &[&str] = &[
    "sphere",
    "ellipsoid",
    "box",
    "capsule",
    "cylinder",
    "torus",
    "chain",
    "part",
    "group",
];

/// The parameters of an `sdf` op.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SdfDef {
    pub shapes: Vec<Shape>,
    #[serde(default, skip_serializing_if = "none")]
    pub blend: Option<f32>,
    /// Fold the field so it is symmetric: "x" (model the +x side), "z", "xz".
    #[serde(default, skip_serializing_if = "none")]
    pub mirror: Option<String>,
    /// Space warps applied to the whole field, in order (twist / bend /
    /// taper / displace, the `deform` fields).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warps: Vec<Deform>,
    #[serde(default, skip_serializing_if = "none")]
    pub resolution: Option<f32>,
}

/// A chain point: `[x, y, z, radius]` or the name of a joint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChainPt {
    Joint(String),
    Point([f32; 4]),
}

/// The parameters of a `skin` op.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SkinDef {
    pub chains: Vec<Vec<ChainPt>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub joints: BTreeMap<String, [f32; 4]>,
    /// Smooth curves through the chain points (default true).
    #[serde(default, skip_serializing_if = "none")]
    pub smooth: Option<bool>,
    #[serde(default, skip_serializing_if = "none")]
    pub blend: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub mirror: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warps: Vec<Deform>,
    #[serde(default, skip_serializing_if = "none")]
    pub resolution: Option<f32>,
}

fn finite_pos(v: f32) -> bool {
    v.is_finite() && v > 0.0
}

impl Shape {
    /// Checks fields for this shape type; `at` names it in errors.
    pub fn validate(&self, at: &str) -> Result<(), String> {
        let e = |m: String| Err(format!("{at} ({}): {m}", self.shape));
        if !SHAPES.contains(&self.shape.as_str()) {
            return Err(format!(
                "{at}: shape: must be one of {}, got `{}`",
                SHAPES.join(", "),
                self.shape
            ));
        }
        if let Some(o) = &self.op
            && !["union", "subtract", "intersect"].contains(&o.as_str())
        {
            return e(format!(
                "op: must be union, subtract or intersect, got `{o}`"
            ));
        }
        if self.blend.is_some_and(|b| !b.is_finite() || b < 0.0) {
            return e("blend: must be >= 0 meters".into());
        }
        if self.round.is_some_and(|b| !b.is_finite() || b < 0.0) {
            return e("round: must be >= 0 meters".into());
        }
        // Which fields each shape reads; anything else set is a mistake.
        let allowed: &[&str] = match self.shape.as_str() {
            "sphere" => &["radius", "at"],
            "ellipsoid" => &["size", "at", "rotation"],
            "box" => &["size", "round", "at", "rotation"],
            "capsule" => &["from", "to", "radius"],
            "cylinder" => &["radius", "height", "round", "at", "rotation"],
            "torus" => &["size", "at", "rotation"],
            "chain" => &["points", "smooth"],
            "part" => &["part"],
            _ => &["shapes"],
        };
        let set = [
            ("at", self.at.is_some()),
            ("rotation", self.rotation.is_some()),
            ("radius", self.radius.is_some()),
            ("size", self.size.is_some()),
            ("height", self.height.is_some()),
            ("round", self.round.is_some()),
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("points", self.points.is_some()),
            ("smooth", self.smooth.is_some()),
            ("part", self.part.is_some()),
            ("shapes", self.shapes.is_some()),
        ];
        for (f, on) in set {
            if on && !allowed.contains(&f) {
                return e(format!(
                    "`{f}` does not apply (fields: {})",
                    allowed.join(", ")
                ));
            }
        }
        let radius = |n: usize| -> Result<(), String> {
            match &self.radius {
                Some(r) if !r.is_empty() && r.len() <= n && r.iter().all(|v| finite_pos(*v)) => {
                    Ok(())
                }
                Some(r) => Err(format!(
                    "{at} ({}): radius: {} positive number(s), got {r:?}",
                    self.shape,
                    if n == 1 { "one" } else { "one or two" }
                )),
                None => Err(format!("{at} ({}): radius: required", self.shape)),
            }
        };
        let size = |n: usize, what: &str| -> Result<(), String> {
            match &self.size {
                Some(s) if (s.len() == n || s.len() == 1) && s.iter().all(|v| finite_pos(*v)) => {
                    Ok(())
                }
                Some(s) => Err(format!(
                    "{at} ({}): size: {what}, positive, got {s:?}",
                    self.shape
                )),
                None => Err(format!("{at} ({}): size: required ({what})", self.shape)),
            }
        };
        match self.shape.as_str() {
            "sphere" => radius(1)?,
            "ellipsoid" => size(3, "radii [x, y, z] (or one number)")?,
            "box" => size(3, "full size [x, y, z] (or one number)")?,
            "torus" => {
                size(2, "[ring radius, tube radius]")?;
                if self.size.as_ref().is_some_and(|s| s.len() != 2) {
                    return e("size: [ring radius, tube radius]".into());
                }
            }
            "capsule" => {
                radius(2)?;
                if self.from.is_none() || self.to.is_none() {
                    return e("needs `from` and `to` points".into());
                }
            }
            "cylinder" => {
                radius(1)?;
                if !self.height.is_some_and(finite_pos) {
                    return e("height: required, > 0 meters".into());
                }
            }
            "chain" => match &self.points {
                Some(p) if !p.is_empty() => {
                    if let Some(q) = p.iter().find(|q| !finite_pos(q[3])) {
                        return e(format!(
                            "points: every radius (4th value) must be > 0, got {q:?}"
                        ));
                    }
                }
                _ => return e("points: at least one [x, y, z, radius]".into()),
            },
            "part" => {
                if self.part.as_deref().is_none_or(str::is_empty) {
                    return e("part: the name of a closed solid".into());
                }
            }
            _ => match &self.shapes {
                Some(s) if !s.is_empty() => {
                    for (i, c) in s.iter().enumerate() {
                        c.validate(&format!("{at}.shapes[{i}]"))?;
                    }
                }
                _ => return e("shapes: at least one shape".into()),
            },
        }
        Ok(())
    }

    /// Part names this shape reads (part shapes, nested in groups too).
    pub fn reads<'a>(&'a self, out: &mut Vec<&'a str>) {
        if let Some(p) = &self.part {
            out.push(p);
        }
        for c in self.shapes.iter().flatten() {
            c.reads(out);
        }
    }
}

fn check_mirror(m: &Option<String>) -> Result<(), String> {
    match m.as_deref() {
        None | Some("x" | "z" | "xz" | "zx") => Ok(()),
        Some(o) => Err(format!("mirror: must be x, z or xz, got `{o}`")),
    }
}

fn check_warps(w: &[Deform]) -> Result<(), String> {
    for (i, d) in w.iter().enumerate() {
        d.validate().map_err(|e| format!("warps[{i}]: {e}"))?;
        if d.detail.is_some() {
            return Err(format!(
                "warps[{i}]: detail: does not apply to a field warp"
            ));
        }
        if d.mode == "material" {
            return Err(format!(
                "warps[{i}]: mode: material displacement is a deform op, not a warp"
            ));
        }
    }
    Ok(())
}

fn check_res(r: Option<f32>) -> Result<(), String> {
    if r.is_some_and(|v| !finite_pos(v)) {
        return Err("resolution: must be > 0 meters".into());
    }
    Ok(())
}

impl SdfDef {
    pub fn validate(&self) -> Result<(), String> {
        if self.shapes.is_empty() {
            return Err("shapes: at least one shape".into());
        }
        for (i, s) in self.shapes.iter().enumerate() {
            s.validate(&format!("shapes[{i}]"))?;
        }
        if self.blend.is_some_and(|b| !b.is_finite() || b < 0.0) {
            return Err("blend: must be >= 0 meters".into());
        }
        check_mirror(&self.mirror)?;
        check_warps(&self.warps)?;
        check_res(self.resolution)
    }

    pub fn reads(&self) -> Vec<&str> {
        let mut v = Vec::new();
        for s in &self.shapes {
            s.reads(&mut v);
        }
        v
    }

    pub fn summary(&self) -> String {
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.shapes {
            *kinds.entry(s.shape.as_str()).or_default() += 1;
        }
        let mut s = kinds
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ");
        if let Some(b) = self.blend {
            s += &format!(" blend={b}");
        }
        if let Some(m) = &self.mirror {
            s += &format!(" mirror={m}");
        }
        for w in &self.warps {
            s += &format!(" warp({})", w.summary());
        }
        if let Some(r) = self.resolution {
            s += &format!(" resolution={r}");
        }
        s
    }
}

impl SkinDef {
    pub fn validate(&self) -> Result<(), String> {
        if self.chains.is_empty() {
            return Err("chains: at least one chain of points".into());
        }
        for (k, v) in &self.joints {
            if !finite_pos(v[3]) {
                return Err(format!("joints.{k}: radius (4th value) must be > 0"));
            }
        }
        for (i, c) in self.chains.iter().enumerate() {
            if c.is_empty() {
                return Err(format!("chains[{i}]: at least one point"));
            }
            for (j, p) in c.iter().enumerate() {
                match p {
                    ChainPt::Joint(n) if !self.joints.contains_key(n) => {
                        let known: Vec<&str> = self.joints.keys().map(String::as_str).collect();
                        return Err(format!(
                            "chains[{i}][{j}]: no joint `{n}` (joints: {})",
                            if known.is_empty() {
                                "none".into()
                            } else {
                                known.join(", ")
                            }
                        ));
                    }
                    ChainPt::Point(q) if !finite_pos(q[3]) => {
                        return Err(format!(
                            "chains[{i}][{j}]: radius (4th value) must be > 0, got {q:?}"
                        ));
                    }
                    _ => {}
                }
            }
        }
        if self.blend.is_some_and(|b| !b.is_finite() || b < 0.0) {
            return Err("blend: must be >= 0 meters".into());
        }
        check_mirror(&self.mirror)?;
        check_warps(&self.warps)?;
        check_res(self.resolution)
    }

    pub fn summary(&self) -> String {
        let pts: usize = self.chains.iter().map(Vec::len).sum();
        let mut s = format!(
            "{} chain(s), {pts} point(s), {} joint(s)",
            self.chains.len(),
            self.joints.len()
        );
        if self.smooth == Some(false) {
            s += " straight";
        }
        if let Some(b) = self.blend {
            s += &format!(" blend={b}");
        }
        if let Some(m) = &self.mirror {
            s += &format!(" mirror={m}");
        }
        for w in &self.warps {
            s += &format!(" warp({})", w.summary());
        }
        if let Some(r) = self.resolution {
            s += &format!(" resolution={r}");
        }
        s
    }

    /// The skin as an sdf: one chain shape per chain, joints resolved.
    pub fn to_sdf(&self) -> SdfDef {
        let shapes = self
            .chains
            .iter()
            .map(|c| Shape {
                shape: "chain".into(),
                points: Some(
                    c.iter()
                        .map(|p| match p {
                            ChainPt::Point(q) => *q,
                            ChainPt::Joint(n) => self.joints.get(n).copied().unwrap_or([0.0; 4]),
                        })
                        .collect(),
                ),
                smooth: self.smooth,
                ..Shape::default()
            })
            .collect();
        // Default blend: half the median radius, so limbs melt into the body.
        let mut radii: Vec<f32> = self
            .chains
            .iter()
            .flatten()
            .map(|p| match p {
                ChainPt::Point(q) => q[3],
                ChainPt::Joint(n) => self.joints.get(n).map_or(0.0, |q| q[3]),
            })
            .collect();
        radii.sort_by(f32::total_cmp);
        let median = radii.get(radii.len() / 2).copied().unwrap_or(0.0);
        SdfDef {
            shapes,
            blend: Some(self.blend.unwrap_or(0.5 * median)),
            mirror: self.mirror.clone(),
            warps: self.warps.clone(),
            resolution: self.resolution,
        }
    }
}

// ---------------------------------------------------------------- evaluation

#[derive(Clone, Copy, PartialEq)]
enum Join {
    Union,
    Subtract,
    Intersect,
}

enum Prim {
    Sphere(f32),
    Ellipsoid(Vec3),
    Box(Vec3, f32),
    /// Rounded cone between two world points.
    Cone(Vec3, Vec3, f32, f32),
    Cylinder(f32, f32, f32),
    Torus(f32, f32),
    Grid(Arc<crate::blob::Grid>),
    Group(Vec<Node>),
}

struct Node {
    prim: Prim,
    at: Vec3,
    inv: Quat,
    join: Join,
    k: f32,
    /// Chain id (segments of one chain join by a plain min, then the chain
    /// joins the rest by `k`); 0 = not a chain segment.
    chain: u32,
    lo: Vec3,
    hi: Vec3,
    /// Smallest feature (for the default resolution).
    thin: f32,
}

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

fn join(d: f32, e: f32, j: Join, k: f32) -> f32 {
    match j {
        Join::Union => smin(d, e, k),
        Join::Subtract => smax(d, -e, k),
        Join::Intersect => smax(d, e, k),
    }
}

/// Rounded cone (Quilez): spheres r1 at a and r2 at b joined by their
/// tangent cone. One sphere inside the other is just the bigger sphere.
fn round_cone(p: Vec3, a: Vec3, b: Vec3, r1: f32, r2: f32) -> f32 {
    let ba = b - a;
    let l2 = ba.dot(ba);
    let rr = r1 - r2;
    let a2 = l2 - rr * rr;
    if a2 <= 1e-12 {
        return if r1 >= r2 {
            (p - a).length() - r1
        } else {
            (p - b).length() - r2
        };
    }
    let il2 = 1.0 / l2;
    let pa = p - a;
    let y = pa.dot(ba);
    let z = y - l2;
    let x2 = (pa * l2 - ba * y).length_squared();
    let y2 = y * y * l2;
    let z2 = z * z * l2;
    let k = rr.signum() * rr * rr * x2;
    if z.signum() * a2 * z2 > k {
        return (x2 + z2).sqrt() * il2 - r2;
    }
    if y.signum() * a2 * y2 < k {
        return (x2 + y2).sqrt() * il2 - r1;
    }
    ((x2 * a2 * il2).sqrt() + y * rr) * il2 - r1
}

impl Node {
    fn eval(&self, p: Vec3, band: f32) -> f32 {
        let l = || self.inv * (p - self.at);
        match &self.prim {
            Prim::Sphere(r) => (p - self.at).length() - r,
            Prim::Ellipsoid(r) => {
                let q = l();
                let k0 = (q / *r).length();
                let k1 = (q / (*r * *r)).length();
                if k1 < 1e-12 {
                    -r.min_element()
                } else {
                    k0 * (k0 - 1.0) / k1
                }
            }
            Prim::Box(h, round) => {
                let q = l().abs() - (*h - Vec3::splat(*round));
                q.max(Vec3::ZERO).length() + q.max_element().min(0.0) - round
            }
            Prim::Cone(a, b, r1, r2) => round_cone(p, *a, *b, *r1, *r2),
            Prim::Cylinder(r, hh, round) => {
                let q = l();
                let d = bevy::math::Vec2::new(
                    bevy::math::Vec2::new(q.x, q.z).length() - (r - round),
                    q.y.abs() - (hh - round),
                );
                d.max_element().min(0.0) + d.max(bevy::math::Vec2::ZERO).length() - round
            }
            Prim::Torus(ring, tube) => {
                let q = l();
                bevy::math::Vec2::new(bevy::math::Vec2::new(q.x, q.z).length() - ring, q.y).length()
                    - tube
            }
            Prim::Grid(g) => g.sample(p),
            Prim::Group(children) => eval_list(children, p, band),
        }
    }
}

/// Folds an ordered list of nodes (no buckets: groups are small).
fn eval_list(nodes: &[Node], p: Vec3, band: f32) -> f32 {
    let mut d = band;
    let mut chain = (0u32, band, 0.0f32);
    for n in nodes {
        if n.chain != 0 {
            if n.chain != chain.0 {
                if chain.0 != 0 {
                    d = smin(d, chain.1, chain.2);
                }
                chain = (n.chain, band, n.k);
            }
            if inside_box(p, n, band) {
                chain.1 = chain.1.min(n.eval(p, band));
            }
            continue;
        }
        if chain.0 != 0 {
            d = smin(d, chain.1, chain.2);
            chain.0 = 0;
        }
        let e = if n.join == Join::Intersect || inside_box(p, n, band) {
            n.eval(p, band).min(band)
        } else {
            band
        };
        d = join(d, e, n.join, n.k);
    }
    if chain.0 != 0 {
        d = smin(d, chain.1, chain.2);
    }
    d.clamp(-band * 4.0, band)
}

fn inside_box(p: Vec3, n: &Node, band: f32) -> bool {
    let b = Vec3::splat(band);
    p.cmpge(n.lo - b).all() && p.cmple(n.hi + b).all()
}

fn rot_box(lo: Vec3, hi: Vec3, q: Quat, at: Vec3) -> (Vec3, Vec3) {
    let mut a = Vec3::splat(f32::MAX);
    let mut b = Vec3::splat(f32::MIN);
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let w = q * c + at;
        a = a.min(w);
        b = b.max(w);
    }
    (a, b)
}

/// Every closed part a `part` shape reads, by name.
pub type Lookup<'a> = dyn Fn(&str) -> Result<Arc<Part>, ModelerError> + 'a;

struct Builder<'a> {
    lookup: &'a Lookup<'a>,
    chains: u32,
    /// Grid spacing / band for part shapes (set before compiling them).
    res: f32,
    band: f32,
}

impl Builder<'_> {
    fn compile(
        &mut self,
        s: &Shape,
        k_default: f32,
        out: &mut Vec<Node>,
    ) -> Result<(), ModelerError> {
        let join = match s.op.as_deref() {
            Some("subtract") => Join::Subtract,
            Some("intersect") => Join::Intersect,
            _ => Join::Union,
        };
        let k = s.blend.unwrap_or(k_default);
        let at = Vec3::from(s.at.unwrap_or([0.0; 3]));
        let rot = s
            .rotation
            .map(|q| Quat::from_array(q).normalize())
            .unwrap_or(Quat::IDENTITY);
        let v3 = |v: &Option<Vec<f32>>| -> Vec3 {
            match v.as_deref() {
                Some([a]) => Vec3::splat(*a),
                Some([a, b, c, ..]) => Vec3::new(*a, *b, *c),
                _ => Vec3::splat(0.1),
            }
        };
        let r0 = s
            .radius
            .as_ref()
            .and_then(|r| r.first().copied())
            .unwrap_or(0.1);
        let node = |prim, lo: Vec3, hi: Vec3, thin: f32| {
            let (lo, hi) = rot_box(lo, hi, rot, at);
            Node {
                prim,
                at,
                inv: rot.inverse(),
                join,
                k,
                chain: 0,
                lo,
                hi,
                thin,
            }
        };
        match s.shape.as_str() {
            "sphere" => out.push(node(
                Prim::Sphere(r0),
                Vec3::splat(-r0),
                Vec3::splat(r0),
                2.0 * r0,
            )),
            "ellipsoid" => {
                let r = v3(&s.size);
                out.push(node(Prim::Ellipsoid(r), -r, r, 2.0 * r.min_element()));
            }
            "box" => {
                let h = v3(&s.size) * 0.5;
                let round = s.round.unwrap_or(0.0).min(h.min_element());
                out.push(node(Prim::Box(h, round), -h, h, 2.0 * h.min_element()));
            }
            "cylinder" => {
                let hh = s.height.unwrap_or(0.1) * 0.5;
                let round = s.round.unwrap_or(0.0).min(r0).min(hh);
                let e = Vec3::new(r0, hh, r0);
                out.push(node(Prim::Cylinder(r0, hh, round), -e, e, 2.0 * r0.min(hh)));
            }
            "torus" => {
                let sz = s.size.clone().unwrap_or_default();
                let (ring, tube) = (
                    sz.first().copied().unwrap_or(0.1),
                    sz.get(1).copied().unwrap_or(0.02),
                );
                let e = Vec3::new(ring + tube, tube, ring + tube);
                out.push(node(Prim::Torus(ring, tube), -e, e, 2.0 * tube));
            }
            "capsule" => {
                let a = Vec3::from(s.from.unwrap_or([0.0; 3]));
                let b = Vec3::from(s.to.unwrap_or([0.0; 3]));
                let r1 = r0;
                let r2 = s
                    .radius
                    .as_ref()
                    .and_then(|r| r.get(1).copied())
                    .unwrap_or(r1);
                let lo = (a - Vec3::splat(r1)).min(b - Vec3::splat(r2));
                let hi = (a + Vec3::splat(r1)).max(b + Vec3::splat(r2));
                out.push(Node {
                    prim: Prim::Cone(a, b, r1, r2),
                    at: Vec3::ZERO,
                    inv: Quat::IDENTITY,
                    join,
                    k,
                    chain: 0,
                    lo,
                    hi,
                    thin: 2.0 * r1.min(r2),
                });
            }
            "chain" => {
                let pts =
                    chain_samples(s.points.as_deref().unwrap_or(&[]), s.smooth.unwrap_or(true));
                self.chains += 1;
                let id = self.chains;
                let seg = |a: Vec4, b: Vec4| {
                    let (pa, pb) = (a.truncate(), b.truncate());
                    let lo = (pa - Vec3::splat(a.w)).min(pb - Vec3::splat(b.w));
                    let hi = (pa + Vec3::splat(a.w)).max(pb + Vec3::splat(b.w));
                    Node {
                        prim: Prim::Cone(pa, pb, a.w, b.w),
                        at: Vec3::ZERO,
                        inv: Quat::IDENTITY,
                        join: Join::Union,
                        k,
                        chain: id,
                        lo,
                        hi,
                        thin: 2.0 * a.w.min(b.w),
                    }
                };
                if pts.len() == 1 {
                    out.push(seg(pts[0], pts[0]));
                }
                for w in pts.windows(2) {
                    out.push(seg(w[0], w[1]));
                }
            }
            "part" => {
                let name = s.part.as_deref().unwrap_or("");
                let p = (self.lookup)(name)?;
                let g = crate::blob::sdf_grid(&p, self.res, self.band)?;
                let (lo, hi) = p.bounds();
                out.push(Node {
                    prim: Prim::Grid(Arc::new(g)),
                    at: Vec3::ZERO,
                    inv: Quat::IDENTITY,
                    join,
                    k,
                    chain: 0,
                    lo,
                    hi,
                    thin: (hi - lo).min_element(),
                });
            }
            _ => {
                let mut kids = Vec::new();
                for c in s.shapes.iter().flatten() {
                    self.compile(c, k_default, &mut kids)?;
                }
                let (lo, hi) = union_box(&kids);
                let thin = kids.iter().map(|n| n.thin).fold(f32::MAX, f32::min);
                out.push(Node {
                    prim: Prim::Group(kids),
                    at: Vec3::ZERO,
                    inv: Quat::IDENTITY,
                    join,
                    k,
                    chain: 0,
                    lo,
                    hi,
                    thin,
                });
            }
        }
        Ok(())
    }
}

/// The box of the nodes that add volume (unions), grown by their blends.
fn union_box(nodes: &[Node]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for n in nodes.iter().filter(|n| n.join == Join::Union) {
        lo = lo.min(n.lo - Vec3::splat(n.k * 0.25));
        hi = hi.max(n.hi + Vec3::splat(n.k * 0.25));
    }
    (lo, hi)
}

/// Chain samples as (x, y, z, r): the points, or a centripetal Catmull-Rom
/// through them (about one sample per 5 degrees of bend, and at most a
/// radius apart), radii interpolated along with the position.
fn chain_samples(pts: &[[f32; 4]], smooth: bool) -> Vec<Vec4> {
    let p: Vec<Vec4> = pts.iter().map(|q| Vec4::from(*q)).collect();
    if !smooth || p.len() < 3 {
        return p;
    }
    let n = p.len();
    let at = |i: isize| -> Vec4 {
        if i < 0 {
            2.0 * p[0] - p[1]
        } else if i as usize >= n {
            2.0 * p[n - 1] - p[n - 2]
        } else {
            p[i as usize]
        }
    };
    let mut out = Vec::new();
    for s in 0..n - 1 {
        let i = s as isize;
        let q = [at(i - 1), at(i), at(i + 1), at(i + 2)];
        let pos = q.map(Vec4::truncate);
        let t1 = (pos[2] - pos[0]).normalize_or_zero();
        let t2 = (pos[3] - pos[1]).normalize_or_zero();
        let deg = t1.dot(t2).clamp(-1.0, 1.0).acos().to_degrees();
        let len = pos[1].distance(pos[2]);
        let rmin = q[1].w.min(q[2].w).max(1e-4);
        let k = ((deg / 5.0).ceil() as usize)
            .max((len / rmin).ceil() as usize)
            .clamp(1, 64);
        let knot = |a: Vec3, b: Vec3| (a - b).length().sqrt().max(1e-6);
        let t0 = 0.0;
        let tk1 = t0 + knot(pos[0], pos[1]);
        let tk2 = tk1 + knot(pos[1], pos[2]);
        let tk3 = tk2 + knot(pos[2], pos[3]);
        for j in 0..k {
            let u = j as f32 / k as f32;
            let mut v = crate::loft::barry_goldman(q, [t0, tk1, tk2, tk3], tk1 + (tk2 - tk1) * u);
            // Overshoot must not pinch the tube to nothing.
            v.w = v.w.max(0.5 * q[1].w.min(q[2].w));
            out.push(v);
        }
    }
    out.push(p[n - 1]);
    out
}

/// The coarse grid of node lists (indices into the top-level nodes, in
/// order) a sample looks up.
struct Buckets {
    lo: Vec3,
    cell: Vec3,
    n: usize,
    lists: Vec<Vec<u32>>,
    /// Intersect nodes: evaluated everywhere.
    always: Vec<u32>,
}

impl Buckets {
    fn new(nodes: &[Node], band: f32) -> Buckets {
        let n = 24usize;
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for nd in nodes {
            lo = lo.min(nd.lo);
            hi = hi.max(nd.hi);
        }
        let b = Vec3::splat(band + 1e-6);
        lo -= b;
        hi += b;
        let cell = ((hi - lo) / n as f32).max(Vec3::splat(1e-6));
        let mut lists = vec![Vec::new(); n * n * n];
        let always: Vec<u32> = nodes
            .iter()
            .enumerate()
            .filter(|(_, nd)| nd.join == Join::Intersect)
            .map(|(i, _)| i as u32)
            .collect();
        let idx = |v: Vec3| -> [usize; 3] {
            let g = ((v - lo) / cell).floor();
            [g.x, g.y, g.z].map(|c| (c.max(0.0) as usize).min(n - 1))
        };
        for (i, nd) in nodes.iter().enumerate() {
            let a = idx(nd.lo - b);
            let z = idx(nd.hi + b);
            for gz in a[2]..=z[2] {
                for gy in a[1]..=z[1] {
                    for gx in a[0]..=z[0] {
                        let l = &mut lists[(gz * n + gy) * n + gx];
                        if nd.join == Join::Intersect {
                            continue;
                        }
                        l.push(i as u32);
                    }
                }
            }
        }
        // Merge the always-list into each cell, keeping node order.
        if !always.is_empty() {
            for l in &mut lists {
                l.extend(&always);
                l.sort_unstable();
            }
        }
        Buckets {
            lo,
            cell,
            n,
            lists,
            always,
        }
    }

    fn get(&self, p: Vec3) -> &[u32] {
        let g = (p - self.lo) / self.cell;
        if g.x < 0.0 || g.y < 0.0 || g.z < 0.0 {
            return &self.always;
        }
        let (x, y, z) = (g.x as usize, g.y as usize, g.z as usize);
        if x >= self.n || y >= self.n || z >= self.n {
            return &self.always;
        }
        &self.lists[(z * self.n + y) * self.n + x]
    }
}

/// A warp with the box it acts over (the field's box before it).
struct Stage {
    w: Deform,
    lo: Vec3,
    hi: Vec3,
}

impl Stage {
    fn axis(&self) -> usize {
        self.w
            .axis
            .as_deref()
            .and_then(parse_dir)
            .map_or(1, |(k, _)| k)
    }

    fn bend_dir(&self, axis: usize) -> (usize, f32) {
        self.w
            .toward
            .as_deref()
            .and_then(parse_dir)
            .unwrap_or(if axis == 0 { (1, 1.0) } else { (0, 1.0) })
    }

    /// The deform's forward map (as `deform` moves a vertex).
    fn forward(&self, mut p: Vec3) -> Vec3 {
        let axis = self.axis();
        let len = (self.hi[axis] - self.lo[axis]).max(1e-6);
        let c = (self.lo + self.hi) * 0.5;
        let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
        let (cu, cw) = if u < w { (u, w) } else { (w, u) };
        let t = (p[axis] - self.lo[axis]) / len;
        match self.w.mode.as_str() {
            "twist" => {
                let a = self.w.degrees.unwrap_or(0.0).to_radians() * t;
                let (s, co) = a.sin_cos();
                let (x, y) = (p[cu] - c[cu], p[cw] - c[cw]);
                p[cu] = c[cu] + x * co - y * s;
                p[cw] = c[cw] + x * s + y * co;
            }
            "taper" => {
                let (su, sw) = self.taper(t);
                p[cu] = c[cu] + (p[cu] - c[cu]) * su;
                p[cw] = c[cw] + (p[cw] - c[cw]) * sw;
            }
            "bend" => {
                let theta = self.w.degrees.unwrap_or(0.0).to_radians();
                if theta.abs() < 1e-6 {
                    return p;
                }
                let (d, sign) = self.bend_dir(axis);
                let r = len / theta;
                let off = (p[d] - c[d]) * sign;
                let (s, co) = (t * theta).sin_cos();
                p[axis] = self.lo[axis] + (r - off) * s;
                p[d] = c[d] + sign * (r - (r - off) * co);
            }
            _ => {}
        }
        p
    }

    fn taper(&self, t: f32) -> (f32, f32) {
        let e = self.w.scale_end.unwrap_or([1.0; 2]).map(|v| v.max(1e-3));
        let k = t
            .clamp(-1.0, 2.0)
            .max(0.0)
            .powf(self.w.power.unwrap_or(1.0));
        (
            (1.0 + (e[0] - 1.0) * k).max(1e-3),
            (1.0 + (e[1] - 1.0) * k).max(1e-3),
        )
    }

    /// Where `q` came from before this warp.
    fn inverse(&self, mut q: Vec3) -> Vec3 {
        let axis = self.axis();
        let len = (self.hi[axis] - self.lo[axis]).max(1e-6);
        let c = (self.lo + self.hi) * 0.5;
        let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
        let (cu, cw) = if u < w { (u, w) } else { (w, u) };
        match self.w.mode.as_str() {
            "twist" => {
                let t = (q[axis] - self.lo[axis]) / len;
                let a = -self.w.degrees.unwrap_or(0.0).to_radians() * t;
                let (s, co) = a.sin_cos();
                let (x, y) = (q[cu] - c[cu], q[cw] - c[cw]);
                q[cu] = c[cu] + x * co - y * s;
                q[cw] = c[cw] + x * s + y * co;
            }
            "taper" => {
                let t = (q[axis] - self.lo[axis]) / len;
                let (su, sw) = self.taper(t);
                q[cu] = c[cu] + (q[cu] - c[cu]) / su;
                q[cw] = c[cw] + (q[cw] - c[cw]) / sw;
            }
            "bend" => {
                let theta = self.w.degrees.unwrap_or(0.0).to_radians();
                if theta.abs() < 1e-6 {
                    return q;
                }
                let (d, sign) = self.bend_dir(axis);
                let r = len / theta;
                let a = q[axis] - self.lo[axis];
                let b = r - (q[d] - c[d]) * sign;
                let (phi, rho) = if r > 0.0 {
                    (a.atan2(b), a.hypot(b))
                } else {
                    ((-a).atan2(-b), -a.hypot(b))
                };
                q[axis] = self.lo[axis] + phi / theta * len;
                q[d] = c[d] + (r - rho) * sign;
            }
            _ => {}
        }
        q
    }
}

fn fold(mut p: Vec3, mirror: Option<&str>) -> Vec3 {
    match mirror {
        Some("x") => p.x = p.x.abs(),
        Some("z") => p.z = p.z.abs(),
        Some("xz" | "zx") => {
            p.x = p.x.abs();
            p.z = p.z.abs();
        }
        _ => {}
    }
    p
}

/// A displacement warp's noise at `p`, in meters (+ = outward).
fn noise_at(w: &Deform, p: Vec3, size: Vec3) -> f32 {
    let scale = w.scale.unwrap_or(0.45 * size.max_element());
    let amount = w.amount.unwrap_or(0.12 * size.min_element());
    let seed = w.seed.unwrap_or(0);
    let mode = w.noise.as_deref().unwrap_or("fbm");
    let shift = Vec3::new(17.13, 31.71, 7.37) * scale + Vec3::splat(seed as f32 * 0.618 * scale);
    let lattice = Lattice::new(scale, 0.0);
    let base = |q: [f32; 3], l: Lattice, s: u32| gradient(q, 3, l, s);
    let f = fractal(
        base,
        (p + shift).to_array(),
        lattice,
        seed,
        w.octaves.unwrap_or(4),
        2.0,
        0.5,
        mode,
    );
    let h = match mode {
        "fbm" => ((f - 0.5) * 3.5).clamp(-1.0, 1.0),
        _ => (f - 0.5) * 2.0,
    };
    h * amount
}

/// Meshes an `sdf` item into a closed solid named `name`.
pub fn build(name: &str, def: &SdfDef, lookup: &Lookup) -> Result<Part, ModelerError> {
    let err = |m: String| ModelerError::Csg(format!("sdf `{name}`: {m}"));
    def.validate()
        .map_err(|e| ModelerError::Invalid(format!("sdf `{name}`: {e}")))?;
    // Default blend: a fifth of the thinnest shape (parts sampled later).
    let mut b = Builder {
        lookup,
        chains: 0,
        res: 0.0,
        band: 0.0,
    };
    // First pass without part grids to size things: compile analytic
    // shapes; part shapes need a resolution, so size from their boxes.
    let mut size_lo = Vec3::splat(f32::MAX);
    let mut size_hi = Vec3::splat(f32::MIN);
    let mut thin = f32::MAX;
    measure(&def.shapes, lookup, &mut size_lo, &mut size_hi, &mut thin)?;
    if size_lo.x > size_hi.x {
        return Err(err(
            "no shape adds volume (every shape subtracts or intersects)".into(),
        ));
    }
    let k = def.blend.unwrap_or(0.2 * thin);
    // Warp stages and the final box.
    let mut stages = Vec::new();
    let (mut lo, mut hi) = (size_lo - Vec3::splat(k), size_hi + Vec3::splat(k));
    let mut disp = 0.0f32;
    for w in &def.warps {
        let st = Stage {
            w: w.clone(),
            lo,
            hi,
        };
        if w.mode == "displace" {
            let size = (hi - lo).max(Vec3::splat(1e-6));
            let a = w.amount.unwrap_or(0.12 * size.min_element()).abs();
            disp += a;
            lo -= Vec3::splat(a);
            hi += Vec3::splat(a);
        } else {
            let (a, z) = warped_box(&st, lo, hi);
            lo = a;
            hi = z;
        }
        stages.push(st);
    }
    if let Some(m) = def.mirror.as_deref() {
        if m.contains('x') {
            let e = lo.x.abs().max(hi.x.abs());
            lo.x = -e;
            hi.x = e;
        }
        if m.contains('z') {
            let e = lo.z.abs().max(hi.z.abs());
            lo.z = -e;
            hi.z = e;
        }
    }
    let size = (hi - lo).max_element();
    let samples = |r: f32| -> f64 {
        let e = (hi - lo) / r + Vec3::splat(3.0);
        e.x as f64 * e.y as f64 * e.z as f64
    };
    let res = match def.resolution {
        Some(r) => {
            if samples(r) > MAX_SAMPLES {
                let need = r * (samples(r) / MAX_SAMPLES).cbrt() as f32;
                return Err(err(format!(
                    "resolution {r} m would sample {:.0}M points over a {size:.3} m box (limit {:.0}M); use resolution >= {:.4} (or omit it)",
                    samples(r) / 1e6,
                    MAX_SAMPLES / 1e6,
                    need * 1.01
                )));
            }
            r
        }
        None => {
            let mut r = (size / 120.0).min(thin / 3.0).max(1e-4);
            if samples(r) > AUTO_SAMPLES {
                r *= (samples(r) / AUTO_SAMPLES).cbrt() as f32 * 1.01;
            }
            r
        }
    };
    let band = k + disp + 4.0 * res;
    b.res = res;
    b.band = band;
    let mut nodes = Vec::new();
    for s in &def.shapes {
        b.compile(s, k, &mut nodes)?;
    }
    let buckets = Buckets::new(&nodes, band);
    let mirror = def.mirror.as_deref();
    let disp_stages: Vec<&Stage> = stages.iter().filter(|s| s.w.mode == "displace").collect();
    let space_stages: Vec<&Stage> = stages.iter().filter(|s| s.w.mode != "displace").collect();
    let field = |p: Vec3| -> f32 {
        let mut q = fold(p, mirror);
        let mut off = 0.0;
        for s in &disp_stages {
            off += noise_at(&s.w, q, (s.hi - s.lo).max(Vec3::splat(1e-6)));
        }
        for s in space_stages.iter().rev() {
            q = s.inverse(q);
        }
        let mut d = band;
        let mut chain = (0u32, band, 0.0f32);
        for &i in buckets.get(q) {
            let n = &nodes[i as usize];
            if n.chain != 0 {
                if n.chain != chain.0 {
                    if chain.0 != 0 {
                        d = smin(d, chain.1, chain.2);
                    }
                    chain = (n.chain, band, n.k);
                }
                if inside_box(q, n, band) {
                    chain.1 = chain.1.min(n.eval(q, band));
                }
                continue;
            }
            if chain.0 != 0 {
                d = smin(d, chain.1, chain.2);
                chain.0 = 0;
            }
            let e = if n.join == Join::Intersect || inside_box(q, n, band) {
                n.eval(q, band).min(band)
            } else {
                band
            };
            d = join(d, e, n.join, n.k);
        }
        if chain.0 != 0 {
            d = smin(d, chain.1, chain.2);
        }
        d.clamp(-4.0 * band, band) - off
    };
    let pad = Vec3::splat(2.0 * res);
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
        return Err(err(format!(
            "nothing to mesh (the subtracts remove everything, or shapes are thinner than the {res:.4} m resolution)"
        )));
    }
    let mut out = crate::csg::manifold_to_part(&m, name, crate::Material::default());
    crate::uv::crease_normals(&mut out, 60.0);
    out.visible = true;
    Ok(out)
}

/// The union box of the volume-adding shapes and the thinnest feature.
fn measure(
    shapes: &[Shape],
    lookup: &Lookup,
    lo: &mut Vec3,
    hi: &mut Vec3,
    thin: &mut f32,
) -> Result<(), ModelerError> {
    let mut b = Builder {
        lookup,
        chains: 0,
        res: 0.0,
        band: 0.0,
    };
    for s in shapes {
        let mut nodes = Vec::new();
        if s.shape == "part" {
            let p = lookup(s.part.as_deref().unwrap_or(""))?;
            let (a, z) = p.bounds();
            *thin = thin.min((z - a).min_element());
            if s.op.as_deref().is_none_or(|o| o == "union") {
                *lo = lo.min(a);
                *hi = hi.max(z);
            }
            continue;
        }
        if s.shape == "group" && s.shapes.iter().flatten().any(has_part) {
            measure(s.shapes.as_deref().unwrap_or(&[]), lookup, lo, hi, thin)?;
            continue;
        }
        b.compile(s, 0.0, &mut nodes)?;
        for n in &nodes {
            *thin = thin.min(n.thin);
            if n.join == Join::Union {
                *lo = lo.min(n.lo);
                *hi = hi.max(n.hi);
            }
        }
    }
    Ok(())
}

fn has_part(s: &Shape) -> bool {
    s.shape == "part" || s.shapes.iter().flatten().any(has_part)
}

/// The box a space warp moves `lo..hi` into (sampled on a 9^3 lattice of
/// the box, grown by 5%).
fn warped_box(st: &Stage, lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    let mut a = Vec3::splat(f32::MAX);
    let mut z = Vec3::splat(f32::MIN);
    for i in 0..9 {
        for j in 0..9 {
            for k in 0..9 {
                let t = Vec3::new(i as f32, j as f32, k as f32) / 8.0;
                let p = st.forward(lo + (hi - lo) * t);
                a = a.min(p);
                z = z.max(p);
            }
        }
    }
    let g = (z - a) * 0.05;
    (a - g, z + g)
}

/// Meshes a `skin` item.
pub fn skin(name: &str, def: &SkinDef) -> Result<Part, ModelerError> {
    def.validate()
        .map_err(|e| ModelerError::Invalid(format!("skin `{name}`: {e}")))?;
    let none = |n: &str| -> Result<Arc<Part>, ModelerError> {
        Err(ModelerError::Invalid(format!(
            "skin reads no parts (`{n}`)"
        )))
    };
    build(name, &def.to_sdf(), &none).map_err(|e| match e {
        ModelerError::Csg(m) => ModelerError::Csg(m.replacen("sdf `", "skin `", 1)),
        o => o,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_parts(n: &str) -> Result<Arc<Part>, ModelerError> {
        Err(ModelerError::Invalid(format!("no part {n}")))
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
        assert!(volume(p) > 0.0, "{} is inside out", p.name);
    }

    fn shape(v: serde_json::Value) -> Shape {
        serde_json::from_value(v).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_sphere_meshes_to_its_volume() {
        let def = SdfDef {
            shapes: vec![shape(
                serde_json::json!({"shape": "sphere", "radius": [0.1]}),
            )],
            ..SdfDef::default()
        };
        let p = build("s", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let v = volume(&p);
        let want = 4.0 / 3.0 * std::f64::consts::PI * 1e-3;
        assert!((v - want).abs() / want < 0.03, "volume {v} vs {want}");
    }

    #[test]
    fn smooth_union_fills_the_neck_and_subtract_carves() {
        let two = |blend: f32| SdfDef {
            shapes: vec![
                shape(
                    serde_json::json!({"shape": "sphere", "radius": [0.1], "at": [-0.09, 0.0, 0.0]}),
                ),
                shape(
                    serde_json::json!({"shape": "sphere", "radius": [0.1], "at": [0.09, 0.0, 0.0]}),
                ),
            ],
            blend: Some(blend),
            resolution: Some(0.006),
            ..SdfDef::default()
        };
        let sharp = build("a", &two(0.0), &no_parts).unwrap_or_else(|e| panic!("{e}"));
        let soft = build("b", &two(0.08), &no_parts).unwrap_or_else(|e| panic!("{e}"));
        closed(&sharp);
        closed(&soft);
        assert!(volume(&soft) > volume(&sharp) * 1.02);
        let mut cut = two(0.0);
        cut.shapes.push(shape(
            serde_json::json!({"shape": "box", "size": [0.4, 0.1, 0.4], "at": [0.0, 0.1, 0.0], "op": "subtract"}),
        ));
        let c = build("c", &cut, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        closed(&c);
        assert!(c.bounds().1.y < 0.06, "top {}", c.bounds().1.y);
    }

    #[test]
    fn mirror_and_twist_warp() {
        let def = SdfDef {
            shapes: vec![shape(
                serde_json::json!({"shape": "capsule", "from": [0.05, 0.0, 0.0], "to": [0.3, 0.0, 0.0], "radius": [0.04, 0.02]}),
            )],
            mirror: Some("x".into()),
            ..SdfDef::default()
        };
        let p = build("m", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let (lo, hi) = p.bounds();
        assert!((lo.x + hi.x).abs() < 0.01 && hi.x > 0.3, "{lo} {hi}");
        let bent = SdfDef {
            shapes: vec![shape(
                serde_json::json!({"shape": "box", "size": [0.1, 1.0, 0.1]}),
            )],
            warps: vec![Deform {
                mode: "bend".into(),
                degrees: Some(90.0),
                toward: Some("x".into()),
                ..Deform::default()
            }],
            ..SdfDef::default()
        };
        let p = build("b", &bent, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let (lo, hi) = p.bounds();
        // A 1 m bar bent 90 degrees: an arc of radius ~0.64, so it reaches
        // ~0.6 to the side and rises only ~0.64.
        assert!(hi.x > 0.5 && hi.y < 0.25, "{lo} {hi}");
    }

    #[test]
    fn a_skin_joins_chains_at_shared_joints() {
        let def: SkinDef = serde_json::from_value(serde_json::json!({
            "joints": {"hip": [0.0, 0.5, 0.0, 0.08]},
            "chains": [
                ["hip", [0.0, 0.8, 0.0, 0.06], [0.0, 1.0, 0.05, 0.07]],
                ["hip", [0.15, 0.3, 0.0, 0.04], [0.2, 0.02, 0.05, 0.03]],
                ["hip", [-0.15, 0.3, 0.0, 0.04], [-0.2, 0.02, 0.05, 0.03]]
            ]
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let p = skin("guy", &def).unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let (lo, hi) = p.bounds();
        assert!(lo.y < 0.0 && hi.y > 1.05, "{lo} {hi}");
        let bad: SkinDef = serde_json::from_value(serde_json::json!({"chains": [["knee"]]}))
            .unwrap_or_else(|e| panic!("{e}"));
        let e = skin("x", &bad)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("no joint `knee`"), "{e}");
    }

    #[test]
    fn validation_names_the_shape_and_field() {
        let def = SdfDef {
            shapes: vec![shape(
                serde_json::json!({"shape": "sphere", "radius": [0.1], "height": 2.0}),
            )],
            ..SdfDef::default()
        };
        let e = def.validate().err().unwrap_or_default();
        assert!(
            e.contains("shapes[0] (sphere): `height` does not apply"),
            "{e}"
        );
    }
}
