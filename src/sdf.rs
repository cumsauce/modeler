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
    /// chain points [x, y, z, radius] or [x, y, z, [rx, ry], roll?].
    #[serde(default, skip_serializing_if = "none")]
    pub points: Option<Vec<Sect>>,
    /// chain: a smooth curve through the points (default true).
    #[serde(default, skip_serializing_if = "none")]
    pub smooth: Option<bool>,
    /// part: a closed solid of the model, sampled as a distance field.
    #[serde(default, skip_serializing_if = "none")]
    pub part: Option<String>,
    /// group: shapes combined on their own first, then joined as one.
    #[serde(default, skip_serializing_if = "none")]
    pub shapes: Option<Vec<Shape>>,
    /// silhouette: the side outline, rings of [z, y] (seen from +x).
    #[serde(default, skip_serializing_if = "none")]
    pub side: Option<Vec<Vec<[f32; 2]>>>,
    /// silhouette: the plan outline, rings of [x, z].
    #[serde(default, skip_serializing_if = "none")]
    pub top: Option<Vec<Vec<[f32; 2]>>>,
    /// silhouette: the front outline, rings of [x, y] (optional).
    #[serde(default, skip_serializing_if = "none")]
    pub front: Option<Vec<Vec<[f32; 2]>>>,
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
    "silhouette",
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
    /// Decimate the meshed field to about this many triangles (the dense
    /// mesh is never kept): one item instead of sdf + decimate + hide.
    #[serde(default, skip_serializing_if = "none")]
    pub tris: Option<u32>,
    /// With `tris`: bake the dense surface into the light mesh's normal and
    /// height maps (as decimate's `bake`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bake: bool,
    /// Set by the graph (never read from input or saved) when the result
    /// only feeds decimates: the most triangles any consumer keeps,
    /// so the default resolution meshes only as finely as that needs.
    #[serde(default, skip_deserializing, skip_serializing_if = "none")]
    pub target_tris: Option<u32>,
}

/// A chain section: `[x, y, z, radius]` (round) or `[x, y, z, [rx, ry],
/// roll?]` (an ellipse: `rx` across the tube sideways, `ry` along the
/// tube's up, rolled `roll` degrees about the tangent), or `[x, y, z,
/// [rx, ry], roll, n]`: a superellipse `|u/rx|^n + |v/ry|^n = 1` (2 = the
/// ellipse, 3-5 a rounded box, higher boxier).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Sect {
    Round([f32; 4]),
    Oval(f32, f32, f32, [f32; 2]),
    Rolled(f32, f32, f32, [f32; 2], f32),
    Super(f32, f32, f32, [f32; 2], f32, f32),
}

impl Sect {
    pub fn pos(&self) -> Vec3 {
        match *self {
            Sect::Round(q) => Vec3::new(q[0], q[1], q[2]),
            Sect::Oval(x, y, z, _)
            | Sect::Rolled(x, y, z, _, _)
            | Sect::Super(x, y, z, _, _, _) => Vec3::new(x, y, z),
        }
    }
    /// (sideways, up) radii.
    pub fn radii(&self) -> [f32; 2] {
        match *self {
            Sect::Round(q) => [q[3], q[3]],
            Sect::Oval(_, _, _, r)
            | Sect::Rolled(_, _, _, r, _)
            | Sect::Super(_, _, _, r, _, _) => r,
        }
    }
    /// The superellipse exponent (2 = an ellipse).
    pub fn n(&self) -> f32 {
        match *self {
            Sect::Super(_, _, _, _, _, n) => n,
            _ => 2.0,
        }
    }
    pub fn roll(&self) -> f32 {
        match *self {
            Sect::Rolled(_, _, _, _, a) | Sect::Super(_, _, _, _, a, _) => a,
            _ => 0.0,
        }
    }
    pub fn round(&self) -> bool {
        matches!(self, Sect::Round(_))
    }
    /// One radius standing for the section (the mean of the two).
    pub fn r(&self) -> f32 {
        let [a, b] = self.radii();
        0.5 * (a + b)
    }
    /// `[x, y, z, mean radius]`.
    pub fn xyzr(&self) -> [f32; 4] {
        let p = self.pos();
        [p.x, p.y, p.z, self.r()]
    }
    pub fn valid(&self) -> bool {
        let [a, b] = self.radii();
        finite_pos(a)
            && finite_pos(b)
            && self.pos().is_finite()
            && self.roll().is_finite()
            && (1.0..=40.0).contains(&self.n())
    }
    pub fn moved(&self, d: Vec3, k: f32) -> Sect {
        let p = self.pos() + d;
        let [a, b] = self.radii();
        if self.round() {
            Sect::Round([p.x, p.y, p.z, a * k])
        } else if self.n() != 2.0 {
            Sect::Super(p.x, p.y, p.z, [a * k, b * k], self.roll(), self.n())
        } else {
            Sect::Rolled(p.x, p.y, p.z, [a * k, b * k], self.roll())
        }
    }
}

/// A chain point: a section or the name of a joint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChainPt {
    Joint(String),
    Point(Sect),
}

/// The parameters of a `skin` op.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SkinDef {
    pub chains: Vec<Vec<ChainPt>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub joints: BTreeMap<String, Sect>,
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
    /// Decimate the meshed field to about this many triangles (the dense
    /// mesh is never kept): one item instead of sdf + decimate + hide.
    #[serde(default, skip_serializing_if = "none")]
    pub tris: Option<u32>,
    /// With `tris`: bake the dense surface into the light mesh's normal and
    /// height maps (as decimate's `bake`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bake: bool,
    /// Set by the graph (never read from input or saved) when the result
    /// only feeds decimates: the most triangles any consumer keeps,
    /// so the default resolution meshes only as finely as that needs.
    #[serde(default, skip_deserializing, skip_serializing_if = "none")]
    pub target_tris: Option<u32>,
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
            "silhouette" => &["side", "top", "front", "round", "at", "rotation"],
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
            ("side", self.side.is_some()),
            ("top", self.top.is_some()),
            ("front", self.front.is_some()),
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
                    if let Some(q) = p.iter().find(|q| !q.valid()) {
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
            "silhouette" => {
                for (f, v, need) in [
                    ("side", &self.side, true),
                    ("top", &self.top, true),
                    ("front", &self.front, false),
                ] {
                    match v {
                        None if need => {
                            return e(format!(
                                "{f}: required (a closed outline; side is [z, y], top [x, z], front [x, y])"
                            ));
                        }
                        None => {}
                        Some(r) => crate::silhouette::check_rings(r)
                            .map_err(|m| format!("{at} (silhouette): {f}: {m}"))?,
                    }
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

pub(crate) fn check_warps(w: &[Deform]) -> Result<(), String> {
    for (i, d) in w.iter().enumerate() {
        if crate::deform::LOCAL_MODES.contains(&d.mode.as_str()) {
            d.validate_local()
                .map_err(|e| format!("warps[{i}] ({}): {e}", d.mode))?;
            continue;
        }
        d.validate().map_err(|e| {
            if e.starts_with("mode:") {
                format!(
                    "warps[{i}]: mode: twist, bend, taper, displace or a local sculpt warp ({}), got `{}`",
                    crate::deform::LOCAL_MODES.join(", "),
                    d.mode
                )
            } else {
                format!("warps[{i}]: {e}")
            }
        })?;
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

fn check_tris(tris: Option<u32>, bake: bool) -> Result<(), String> {
    match tris {
        Some(t) if t < 4 => Err(format!("tris: at least 4 triangles, got {t}")),
        None if bake => Err("bake: needs `tris` (the light mesh to bake onto)".into()),
        _ => Ok(()),
    }
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
        check_tris(self.tris, self.bake)?;
        check_res(self.resolution)
    }

    pub fn reads(&self) -> Vec<&str> {
        let mut v = Vec::new();
        for s in &self.shapes {
            s.reads(&mut v);
        }
        v.extend(warp_reads(&self.warps));
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
            if !v.valid() {
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
                    ChainPt::Point(q) if !q.valid() => {
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
        check_tris(self.tris, self.bake)?;
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
                            ChainPt::Joint(n) => {
                                self.joints.get(n).copied().unwrap_or(Sect::Round([0.0; 4]))
                            }
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
                ChainPt::Point(q) => q.r(),
                ChainPt::Joint(n) => self.joints.get(n).map_or(0.0, Sect::r),
            })
            .collect();
        radii.sort_by(f32::total_cmp);
        let median = radii.get(radii.len() / 2).copied().unwrap_or(0.0);
        SdfDef {
            shapes,
            blend: Some(self.blend.unwrap_or(0.5 * median)),
            mirror: self.mirror.clone(),
            warps: resolve_joint_anchors(&self.warps, |n| {
                self.joints.get(n).map(|q| q.pos().to_array())
            }),
            resolution: self.resolution,
            tris: self.tris,
            bake: self.bake,
            target_tris: self.target_tris,
        }
    }
}

/// Parts local warps read through "part:anchor" centres (sdf, skin and
/// body all resolve them at build time, so they are graph dependencies).
pub fn warp_reads(warps: &[Deform]) -> Vec<&str> {
    warps
        .iter()
        .filter_map(|w| match &w.at {
            Some(crate::deform::WarpAt::Anchor(a)) => a.split_once(':').map(|(p, _)| p),
            _ => None,
        })
        .collect()
}

/// Local warps centred on a joint name get that joint's point.
pub fn resolve_joint_anchors(
    warps: &[Deform],
    joint: impl Fn(&str) -> Option<[f32; 3]>,
) -> Vec<Deform> {
    warps
        .iter()
        .map(|w| {
            let mut w = w.clone();
            if let Some(crate::deform::WarpAt::Anchor(a)) = &w.at
                && let Some(p) = joint(a)
            {
                w.at = Some(crate::deform::WarpAt::Point(p));
            }
            w
        })
        .collect()
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
    /// Elliptical rounded cone (a chain with oval sections).
    ECone(Box<ECone>),
    Cylinder(f32, f32, f32),
    Torus(f32, f32),
    Grid(Arc<crate::blob::Grid>),
    Group(Vec<Node>),
    /// Intersected extrusions of 2D outlines (side [z, y], top [x, z],
    /// front [x, y]) with a smooth radius.
    Silhouette(Box<crate::silhouette::Sil>),
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
    // Cubic polynomial smooth-min (C2): the quadratic one is only C1, so
    // its curvature jumps where the blend ends, which shades as a faint
    // ridge round every join (a cat's shoulders and hips). Its width is
    // 1.5 k, so the fillet is as deep as the quadratic's (k / 4).
    let w = SMIN_WIDTH * k;
    let h = (w - (a - b).abs()).max(0.0) / w;
    a.min(b) - h * h * h * w / 6.0
}

/// How far (in units of the blend `k`) a smooth join reaches.
const SMIN_WIDTH: f32 = 1.5;

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
    /// A cheap lower bound of `eval` for oval chain segments (the distance
    /// to the axis less the largest reach), to skip far segments.
    fn lower(&self, p: Vec3) -> f32 {
        let Prim::ECone(e) = &self.prim else {
            return f32::MIN;
        };
        let ba = e.b - e.a;
        let l2 = ba.length_squared().max(1e-12);
        let h = ((p - e.a).dot(ba) / l2).clamp(0.0, 1.0);
        let reach = e.ra.max_element().max(e.rb.max_element()) * 1.3 + e.ext_a.max(e.ext_b);
        // The ellipse estimate is not a true distance: scale it down.
        ((p - e.a - ba * h).length() - reach) * 0.8
    }

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
            Prim::ECone(e) => e.eval(p),
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
            Prim::Silhouette(sil) => sil.eval(l()),
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
            if inside_box(p, n, band) && n.lower(p) < chain.1 {
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

/// The nearest surface of one chain's segments (`idx` into `nodes`): the
/// segment with the smallest lower bound first, then only those whose
/// bound beats the best so far.
fn chain_min(nodes: &[Node], idx: &[u32], p: Vec3, band: f32) -> f32 {
    let mut first = None;
    let mut lb_min = f32::MAX;
    for &k in idx {
        let n = &nodes[k as usize];
        if inside_box(p, n, band) {
            let lb = n.lower(p);
            if lb < lb_min {
                lb_min = lb;
                first = Some(k);
            }
        }
    }
    let Some(f) = first else { return band };
    let mut best = nodes[f as usize].eval(p, band);
    for &k in idx {
        let n = &nodes[k as usize];
        if k != f && inside_box(p, n, band) && n.lower(p) < best {
            best = best.min(n.eval(p, band));
        }
    }
    best.min(band)
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
                let round = s.points.iter().flatten().all(Sect::round);
                let frames = chain_frames(&pts);
                // Straight run past an inner end: the radius times the
                // tangent of the bend to the neighbouring segment, plus a
                // little.
                let ext = |i: usize, j: usize, k: usize| -> f32 {
                    let Some(q) = pts.get(k) else { return 0.0 };
                    let d0 = (pts[j].p - pts[i].p).normalize_or_zero();
                    let d1 = if k > j {
                        (q.p - pts[j].p).normalize_or_zero()
                    } else {
                        (pts[i].p - q.p).normalize_or_zero()
                    };
                    let c = d0.dot(d1).clamp(0.2, 1.0);
                    let tan = (1.0 - c * c).sqrt() / c;
                    let at = if k > j { pts[j].r } else { pts[i].r };
                    at[0].max(at[1]) * (tan + 0.01)
                };
                // Radius slope per meter at sample i (central difference).
                let slope = |i: usize| -> bevy::math::Vec2 {
                    let a = i.saturating_sub(1);
                    let b = (i + 1).min(pts.len() - 1);
                    let l = pts[a].p.distance(pts[i].p) + pts[i].p.distance(pts[b].p);
                    if l < 1e-9 {
                        bevy::math::Vec2::ZERO
                    } else {
                        (bevy::math::Vec2::from(pts[b].r) - bevy::math::Vec2::from(pts[a].r)) / l
                    }
                };
                let seg = |i: usize, j: usize| {
                    let (ea, eb) = (ext(i, j, i.wrapping_sub(1)), ext(i, j, j + 1));
                    let (a, b) = (&pts[i], &pts[j]);
                    let (ma, mb) = (a.r[0].max(a.r[1]), b.r[0].max(b.r[1]));
                    let lo = (a.p - Vec3::splat(ma)).min(b.p - Vec3::splat(mb));
                    let hi = (a.p + Vec3::splat(ma)).max(b.p + Vec3::splat(mb));
                    let prim = if round {
                        Prim::Cone(a.p, b.p, a.r[0], b.r[0])
                    } else {
                        Prim::ECone(Box::new(ECone {
                            a: a.p,
                            b: b.p,
                            sa: frames[i].0,
                            ua: frames[i].1,
                            sb: frames[j].0,
                            ub: frames[j].1,
                            ra: a.r.into(),
                            rb: b.r.into(),
                            cap_a: i == 0,
                            cap_b: j + 1 == pts.len(),
                            ext_a: ea,
                            ext_b: eb,
                            // A capped end is flat (zero radius slope),
                            // so the half-ellipsoid cap meets the tube
                            // tangentially: a sloped end left a crease
                            // ring where a limb starts inside a torso.
                            da: if i == 0 {
                                bevy::math::Vec2::ZERO
                            } else {
                                slope(i)
                            },
                            db: if j + 1 == pts.len() {
                                bevy::math::Vec2::ZERO
                            } else {
                                slope(j)
                            },
                            na: a.n,
                            nb: b.n,
                        }))
                    };
                    Node {
                        prim,
                        at: Vec3::ZERO,
                        inv: Quat::IDENTITY,
                        join: Join::Union,
                        k,
                        chain: id,
                        lo,
                        hi,
                        thin: 2.0 * a.r[0].min(a.r[1]).min(b.r[0]).min(b.r[1]),
                    }
                };
                if pts.len() == 1 {
                    out.push(seg(0, 0));
                }
                for i in 1..pts.len() {
                    out.push(seg(i - 1, i));
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
            "silhouette" => {
                let none = Vec::new();
                let sil = crate::silhouette::Sil::new(
                    s.side.as_ref().unwrap_or(&none),
                    s.top.as_ref().unwrap_or(&none),
                    s.front.as_ref(),
                    s.round.unwrap_or(0.0),
                    if self.res > 0.0 { self.res * 0.5 } else { 0.0 },
                    self.band,
                );
                let (lo, hi) = sil.bounds();
                let thin = (hi - lo).min_element();
                out.push(node(Prim::Silhouette(Box::new(sil)), lo, hi, thin));
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

/// One sample along a chain: position, (sideways, up) radii, roll degrees.
#[derive(Clone, Copy)]
struct Samp {
    p: Vec3,
    r: [f32; 2],
    roll: f32,
    /// Superellipse exponent.
    n: f32,
}

/// Chain samples: the points, or a centripetal Catmull-Rom through them
/// (about one sample per 5 degrees of bend, and at most a radius apart),
/// radii and roll interpolated along with the position.
fn chain_samples(pts: &[Sect], smooth: bool) -> Vec<Samp> {
    // Oval sections join segments by straight runs, so they sample finer
    // (about 1.5 degrees of bend and half a radius apart).
    let fine = pts.iter().any(|q| !q.round());
    let (step_deg, step_r) = if fine { (1.0, 0.75) } else { (5.0, 1.0) };
    let samp = |a: Vec4, b: Vec4| Samp {
        p: a.truncate(),
        r: [a.w, b.x],
        roll: b.y,
        n: b.z.max(1.0),
    };
    let p: Vec<(Vec4, Vec4)> = pts
        .iter()
        .map(|q| {
            let [rx, ry] = q.radii();
            (q.pos().extend(rx), Vec4::new(ry, q.roll(), q.n(), 0.0))
        })
        .collect();
    if !smooth || p.len() < 3 {
        return p.iter().map(|&(a, b)| samp(a, b)).collect();
    }
    let n = p.len();
    // Phantom end points carry the position on straight but mirror the
    // radii (the neighbour's again), so the radius levels off into a capped
    // end: the half-ellipsoid
    // cap then meets the tube tangentially instead of leaving a crease ring
    // where a tapering limb starts (a cat's shoulders and hips).
    let flat = |e: (Vec4, Vec4), o: (Vec4, Vec4)| -> (Vec4, Vec4) {
        let mut a = 2.0 * e.0 - o.0;
        let mut b = 2.0 * e.1 - o.1;
        a.w = o.0.w;
        b.x = o.1.x;
        (a, b)
    };
    let at = |i: isize| -> (Vec4, Vec4) {
        if i < 0 {
            flat(p[0], p[1])
        } else if i as usize >= n {
            flat(p[n - 1], p[n - 2])
        } else {
            p[i as usize]
        }
    };
    let mut out = Vec::new();
    for s in 0..n - 1 {
        let i = s as isize;
        let q = [at(i - 1), at(i), at(i + 1), at(i + 2)];
        let qa = q.map(|v| v.0);
        let qb = q.map(|v| v.1);
        let pos = qa.map(Vec4::truncate);
        let len = pos[1].distance(pos[2]);
        let rmin = qa[1].w.min(qa[2].w).min(qb[1].x).min(qb[2].x).max(1e-4);
        let rmean = (0.25 * (qa[1].w + qa[2].w + qb[1].x + qb[2].x)).max(1e-4);
        let knot = |a: Vec3, b: Vec3| (a - b).length().sqrt().max(1e-6);
        let t0 = 0.0;
        let tk1 = t0 + knot(pos[0], pos[1]);
        let tk2 = tk1 + knot(pos[1], pos[2]);
        let tk3 = tk2 + knot(pos[2], pos[3]);
        let ks = [t0, tk1, tk2, tk3];
        let at_t = |t: f32| {
            let mut v = crate::loft::barry_goldman(qa, ks, t);
            let mut w = crate::loft::barry_goldman(qb, ks, t);
            // Overshoot must not pinch the tube to nothing.
            v.w = v.w.max(0.5 * qa[1].w.min(qa[2].w));
            w.x = w.x.max(0.5 * qb[1].x.min(qb[2].x));
            samp(v, w)
        };
        if fine {
            // Adaptive: walk a fine pass and keep a sample whenever the
            // curve has turned `step_deg` or run `step_r` radii since the
            // last, so a bend concentrated near a joint is sampled densely.
            const F: usize = 160;
            let tf = |j: usize| tk1 + (tk2 - tk1) * j as f32 / F as f32;
            let fp: Vec<Vec3> = (0..=F)
                .map(|j| crate::loft::barry_goldman(qa, ks, tf(j)).truncate())
                .collect();
            out.push(at_t(tk1));
            let (mut turn, mut run) = (0.0f32, 0.0f32);
            for j in 1..F {
                let (d0, d1) = (fp[j] - fp[j - 1], fp[j + 1] - fp[j]);
                turn += d0
                    .normalize_or_zero()
                    .dot(d1.normalize_or_zero())
                    .clamp(-1.0, 1.0)
                    .acos()
                    .to_degrees();
                run += d0.length();
                if turn >= step_deg || run >= step_r * rmean {
                    out.push(at_t(tf(j)));
                    turn = 0.0;
                    run = 0.0;
                }
            }
            continue;
        }
        let t1 = (pos[2] - pos[0]).normalize_or_zero();
        let t2 = (pos[3] - pos[1]).normalize_or_zero();
        let deg = t1.dot(t2).clamp(-1.0, 1.0).acos().to_degrees();
        let k = ((deg / step_deg).ceil() as usize)
            .max((len / (step_r * rmin)).ceil() as usize)
            .clamp(1, 128);
        for j in 0..k {
            out.push(at_t(tk1 + (tk2 - tk1) * j as f32 / k as f32));
        }
    }
    let (a, b) = p[n - 1];
    out.push(samp(a, b));
    out
}

/// A rotation-minimising (side, up) frame per sample, rolled by each
/// sample's roll: up starts as world up (or +z on a vertical start) and is
/// carried along the curve, so the sections stay continuous.
fn chain_frames(pts: &[Samp]) -> Vec<(Vec3, Vec3)> {
    let n = pts.len();
    let tan = |i: usize| -> Vec3 {
        let a = pts[i.saturating_sub(1)].p;
        let b = pts[(i + 1).min(n - 1)].p;
        let t = (b - a).normalize_or_zero();
        if t == Vec3::ZERO { Vec3::Z } else { t }
    };
    let mut out = Vec::with_capacity(n);
    let mut prev_t = Vec3::ZERO;
    let mut up = Vec3::Y;
    for (i, pt) in pts.iter().enumerate() {
        let t = tan(i);
        if i == 0 {
            up = if t.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
        } else {
            up = Quat::from_rotation_arc(prev_t, t) * up;
        }
        up = (up - t * up.dot(t)).normalize_or_zero();
        if up == Vec3::ZERO {
            up = t.any_orthonormal_vector();
        }
        prev_t = t;
        let side = up.cross(t);
        let (sn, cs) = pt.roll.to_radians().sin_cos();
        out.push((side * cs + up * sn, up * cs - side * sn));
    }
    out
}

/// The lowest and highest projection onto `dir` of a chain's surface
/// (ellipse supports at every sample, half-ellipsoid caps at the ends): how
/// far the tube reaches along `dir`, without meshing it.
pub fn chain_extent(points: &[Sect], smooth: bool, dir: Vec3) -> (f32, f32) {
    let pts = chain_samples(points, smooth);
    if pts.is_empty() {
        return (0.0, 0.0);
    }
    let frames = chain_frames(&pts);
    let n = pts.len();
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for (i, (p, (s, u))) in pts.iter().zip(&frames).enumerate() {
        let t = u.cross(*s).normalize_or_zero();
        let rz = if i == 0 || i + 1 == n {
            p.r[0].min(p.r[1])
        } else {
            0.0
        };
        // Support of a superellipse: the dual norm (exponent n / (n - 1)).
        let (a, b) = ((p.r[0] * s.dot(dir)).abs(), (p.r[1] * u.dot(dir)).abs());
        let m = if p.n > 1.01 { p.n / (p.n - 1.0) } else { 100.0 };
        let cross = if (p.n - 2.0).abs() < 1e-4 {
            (a * a + b * b).sqrt()
        } else {
            let c = a.max(b);
            if c < 1e-12 {
                0.0
            } else {
                c * ((a / c).powf(m) + (b / c).powf(m)).powf(1.0 / m)
            }
        };
        let e = (cross * cross + (rz * t.dot(dir)).powi(2)).sqrt();
        let c = p.p.dot(dir);
        lo = lo.min(c - e);
        hi = hi.max(c + e);
    }
    (lo, hi)
}

/// Approximate signed distance to an ellipse of radii `r` (Quilez's
/// gradient-scaled form: exact on the curve, good near it).
fn ellipse(q: bevy::math::Vec2, r: bevy::math::Vec2) -> f32 {
    let k0 = (q / r).length();
    let k1 = (q / (r * r)).length();
    if k1 < 1e-12 {
        -r.min_element()
    } else {
        k0 * (k0 - 1.0) / k1
    }
}

/// Approximate signed distance to a superellipse `|x/rx|^n + |y/ry|^n = 1`
/// (gradient-scaled like `ellipse`, which it equals at n = 2).
fn superellipse(q: bevy::math::Vec2, r: bevy::math::Vec2, n: f32) -> f32 {
    if (n - 2.0).abs() < 1e-4 {
        return ellipse(q, r);
    }
    let g = (q / r).abs();
    let c = g.max_element();
    if c < 1e-9 {
        return -r.min_element();
    }
    let h = g / c;
    let sum = h.x.powf(n) + h.y.powf(n);
    let f = c * sum.powf(1.0 / n);
    // d f / d g_i = (g_i / f)^(n-1); then / r_i.
    let gx = (g.x / f).powf(n - 1.0) / r.x;
    let gy = (g.y / f).powf(n - 1.0) / r.y;
    let gl = (gx * gx + gy * gy).sqrt();
    if gl < 1e-12 {
        -r.min_element()
    } else {
        (f - 1.0) / gl
    }
}

/// A superellipse-section (in-plane) by ellipse (along `z`) solid: the
/// cross-section is a superellipse of radii `r.xy`, rounded off by an
/// ellipse of radius `r.z` along the tangent (the caps).
fn super_cap(q: Vec3, r: Vec3, n: f32) -> f32 {
    if (n - 2.0).abs() < 1e-4 {
        let k0 = (q / r).length();
        let k1 = (q / (r * r)).length();
        return if k1 < 1e-12 {
            -r.min_element()
        } else {
            k0 * (k0 - 1.0) / k1
        };
    }
    let g = bevy::math::Vec2::new(q.x / r.x, q.y / r.y).abs();
    let c = g.max_element();
    let s = if c < 1e-9 {
        0.0
    } else {
        let h = g / c;
        c * (h.x.powf(n) + h.y.powf(n)).powf(1.0 / n)
    };
    let w = q.z / r.z;
    let f = (s * s + w * w).sqrt();
    if f < 1e-9 {
        return -r.min_element();
    }
    let (gx, gy) = if s < 1e-9 {
        (0.0, 0.0)
    } else {
        ((g.x / s).powf(n - 1.0) / r.x, (g.y / s).powf(n - 1.0) / r.y)
    };
    let k = s / f;
    let grad = Vec3::new(k * gx, k * gy, w / (f * r.z));
    let gl = grad.length();
    if gl < 1e-12 {
        -r.min_element()
    } else {
        (f - 1.0) / gl
    }
}

/// An elliptical (or superelliptical) rounded cone between two chain
/// samples.
struct ECone {
    a: Vec3,
    b: Vec3,
    sa: Vec3,
    ua: Vec3,
    sb: Vec3,
    ub: Vec3,
    ra: bevy::math::Vec2,
    rb: bevy::math::Vec2,
    /// Rounded caps at the chain's own ends; between segments the ends
    /// are cut flat (the neighbour carries on), so a taper has no bumps.
    cap_a: bool,
    cap_b: bool,
    /// How far an uncapped end carries on straight (meters): enough to
    /// close the wedge on the outside of a bend with the next segment.
    ext_a: f32,
    ext_b: f32,
    /// Radius slopes (per meter along the chain) at `a` and `b`, from the
    /// neighbouring samples: radii follow a Hermite curve across the
    /// samples, so a taper has no kinks however the samples are spaced.
    da: bevy::math::Vec2,
    db: bevy::math::Vec2,
    /// Superellipse exponents at `a` and `b` (2 = ellipse).
    na: f32,
    nb: f32,
}

impl ECone {
    /// Ellipse distance in the (interpolated) frame across the segment,
    /// capped by a half-ellipsoid past the ends.
    fn eval(&self, p: Vec3) -> f32 {
        let ba = self.b - self.a;
        let l2 = ba.length_squared();
        let (h, t) = if l2 < 1e-12 {
            (0.0, self.ua.cross(self.sa).normalize_or_zero())
        } else {
            ((p - self.a).dot(ba) / l2, ba / l2.sqrt())
        };
        let len = l2.sqrt().max(1e-9);
        let lo = if self.cap_a { 0.0 } else { -self.ext_a / len };
        let hi = if self.cap_b {
            1.0
        } else {
            1.0 + self.ext_b / len
        };
        let hc = h.clamp(lo, hi);
        let c = self.a + ba * hc;
        let hf = hc.clamp(0.0, 1.0);
        let mut u = self.ua.lerp(self.ub, hf);
        u = (u - t * u.dot(t)).normalize_or_zero();
        let s = self.sa.lerp(self.sb, hf);
        let s = (s - t * s.dot(t) - u * s.dot(u)).normalize_or_zero();
        // Radii: a Hermite curve across the samples inside, carried on
        // along its end slope past the ends.
        let r = if hc < 0.0 {
            self.ra + self.da * (hc * len)
        } else if hc > 1.0 {
            self.rb + self.db * ((hc - 1.0) * len)
        } else {
            let x = hc;
            let (x2, x3) = (x * x, x * x * x);
            self.ra * (2.0 * x3 - 3.0 * x2 + 1.0)
                + self.da * (len * (x3 - 2.0 * x2 + x))
                + self.rb * (3.0 * x2 - 2.0 * x3)
                + self.db * (len * (x3 - x2))
        };
        let r = r.max(0.3 * self.ra.min(self.rb));
        let n = self.na + (self.nb - self.na) * hf;
        let d = p - c;
        let q = Vec3::new(d.dot(s), d.dot(u), d.dot(t));
        // Past an inner end's run the tube closes over another run's
        // length instead of stopping flat, so nothing leaves a ledge.
        let over = if h < lo && !self.cap_a {
            (lo - h) * len / self.ext_a.max(1e-6)
        } else if h > hi && !self.cap_b {
            (h - hi) * len / self.ext_b.max(1e-6)
        } else {
            -1.0
        };
        if over >= 0.0 {
            if over >= 1.0 {
                return q.length().max(1e-5);
            }
            let rs = (r * (1.0 - over)).max(bevy::math::Vec2::splat(1e-5));
            return superellipse(bevy::math::Vec2::new(q.x, q.y), rs, n);
        }
        let rr = Vec3::new(r.x, r.y, r.x.min(r.y)).max(Vec3::splat(1e-5));
        super_cap(q, rr, n)
    }
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
    /// Local warps: the resolved centre.
    at: Vec3,
}

/// The largest gradient a push / pinch / flatten may give its displacement:
/// below 1 the warp cannot fold space.
const LIP: f32 = 0.8;

fn is_local(w: &Deform) -> bool {
    crate::deform::LOCAL_MODES.contains(&w.mode.as_str())
}

impl Stage {
    /// A local warp's weight at `q` (0 outside its radius).
    fn weight(&self, q: Vec3) -> f32 {
        self.w.local_weight(q.distance(self.at))
    }

    /// Where `q` came from before a local space warp (push / pinch /
    /// flatten); identity outside the radius.
    ///
    /// The forward map is p + u(p) with the weight read at the source point,
    /// and its strength is clamped so |grad u| <= `LIP` (`lip`): the map is
    /// then a bijection (no folds, however large the amount), and the inverse
    /// is the fixed point of p = q - u(p), a contraction iterated to
    /// convergence.
    fn local_inverse(&self, q: Vec3) -> Vec3 {
        let r = self.w.radius.unwrap_or(0.1);
        if q.distance(self.at) >= r + self.reach() {
            return q;
        }
        let mut p = q;
        for _ in 0..64 {
            let next = q - self.local_offset(p);
            let step = next.distance(p);
            p = next;
            if step < 1e-7 * r.max(1e-3) {
                break;
            }
        }
        p
    }

    /// The forward displacement u(p) of push / pinch / flatten.
    fn local_offset(&self, p: Vec3) -> Vec3 {
        let wt = self.weight(p);
        if wt <= 0.0 {
            return Vec3::ZERO;
        }
        let a = self.strength();
        match self.w.mode.as_str() {
            "push" => {
                let d = Vec3::from(self.w.direction.unwrap_or([0.0, 1.0, 0.0])).normalize_or_zero();
                d * a * wt
            }
            "pinch" => (self.at - p) * a * wt,
            "flatten" => {
                let n = Vec3::from(self.w.normal.unwrap_or([0.0, 1.0, 0.0])).normalize_or_zero();
                -n * (p - self.at).dot(n) * a * wt
            }
            _ => Vec3::ZERO,
        }
    }

    /// The warp's amount, clamped so the map's gradient stays <= `LIP`.
    fn strength(&self) -> f32 {
        let r = self.w.radius.unwrap_or(0.1).max(1e-9);
        match self.w.mode.as_str() {
            "push" => {
                let a = self.w.amount.unwrap_or(0.0);
                let cap = LIP * r / self.w.falloff_slope();
                a.clamp(-cap, cap)
            }
            "pinch" | "flatten" => {
                let a = self.w.amount.unwrap_or(0.5);
                a.min(LIP / self.w.falloff_stretch().max(1e-6))
            }
            _ => 0.0,
        }
    }

    /// How far the warp can move a point (so points that far outside the
    /// radius may still come from inside it).
    fn reach(&self) -> f32 {
        let r = self.w.radius.unwrap_or(0.1);
        match self.w.mode.as_str() {
            "push" => self.strength().abs(),
            "pinch" | "flatten" => self.strength() * r,
            _ => 0.0,
        }
    }

    /// The Lipschitz bound of the inverse map near `q` minus one, so the
    /// warped distance divided by (1 + lip) stays a lower bound (the mesher
    /// and smooth booleans trust it). Smoothly 0 outside the radius.
    fn lip(&self, q: Vec3) -> f32 {
        let r = self.w.radius.unwrap_or(0.1);
        let d = q.distance(self.at);
        let outer = r + self.reach();
        if d >= outer {
            return 0.0;
        }
        let t = 1.0 - d / outer;
        let s = t * t * (3.0 - 2.0 * t);
        // |grad u| <= LIP gives |grad u^-1| <= 1 / (1 - LIP).
        let l = match self.w.mode.as_str() {
            "push" => self.strength().abs() * self.w.falloff_slope() / r.max(1e-9),
            _ => self.strength() * self.w.falloff_stretch(),
        };
        s * l / (1.0 - l).max(1e-3)
    }

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

/// The field at `q` through the space and local warps (last one outermost).
///
/// A smooth warp blurs with six samples of the field below it; samples taken
/// for a blur skip the smooth warps further in (`blurring`), so stacked
/// smooths cost 1 + 6 per smooth instead of 7^n.
fn through(stages: &[&Stage], q: Vec3, base: &dyn Fn(Vec3) -> f32) -> f32 {
    through_at(stages, q, base, false)
}

fn through_at(stages: &[&Stage], q: Vec3, base: &dyn Fn(Vec3) -> f32, blurring: bool) -> f32 {
    let Some((last, rest)) = stages.split_last() else {
        return base(q);
    };
    let through =
        |s: &[&Stage], q: Vec3, base: &dyn Fn(Vec3) -> f32| through_at(s, q, base, blurring);
    match last.w.mode.as_str() {
        "inflate" => through(rest, q, base) - last.w.amount.unwrap_or(0.0) * last.weight(q),
        "smooth" if blurring => through(rest, q, base),
        "smooth" => {
            let d = through(rest, q, base);
            let wt = last.w.amount.unwrap_or(0.5) * last.weight(q);
            if wt <= 0.0 {
                return d;
            }
            // A local blur of the field: the mean of six samples a third of
            // the radius away (the Laplacian pulls ridges in, fills dents).
            let h = last.w.size.unwrap_or(last.w.radius.unwrap_or(0.1) / 3.0);
            let mut sum = 0.0;
            for dir in [
                Vec3::X,
                Vec3::NEG_X,
                Vec3::Y,
                Vec3::NEG_Y,
                Vec3::Z,
                Vec3::NEG_Z,
            ] {
                sum += through_at(rest, q + dir * h, base, true);
            }
            d + (sum / 6.0 - d) * wt
        }
        "push" | "pinch" | "flatten" => {
            through(rest, last.local_inverse(q), base) / (1.0 + last.lip(q))
        }
        _ => through(rest, last.inverse(q), base),
    }
}

/// A local warp's anchor: "part:words" (a joint of that part, else words
/// on its box), or words on the field's own box ("top-front").
fn resolve_anchor(a: &str, lo: Vec3, hi: Vec3, lookup: &Lookup) -> Result<Vec3, String> {
    if let Some((p, words)) = a.split_once(':') {
        let part = lookup(p).map_err(|e| format!("`{a}`: {e}"))?;
        if let Some(j) = part.joint(words) {
            return Ok(j);
        }
        let (plo, phi) = part.bounds();
        return crate::commands::anchor_point(words, plo, phi)
            .map(Vec3::from)
            .map_err(|e| format!("`{a}`: {e}"));
    }
    crate::commands::anchor_point(a, lo, hi)
        .map(Vec3::from)
        .map_err(|e| format!("`{a}` is not a joint or an anchor on the field's box ({e})"))
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
    for (i, w) in def.warps.iter().enumerate() {
        let at = match &w.at {
            None => Vec3::ZERO,
            Some(crate::deform::WarpAt::Point(p)) => Vec3::from(*p),
            Some(crate::deform::WarpAt::Anchor(a)) => resolve_anchor(a, size_lo, size_hi, lookup)
                .map_err(|m| {
                ModelerError::Invalid(format!("sdf `{name}`: warps[{i}]: at: {m}"))
            })?,
        };
        // Under a mirror, a symmetric local warp (the default) is folded
        // onto the side the field is evaluated on, so one at x < 0 acts on
        // both sides; `mirror: false` keeps it where it is (pre-fold).
        let at = if is_local(w) && w.mirror != Some(false) {
            fold(at, def.mirror.as_deref())
        } else {
            at
        };
        let st = Stage {
            w: w.clone(),
            lo,
            hi,
            at,
        };
        if is_local(w) {
            let grow = match w.mode.as_str() {
                "inflate" => w.amount.unwrap_or(0.0).abs(),
                "push" => st.reach(),
                _ => 0.0,
            };
            if w.mode == "inflate" {
                disp += grow;
            }
            lo -= Vec3::splat(grow);
            hi += Vec3::splat(grow);
        } else if w.mode == "displace" {
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
            let hint = def.target_tris.or(def.tris.filter(|_| !def.bake));
            if let Some(t) = hint {
                // Mesh about 40x the kept triangles (at least 6000): enough
                // for the decimate to pick a faithful light mesh. Surface
                // area is guessed from the box (an ellipsoid's is ~0.55 of
                // its box's); a level set makes ~2.2 triangles per r^2.
                let e = hi - lo;
                let area = 0.55 * 2.0 * (e.x * e.y + e.y * e.z + e.z * e.x);
                let want = (40.0 * t as f32).max(6000.0);
                let coarse = (2.2 * area / want).sqrt().min(size / 24.0).min(thin / 2.0);
                r = r.max(coarse);
            }
            if samples(r) > AUTO_SAMPLES {
                r *= (samples(r) / AUTO_SAMPLES).cbrt() as f32 * 1.01;
            }
            r
        }
    };
    let band = SMIN_WIDTH * k + disp + 4.0 * res;
    b.res = res;
    b.band = band;
    // A join blend under [`SHARPEN_CELLS`] cells, or an edge rounding under
    // [`SHARPEN_ROUND_CELLS`], can't be meshed as a fillet (marching
    // tetrahedra chord across it in a zigzag): such joins and edges are
    // meshed sharp and finished as creases. The choice is per join, so one
    // rim never alternates between the two.
    // A shape too small for clean creases (an eye socket a few cells
    // wide) keeps its fillet: sharpened, its rim would be all corners.
    let min_thin = SHARPEN_MIN_CELLS * res;
    let mut nodes = Vec::new();
    // Nodes of shapes that can make a crease (sharp edges, a hard join).
    let mut hard_nodes: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, s) in def.shapes.iter().enumerate() {
        let start = nodes.len();
        b.compile(s, k, &mut nodes)?;
        // The shape's largest extent: a wide shallow disc has a long rim
        // that snaps cleanly even though it is thin.
        let extent = nodes[start..]
            .iter()
            .map(|n| (n.hi - n.lo).max_element())
            .fold(0.0f32, f32::max);
        let mut s = s.clone();
        if extent >= min_thin {
            let sharp = sharpen_shape(&s, i, k, SHARPEN_CELLS * res, SHARPEN_ROUND_CELLS * res);
            if sharp != s {
                nodes.truncate(start);
                b.compile(&sharp, k, &mut nodes)?;
                s = sharp;
            }
        }
        let join = i > 0 && s.blend.unwrap_or(k) <= 0.0;
        if join || has_creases(std::slice::from_ref(&s), k, res) {
            hard_nodes.push(start..nodes.len());
        }
    }
    let buckets = Buckets::new(&nodes, band);
    let mut is_hard = vec![false; nodes.len()];
    for r in &hard_nodes {
        is_hard[r.clone()].fill(true);
    }

    let mirror = def.mirror.as_deref();
    let disp_stages: Vec<&Stage> = stages.iter().filter(|s| s.w.mode == "displace").collect();
    // One-sided local warps run before the fold, in world space.
    let one_sided = |s: &Stage| mirror.is_some() && is_local(&s.w) && s.w.mirror == Some(false);
    let pre_stages: Vec<&Stage> = stages.iter().filter(|s| one_sided(s)).collect();
    let space_stages: Vec<&Stage> = stages
        .iter()
        .filter(|s| s.w.mode != "displace" && !one_sided(s))
        .collect();
    let base = |q: Vec3| -> f32 {
        let list = buckets.get(q);
        let mut d = band;
        let mut i = 0;
        while i < list.len() {
            let n = &nodes[list[i] as usize];
            if n.chain != 0 {
                let mut j = i + 1;
                while j < list.len() && nodes[list[j] as usize].chain == n.chain {
                    j += 1;
                }
                d = smin(d, chain_min(&nodes, &list[i..j], q, band), n.k);
                i = j;
                continue;
            }
            let e = if n.join == Join::Intersect || inside_box(q, n, band) {
                n.eval(q, band).min(band)
            } else {
                band
            };
            d = join(d, e, n.join, n.k);
            i += 1;
        }
        d.clamp(-4.0 * band, band)
    };
    let folded = |p: Vec3| -> f32 {
        let q = fold(p, mirror);
        let mut off = 0.0;
        for s in &disp_stages {
            off += noise_at(&s.w, q, (s.hi - s.lo).max(Vec3::splat(1e-6)));
        }
        through(&space_stages, q, &base) - off
    };
    let field = |p: Vec3| -> f32 {
        if pre_stages.is_empty() {
            folded(p)
        } else {
            through(&pre_stages, p, &folded)
        }
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
    let meshed = crate::csg::manifold_to_part(&m, name, crate::Material::default());
    let mut sampling = crate::sdf_finish::Sampling::analytic(res);
    if hard_nodes.is_empty() {
        sampling = sampling.smooth();
    }
    if has_sampled(&def.shapes) {
        sampling = sampling.wide();
    }
    // Creases only form on the surfaces of the hard shapes (warps move
    // them: then anywhere).
    let reach = 2.0 * res;
    let zone = |p: Vec3| -> bool {
        if !stages.is_empty() {
            return true;
        }
        let q = fold(p, mirror);
        buckets
            .get(q)
            .iter()
            .any(|&i| is_hard[i as usize] && nodes[i as usize].eval(q, band).abs() < reach)
    };
    let mut out = crate::sdf_finish::finish_in(&meshed, &field, sampling, Some(&zone));
    #[cfg(test)]
    tests::PROBE.with(|p| {
        if let Some(f) = p.borrow_mut().as_mut() {
            f(&meshed, &out, &field, res);
        }
    });
    out.visible = true;
    if let Some(t) = def.tris {
        let d = crate::decimate::Decimate {
            tris: Some(t),
            bake: def.bake,
            ..Default::default()
        };
        out = crate::decimate::decimate(name, &out, &d).map_err(err)?;
    }
    Ok(out)
}

/// Joins blended over fewer cells than this are meshed sharp.
const SHARPEN_CELLS: f32 = 1.25;

/// Edge roundings (box, cylinder, silhouette `round`) under this many cells
/// are meshed sharp: an edge fillet under two cells chords into teeth.
const SHARPEN_ROUND_CELLS: f32 = 2.0;

/// Shapes smaller than this many cells (largest extent) are never
/// sharpened.
const SHARPEN_MIN_CELLS: f32 = 10.0;

/// Shape `i` of a list with its join blend under `lim` and its edge
/// rounding under `round_lim` set to 0 (`k` is the item's blend, the
/// default of a join); a group's members likewise.
fn sharpen_shape(s: &Shape, i: usize, k: f32, lim: f32, round_lim: f32) -> Shape {
    let mut s = s.clone();
    if i > 0 && s.blend.unwrap_or(k) < lim {
        s.blend = Some(0.0);
    }
    if matches!(s.shape.as_str(), "box" | "cylinder" | "silhouette")
        && s.round.is_some_and(|r| r < round_lim)
    {
        s.round = Some(0.0);
    }
    if let Some(kids) = &s.shapes {
        let kk = s.blend.unwrap_or(k);
        let kk = if kk < lim { 0.0 } else { kk };
        s.shapes = Some(
            kids.iter()
                .enumerate()
                .map(|(j, c)| sharpen_shape(c, j, kk, lim, round_lim))
                .collect(),
        );
    }
    s
}

/// Is any shape sampled (a silhouette's outline grids, a part's grid)?
/// Its gradient is piecewise flat.
fn has_sampled(shapes: &[Shape]) -> bool {
    shapes.iter().any(|s| match s.shape.as_str() {
        "silhouette" | "part" => true,
        "group" => has_sampled(s.shapes.as_deref().unwrap_or(&[])),
        _ => false,
    })
}

/// Can these shapes make a crease (after [`sharpen`])? A hard join, a shape with sharp edges (a box or cylinder rounded less
/// than a cell, a silhouette, a sampled part). A `mirror` fold is not
/// counted: bodies and skins cross it symmetrically.
fn has_creases(shapes: &[Shape], k: f32, res: f32) -> bool {
    shapes.iter().enumerate().any(|(i, s)| {
        let join = i > 0 && s.blend.unwrap_or(k) <= 0.0;
        let own = match s.shape.as_str() {
            "box" | "cylinder" | "silhouette" => s.round.unwrap_or(0.0) < res,
            "part" => true,
            "group" => has_creases(
                s.shapes.as_deref().unwrap_or(&[]),
                s.blend.unwrap_or(k),
                res,
            ),
            _ => false,
        };
        join || own
    })
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
pub fn skin(name: &str, def: &SkinDef, lookup: &Lookup) -> Result<Part, ModelerError> {
    def.validate()
        .map_err(|e| ModelerError::Invalid(format!("skin `{name}`: {e}")))?;
    build(name, &def.to_sdf(), lookup).map_err(|e| match e {
        ModelerError::Csg(m) => ModelerError::Csg(m.replacen("sdf `", "skin `", 1)),
        o => o,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field probe for tests: called by [`build`] with the raw mesh, the
    /// finished one, the field and the resolution.
    pub(super) type Probe = Box<dyn FnMut(&Part, &Part, &dyn Fn(Vec3) -> f32, f32)>;
    thread_local! {
        pub(super) static PROBE: std::cell::RefCell<Option<Probe>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Builds `def` and returns (raw mesh, finished mesh, field sampled by
    /// `sample` at the given points, res).
    fn build_probed(def: &SdfDef, pts: &[Vec3]) -> (Part, Part, Vec<f32>, f32) {
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let g2 = got.clone();
        let pts = pts.to_vec();
        PROBE.with(|p| {
            *p.borrow_mut() = Some(Box::new(
                move |raw: &Part, fin: &Part, f: &dyn Fn(Vec3) -> f32, res: f32| {
                    let vals = pts.iter().map(|q| f(*q)).collect::<Vec<_>>();
                    *g2.borrow_mut() = Some((raw.clone(), fin.clone(), vals, res));
                },
            ));
        });
        let r = build("probe", def, &no_parts);
        PROBE.with(|p| *p.borrow_mut() = None);
        r.unwrap_or_else(|e| panic!("{e}"));
        got.borrow_mut()
            .take()
            .unwrap_or_else(|| panic!("probe not called"))
    }

    #[test]
    fn local_warps_move_the_surface_only_within_their_radius() {
        let sphere = |warps: Vec<Deform>| SdfDef {
            shapes: vec![Shape {
                shape: "sphere".into(),
                radius: Some(vec![0.2]),
                ..Shape::default()
            }],
            warps,
            resolution: Some(0.006),
            ..SdfDef::default()
        };
        let at = Vec3::new(0.2, 0.0, 0.0);
        let r = 0.1;
        let warp = |mode: &str| Deform {
            mode: mode.into(),
            at: Some(crate::deform::WarpAt::Point(at.to_array())),
            radius: Some(r),
            amount: Some(if mode == "pinch" || mode == "flatten" {
                0.6
            } else {
                0.03
            }),
            direction: (mode == "push").then_some([1.0, 0.0, 0.0]),
            normal: (mode == "flatten").then_some([1.0, 0.0, 0.0]),
            ..Deform::default()
        };
        for mode in ["inflate", "push", "pinch", "flatten", "smooth"] {
            let def = sphere(vec![warp(mode)]);
            def.validate().unwrap_or_else(|e| panic!("{mode}: {e}"));
            let p = build("s", &def, &no_parts).unwrap_or_else(|e| panic!("{mode}: {e}"));
            let mut moved_out = 0.0f32;
            let mut moved_in = 0.0f32;
            for v in &p.verts {
                let v = Vec3::from(*v);
                let off = v.length() - 0.2;
                if v.distance(at) > r + 0.02 {
                    assert!(
                        off.abs() < 0.004,
                        "{mode}: {v} moved {off} outside the radius"
                    );
                } else {
                    moved_out = moved_out.max(off);
                    moved_in = moved_in.min(off);
                }
            }
            match mode {
                "inflate" | "push" => assert!(moved_out > 0.02, "{mode}: grew {moved_out}"),
                // The cap's sides rise toward the plane x = 0.2 through `at`.
                "flatten" => assert!(moved_out > 0.001, "{mode}: {moved_out} {moved_in}"),
                _ => {}
            }
        }
        // A body warp centred on a joint by name.
        let mut d = crate::body::BodyDef {
            preset: "biped".into(),
            resolution: Some(0.012),
            ..Default::default()
        };
        d.warps = vec![Deform {
            mode: "inflate".into(),
            at: Some(crate::deform::WarpAt::Anchor("head".into())),
            radius: Some(0.12),
            amount: Some(0.03),
            ..Deform::default()
        }];
        d.validate().unwrap_or_else(|e| panic!("{e}"));
        let low = crate::body::lower(&d).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            low.sdf.warps[0].at,
            Some(crate::deform::WarpAt::Point(_))
        ));
        // A typo is an error naming the fields.
        let mut bad = warp("inflate");
        bad.degrees = Some(3.0);
        assert!(check_warps(&[bad]).is_err());
    }

    fn ball(warps: Vec<Deform>, mirror: Option<&str>) -> SdfDef {
        SdfDef {
            shapes: vec![Shape {
                shape: "sphere".into(),
                radius: Some(vec![0.2]),
                ..Shape::default()
            }],
            warps,
            mirror: mirror.map(Into::into),
            resolution: Some(0.008),
            ..SdfDef::default()
        }
    }

    fn local(mode: &str, at: [f32; 3], amount: f32) -> Deform {
        Deform {
            mode: mode.into(),
            at: Some(crate::deform::WarpAt::Point(at)),
            radius: Some(0.1),
            amount: Some(amount),
            direction: (mode == "push").then_some([1.0, 0.0, 0.0]),
            normal: (mode == "flatten").then_some([1.0, 0.0, 0.0]),
            ..Deform::default()
        }
    }

    #[test]
    fn large_local_warps_do_not_fold() {
        for (mode, amount) in [("push", 2.0), ("pinch", 1.0), ("flatten", 1.0)] {
            let def = ball(vec![local(mode, [0.2, 0.0, 0.0], amount)], None);
            let p = build("s", &def, &no_parts).unwrap_or_else(|e| panic!("{mode}: {e}"));
            let (n, _) = crate::organic::self_intersections(&p);
            assert_eq!(n, 0, "{mode}: {n} self-intersections");
            // One closed sphere-like shell: no detached pockets from a fold.
            let chi = p.verts.len() as i64 - p.indices.len() as i64 / 6;
            assert_eq!(chi, 2, "{mode}: euler characteristic {chi}");
            // The inverse really inverts the (clamped) forward map.
            let st = Stage {
                w: def.warps[0].clone(),
                lo: Vec3::splat(-1.0),
                hi: Vec3::splat(1.0),
                at: Vec3::new(0.2, 0.0, 0.0),
            };
            for q in [Vec3::new(0.25, 0.03, 0.0), Vec3::new(0.15, -0.02, 0.04)] {
                let p = st.local_inverse(q);
                assert!((p + st.local_offset(p)).distance(q) < 1e-5, "{mode}");
            }
        }
    }

    #[test]
    fn local_warps_under_mirror_act_where_placed() {
        let bump_at = |p: &Part, x: f32| {
            p.verts
                .iter()
                .map(|v| Vec3::from(*v))
                .filter(|v| (v.x - x).abs() < 0.12 && v.y.abs() < 0.12)
                .map(|v| v.length())
                .fold(0.0f32, f32::max)
        };
        // Symmetric (default): a warp at x < 0 bumps both sides.
        let sym = ball(vec![local("inflate", [-0.2, 0.0, 0.0], 0.04)], Some("x"));
        let p = build("s", &sym, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        assert!(bump_at(&p, -0.2) > 0.225 && bump_at(&p, 0.2) > 0.225);
        // One-sided: only where it is.
        let mut w = local("inflate", [-0.2, 0.0, 0.0], 0.04);
        w.mirror = Some(false);
        let one = ball(vec![w], Some("x"));
        let p = build("s", &one, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        assert!(bump_at(&p, -0.2) > 0.225, "{}", bump_at(&p, -0.2));
        assert!(bump_at(&p, 0.2) < 0.21, "{}", bump_at(&p, 0.2));
    }

    #[test]
    fn stacked_smooth_warps_stay_linear() {
        let mut ws = Vec::new();
        for _ in 0..6 {
            let mut w = local("smooth", [0.2, 0.0, 0.0], 0.5);
            w.size = Some(0.02);
            ws.push(w);
        }
        let def = ball(ws, None);
        def.validate().unwrap_or_else(|e| panic!("{e}"));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let stages: Vec<Stage> = def
            .warps
            .iter()
            .map(|w| Stage {
                w: w.clone(),
                lo: Vec3::splat(-1.0),
                hi: Vec3::splat(1.0),
                at: Vec3::new(0.2, 0.0, 0.0),
            })
            .collect();
        let refs: Vec<&Stage> = stages.iter().collect();
        let base = |q: Vec3| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            q.length() - 0.2
        };
        through(&refs, Vec3::new(0.2, 0.0, 0.0), &base);
        assert_eq!(calls.into_inner(), 1 + 6 * 6);
        let mut bad = local("inflate", [0.0; 3], 0.1);
        bad.size = Some(0.1);
        assert!(check_warps(&[bad]).is_err());
    }

    #[test]
    fn skin_and_body_warps_read_part_anchors() {
        let w = local("inflate", [0.0; 3], 0.02);
        let w = Deform {
            at: Some(crate::deform::WarpAt::Anchor("knob:top".into())),
            ..w
        };
        let knob = Part {
            name: "knob".into(),
            verts: vec![[0.0, 0.0, 0.0], [0.0, 0.1, 0.0]],
            indices: vec![0, 1, 1],
            ..Part::default()
        };
        let knob = Arc::new(knob);
        let look = |n: &str| {
            if n == "knob" {
                Ok(knob.clone())
            } else {
                no_parts(n)
            }
        };
        let def = SkinDef {
            chains: vec![vec![
                ChainPt::Point(Sect::Round([0.0, 0.0, 0.0, 0.05])),
                ChainPt::Point(Sect::Round([0.0, 0.2, 0.0, 0.05])),
            ]],
            warps: vec![w.clone()],
            resolution: Some(0.01),
            ..SkinDef::default()
        };
        skin("t", &def, &look).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(warp_reads(&def.warps), vec!["knob"]);
        let op = crate::Op::Skin {
            name: "t".into(),
            def: Box::new(def),
        };
        assert!(crate::graph::inputs(&op).contains(&"knob"));
    }

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
        let p = skin("guy", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let (lo, hi) = p.bounds();
        assert!(lo.y < 0.0 && hi.y > 1.05, "{lo} {hi}");
        let bad: SkinDef = serde_json::from_value(serde_json::json!({"chains": [["knee"]]}))
            .unwrap_or_else(|e| panic!("{e}"));
        let e = skin("x", &bad, &no_parts)
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

    #[test]
    fn elliptical_sections_measure_their_width_and_depth() {
        let chain = |pts: serde_json::Value| SdfDef {
            shapes: vec![shape(serde_json::json!({"shape": "chain", "points": pts}))],
            blend: Some(0.0),
            resolution: Some(0.004),
            ..SdfDef::default()
        };
        // Along z: rx 0.1 sideways (x), ry 0.05 up (y).
        let p = build(
            "e",
            &chain(serde_json::json!([
                [0.0, 0.2, -0.3, [0.1, 0.05]],
                [0.0, 0.2, 0.0, [0.1, 0.05]],
                [0.0, 0.2, 0.3, [0.1, 0.05]]
            ])),
            &no_parts,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let (lo, hi) = p.bounds();
        let (w, h) = (hi.x - lo.x, hi.y - lo.y);
        assert!(
            (w - 0.2).abs() < 0.01 && (h - 0.1).abs() < 0.01,
            "{w} x {h}"
        );
        // Rolled 90 degrees: the wide axis turns up.
        let p = build(
            "r",
            &chain(serde_json::json!([
                [0.0, 0.2, -0.3, [0.1, 0.05], 90.0],
                [0.0, 0.2, 0.3, [0.1, 0.05], 90.0]
            ])),
            &no_parts,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let (lo, hi) = p.bounds();
        let (w, h) = (hi.x - lo.x, hi.y - lo.y);
        assert!(
            (w - 0.1).abs() < 0.01 && (h - 0.2).abs() < 0.01,
            "{w} x {h}"
        );
        // A vertical chain: rx is x, ry is z (depth).
        let p = build(
            "v",
            &chain(serde_json::json!([
                [0.0, 0.0, 0.0, [0.08, 0.03]],
                [0.0, 0.5, 0.0, [0.08, 0.03]]
            ])),
            &no_parts,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = p.bounds();
        let (w, d) = (hi.x - lo.x, hi.z - lo.z);
        assert!(
            (w - 0.16).abs() < 0.01 && (d - 0.06).abs() < 0.01,
            "{w} x {d}"
        );
    }

    #[test]
    fn tris_decimates_within_ten_percent() {
        let def: SkinDef = serde_json::from_value(serde_json::json!({
            "joints": {"hip": [0.0, 0.5, 0.0, 0.08]},
            "chains": [
                ["hip", [0.0, 0.8, 0.0, [0.1, 0.06]], [0.0, 1.0, 0.05, 0.07]],
                ["hip", [0.15, 0.3, 0.0, 0.04], [0.2, 0.02, 0.05, 0.03]]
            ],
            "tris": 2000
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let p = skin("t", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        closed(&p);
        let n = p.tri_count() as f32;
        assert!((n - 2000.0).abs() / 2000.0 < 0.1, "{n} triangles");
        let bad = SkinDef {
            bake: true,
            ..def.clone()
        };
        let bad = SkinDef { tris: None, ..bad };
        assert!(bad.validate().is_err());
    }

    // --- finishing pass (sdf_finish) ---

    fn seg_tri_dist(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
        // Distance from p to triangle abc (Ericson's closest point).
        let (ab, ac, ap) = (b - a, c - a, p - a);
        let (d1, d2) = (ab.dot(ap), ac.dot(ap));
        if d1 <= 0.0 && d2 <= 0.0 {
            return p.distance(a);
        }
        let bp = p - b;
        let (d3, d4) = (ab.dot(bp), ac.dot(bp));
        if d3 >= 0.0 && d4 <= d3 {
            return p.distance(b);
        }
        let vc = d1 * d4 - d3 * d2;
        if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
            return p.distance(a + ab * (d1 / (d1 - d3)));
        }
        let cp = p - c;
        let (d5, d6) = (ab.dot(cp), ac.dot(cp));
        if d6 >= 0.0 && d5 <= d6 {
            return p.distance(c);
        }
        let vb = d5 * d2 - d1 * d6;
        if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
            return p.distance(a + ac * (d2 / (d2 - d6)));
        }
        let va = d3 * d6 - d5 * d4;
        if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
            return p.distance(b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
        }
        let den = 1.0 / (va + vb + vc);
        p.distance(a + ab * (vb * den) + ac * (vc * den))
    }

    fn surface_dist(part: &Part, p: Vec3) -> f32 {
        part.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| Vec3::from(part.verts[i as usize]));
                seg_tri_dist(p, a, b, c)
            })
            .fold(f32::MAX, f32::min)
    }

    #[test]
    fn finished_sphere_is_on_the_field_with_radial_normals() {
        let def = ball(vec![], None);
        let p = build("s", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        let mut worst = 0.0f32;
        let mut worst_deg = 0.0f32;
        for (v, n) in p.verts.iter().zip(&p.normals) {
            let v = Vec3::from(*v);
            worst = worst.max((v.length() - 0.2).abs());
            let d = Vec3::from(*n)
                .dot(v.normalize())
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            worst_deg = worst_deg.max(d);
        }
        eprintln!("sphere: radius error {worst:e} m (size 0.4), normals {worst_deg:.3} deg");
        assert!(worst < 1e-4 * 0.4, "radius error {worst}");
        assert!(worst_deg < 1.0, "normal error {worst_deg} deg");
    }

    fn rotated_box(res: f32) -> (SdfDef, Quat) {
        let q =
            Quat::from_rotation_y(30f32.to_radians()) * Quat::from_rotation_x(20f32.to_radians());
        let def: SdfDef = serde_json::from_value(serde_json::json!({
            "shapes": [{"shape": "box", "size": [0.3, 0.2, 0.25], "rotation": q.to_array()}],
            "blend": 0.0,
            "resolution": res
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        (def, q)
    }

    #[test]
    fn finished_box_creases_are_straight() {
        let res = 0.01;
        let (def, q) = rotated_box(res);
        let p = build("b", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        // Samples along the middle of the four edges parallel to local y.
        let h = Vec3::new(0.15, 0.1, 0.125);
        let mut worst = 0.0f32;
        for (sx, sz) in [(1.0, 1.0), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
            for k in 0..9 {
                let y = -0.06 + 0.015 * k as f32;
                let e = q * Vec3::new(sx * h.x, y, sz * h.z);
                worst = worst.max(surface_dist(&p, e));
            }
        }
        eprintln!(
            "box crease: max surface distance from the edge {:.3} res",
            worst / res
        );
        crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}"));
        let (n, _) = crate::organic::self_intersections(&p);
        assert_eq!(n, 0, "{n} self-intersections");
        assert!(worst < 0.05 * res, "crease off by {} res", worst / res);
    }

    #[test]
    fn finished_mirror_fold_stays_closed() {
        let def: SdfDef = serde_json::from_value(serde_json::json!({
            "shapes": [{"shape": "capsule", "from": [-0.05, 0.0, 0.0], "to": [0.2, 0.15, 0.05], "radius": [0.05]}],
            "blend": 0.0,
            "mirror": "x",
            "resolution": 0.008
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let p = build("m", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}"));
        let (n, _) = crate::organic::self_intersections(&p);
        assert_eq!(n, 0, "{n} self-intersections");
    }

    #[test]
    fn finished_hard_subtract_and_intersect_stay_closed() {
        let q =
            Quat::from_rotation_y(30f32.to_radians()) * Quat::from_rotation_z(25f32.to_radians());
        for op in ["subtract", "intersect"] {
            let def: SdfDef = serde_json::from_value(serde_json::json!({
                "shapes": [
                    {"shape": "box", "size": [0.3, 0.2, 0.25]},
                    {"shape": "box", "size": [0.2, 0.2, 0.2], "at": [0.12, 0.08, 0.05],
                     "rotation": q.to_array(), "op": op}
                ],
                "blend": 0.0,
                "resolution": 0.01
            }))
            .unwrap_or_else(|e| panic!("{e}"));
            let p = build("c", &def, &no_parts).unwrap_or_else(|e| panic!("{op}: {e}"));
            crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{op}: {e}"));
            let (n, _) = crate::organic::self_intersections(&p);
            assert_eq!(n, 0, "{op}: {n} self-intersections");
        }
    }

    /// A sphere cut flat by a box whose face meets it at a grazing angle:
    /// the max distance from the true rim circle to the surface, in cells.
    fn grazing_rim(deg: f32) -> f32 {
        let (r, res) = (0.2f32, 0.008f32);
        let h = r * deg.to_radians().cos();
        let tilt = Quat::from_rotation_z(0.3) * Quat::from_rotation_x(0.2);
        let def: SdfDef = serde_json::from_value(serde_json::json!({
            "shapes": [
                {"shape": "sphere", "radius": [r]},
                {"shape": "box", "size": [1.0, 0.5, 1.0], "at": (tilt * Vec3::new(0.0, h + 0.25, 0.0)).to_array(),
                 "rotation": tilt.to_array(), "op": "subtract"}
            ],
            "blend": 0.0,
            "resolution": res
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let p = build("g", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}"));
        let (n, _) = crate::organic::self_intersections(&p);
        assert_eq!(n, 0, "{deg}: {n} self-intersections");
        let rr = (r * r - h * h).sqrt();
        let mut worst = 0.0f32;
        for k in 0..48 {
            let a = k as f32 / 48.0 * std::f32::consts::TAU;
            let e = tilt * Vec3::new(rr * a.cos(), h, rr * a.sin());
            worst = worst.max(surface_dist(&p, e));
        }
        worst / res
    }

    #[test]
    fn finished_grazing_rims_are_straight() {
        for deg in [8.0, 15.0, 25.0] {
            let d = grazing_rim(deg);
            eprintln!("grazing rim {deg} deg: {d:.3} res");
            assert!(d < 0.05, "{deg} deg rim off by {d} res");
        }
    }

    /// A box cut at a grazing angle into a blended ellipsoid (the cut's
    /// rim runs along the box's edge, nearly in the ellipsoid's surface): no
    /// corner keeps a normal from the other side of a crease.
    #[test]
    fn finished_grazing_cut_has_no_wrong_side_normals() {
        let def: SdfDef = serde_json::from_value(serde_json::json!({"blend": 0.03, "shapes": [
            {"shape": "ellipsoid", "size": [0.2, 0.14, 0.26], "at": [0.7, 0.15, 0]},
            {"shape": "capsule", "from": [0.7, 0.2, 0.1], "to": [0.95, 0.3, 0.25], "radius": [0.06, 0.03]},
            {"shape": "box", "size": [0.3, 0.12, 0.3], "at": [0.62, 0.3, -0.12],
             "rotation": [0.16773126, 0.254887, -0.044943456, 0.95125127], "op": "subtract", "blend": 0}]}))
        .unwrap_or_else(|e| panic!("{e}"));
        let p = build("g", &def, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        let mut bad = 0;
        for t in p.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]));
            let fnn = (b - a).cross(c - a).normalize_or_zero();
            bad += t
                .iter()
                .filter(|&&i| Vec3::from(p.normals[i as usize]).dot(fnn) < 0.7)
                .count();
        }
        assert_eq!(bad, 0, "{bad} corners shade from the wrong side");
        crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}"));
        let (n, _) = crate::organic::self_intersections(&p);
        assert_eq!(n, 0, "{n} self-intersections");
    }

    fn cut_defs() -> (SdfDef, SdfDef, Quat, Vec3) {
        let q = Quat::from_array([0.16773126, 0.254887, -0.044943456, 0.95125127]);
        let full: SdfDef = serde_json::from_value(serde_json::json!({"blend": 0.03, "shapes": [
            {"shape": "ellipsoid", "size": [0.2, 0.14, 0.26], "at": [0.7, 0.15, 0]},
            {"shape": "capsule", "from": [0.7, 0.2, 0.1], "to": [0.95, 0.3, 0.25], "radius": [0.06, 0.03]},
            {"shape": "box", "size": [0.3, 0.12, 0.3], "at": [0.62, 0.3, -0.12],
             "rotation": q.to_array(), "op": "subtract", "blend": 0}]}))
        .unwrap_or_else(|e| panic!("{e}"));
        let mut ell = full.clone();
        ell.shapes.truncate(2);
        (full, ell, q, Vec3::new(0.62, 0.3, -0.12))
    }

    /// The cut's true rims: per sample z along the box's bottom-left edge
    /// (box-local x = -0.15, y = -0.06), the wall / ellipsoid crease, the
    /// floor / ellipsoid crease and the exposed box edge. Returns the max
    /// distance (in cells) of each from the finished surface.
    fn cut_rim_deviation() -> [f32; 3] {
        let (full, ell, q, c) = cut_defs();
        let (_, fin, _, res) = build_probed(&full, &[]);
        let at = |dx: f32, dy: f32, z: f32| c + q * Vec3::new(-0.15 + dx, -0.06 + dy, z);
        let zs: Vec<f32> = (0..41).map(|k| 0.0 + 0.004 * k as f32).collect();
        let steps = 600;
        let span = 6.0 * res;
        let mut pts = Vec::new();
        for &z in &zs {
            for i in 0..=steps {
                let t = span * i as f32 / steps as f32;
                pts.push(at(0.0, t, z)); // up the wall
                pts.push(at(t, 0.0, z)); // along the floor
            }
        }
        // The floor / ellipsoid crease across the floor: per x, scan z.
        let xs: Vec<f32> = (0..31).map(|k| 0.005 + 0.0095 * k as f32).collect();
        let zsteps = 800;
        let floor_at = |dx: f32, i: usize| at(dx, 0.0, -0.15 + 0.3 * i as f32 / zsteps as f32);
        let mut fpts = Vec::new();
        for &dx in &xs {
            for i in 0..=zsteps {
                fpts.push(floor_at(dx, i));
            }
        }
        let (_, _, fe, _) = build_probed(&ell, &fpts);
        let (_, _, ff, _) = build_probed(&full, &fpts);
        let mut floor_worst = 0.0f32;
        for (xi, &dx) in xs.iter().enumerate() {
            let base = xi * (zsteps + 1);
            for i in 1..=zsteps {
                let (e0, e1) = (fe[base + i - 1], fe[base + i]);
                // A sign change of the ellipsoid on the floor, where the floor
                // is really surface (the full field is zero there).
                if (e0 < 0.0) != (e1 < 0.0) && ff[base + i].abs() < 0.05 * res {
                    let p = floor_at(dx, i);
                    floor_worst = floor_worst.max(surface_dist(&fin, p) / res);
                }
            }
        }
        let (_, _, e, _) = build_probed(&ell, &pts);
        let mut worst = [0.0f32, floor_worst, 0.0];
        for (zi, &z) in zs.iter().enumerate() {
            let base = zi * (steps + 1) * 2;
            let e_at = |i: usize, k: usize| e[base + 2 * i + k];
            let t_of = |i: usize| span * i as f32 / steps as f32;
            if e_at(0, 0) < 0.0 {
                // The edge is inside the ellipsoid: a wall rises to the
                // ellipsoid; the edge itself is a concave crease.
                if let Some(i) = (1..=steps).find(|&i| e_at(i, 0) >= 0.0) {
                    // Where the wall is under 2 cells tall (it shrinks to
                    // nothing at the junction) its rim is within a cell of
                    // the edge; the shading count covers that stretch.
                    if t_of(i) > 2.0 * res {
                        let d = surface_dist(&fin, at(0.0, t_of(i), z));
                        worst[0] = worst[0].max(d / res);
                        let d = surface_dist(&fin, at(0.0, 0.0, z));
                        worst[2] = worst[2].max(d / res);
                    }
                }
            } else if let Some(i) = (1..=steps).find(|&i| e_at(i, 1) < 0.0) {
                // The edge is outside: the floor meets the ellipsoid.
                if t_of(i) > 0.3 * res {
                    let d = surface_dist(&fin, at(t_of(i), 0.0, z));
                    worst[1] = worst[1].max(d / res);
                }
            }
        }
        worst
    }

    /// Corners of the finished cut whose shading normal is more than 15
    /// degrees off the field's gradient at their face's centre (faces over a
    /// twentieth of a cell): what shows as teeth along a ragged crease.
    fn cut_shading_errors() -> usize {
        let (full, _, _, _) = cut_defs();
        let (_, fin, _, res) = build_probed(&full, &[]);
        let h = 0.02 * res;
        let mut pts = Vec::new();
        let mut tris: Vec<[u32; 3]> = Vec::new();
        for t in fin.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| Vec3::from(fin.verts[i as usize]));
            if (b - a).cross(c - a).length() * 0.5 < 0.05 * res * res {
                continue;
            }
            let m = (a + b + c) / 3.0;
            tris.push(*t);
            for d in [Vec3::X, Vec3::Y, Vec3::Z] {
                pts.push(m + d * h);
                pts.push(m - d * h);
            }
        }
        let (_, _, f, _) = build_probed(&full, &pts);
        let cos = 15f32.to_radians().cos();
        let mut bad = 0;
        for (i, t) in tris.iter().enumerate() {
            let g = Vec3::new(
                f[6 * i] - f[6 * i + 1],
                f[6 * i + 2] - f[6 * i + 3],
                f[6 * i + 4] - f[6 * i + 5],
            )
            .normalize_or_zero();
            let nb = t
                .iter()
                .filter(|&&j| Vec3::from(fin.normals[j as usize]).dot(g) < cos)
                .count();
            bad += nb;
        }
        bad
    }

    #[test]
    fn finished_cut_rims_are_straight() {
        let w = cut_rim_deviation();
        let bad = cut_shading_errors();
        eprintln!(
            "cut rims: wall/ellipsoid {:.3} res, floor/ellipsoid {:.3} res, box edge {:.3} res; {bad} corners shaded > 15 deg off",
            w[0], w[1], w[2]
        );
        assert!(w[0] < 0.02 && w[1] < 0.1 && w[2] < 0.02, "rims off: {w:?}");
        assert!(bad < 120, "{bad} corners shaded off the surface");
    }

    /// Output vertices that are extra copies of a position (split normals).
    fn split_verts(p: &Part) -> usize {
        let mut seen = std::collections::HashSet::new();
        for v in &p.verts {
            seen.insert(v.map(|x| (x * 1e5).round() as i64));
        }
        p.verts.len() - seen.len()
    }

    fn smooth_shapes() -> Vec<(&'static str, Part)> {
        let sdf = |v: serde_json::Value| -> Part {
            let d: SdfDef = serde_json::from_value(v).unwrap_or_else(|e| panic!("{e}"));
            build("s", &d, &no_parts).unwrap_or_else(|e| panic!("{e}"))
        };
        let mut out = vec![
            (
                "thin capsule",
                sdf(
                    serde_json::json!({"shapes": [{"shape": "capsule", "from": [0, 0.02, 0], "to": [0.3, 0.1, 0.05], "radius": [0.012]}]}),
                ),
            ),
            (
                "thin capsule coarse",
                sdf(
                    serde_json::json!({"resolution": 0.008, "shapes": [{"shape": "capsule", "from": [0, 0.02, 0], "to": [0.3, 0.1, 0.05], "radius": [0.012]}]}),
                ),
            ),
            (
                "small torus",
                sdf(serde_json::json!({"shapes": [{"shape": "torus", "size": [0.05, 0.012]}]})),
            ),
            (
                "small torus coarse",
                sdf(
                    serde_json::json!({"resolution": 0.006, "shapes": [{"shape": "torus", "size": [0.05, 0.012]}]}),
                ),
            ),
            ("car body", {
                let ring = |r: serde_json::Value| -> Vec<Vec<[f32; 2]>> {
                    let r: Vec<[f32; 2]> =
                        serde_json::from_value(r).unwrap_or_else(|e| panic!("{e}"));
                    crate::silhouette::shape_rings(vec![r], true, 0.0)
                };
                let side = ring(serde_json::json!([
                    [2.2, 0.3],
                    [2.25, 0.75],
                    [1.0, 0.85],
                    [0.4, 1.4],
                    [-1.2, 1.4],
                    [-2.2, 0.95],
                    [-2.2, 0.3]
                ]));
                let top = ring(serde_json::json!([
                    [-0.8, 2.2],
                    [0.8, 2.2],
                    [0.9, 1.2],
                    [0.9, -1.4],
                    [0.85, -2.2],
                    [-0.85, -2.2],
                    [-0.9, -1.4],
                    [-0.9, 1.2]
                ]));
                let front = ring(serde_json::json!([
                    [-0.9, 0.3],
                    [0.9, 0.3],
                    [0.92, 0.85],
                    [0.65, 1.42],
                    [-0.65, 1.42],
                    [-0.92, 0.85]
                ]));
                // Rounded well over the sharpening limit (2 cells of the
                // ~0.037 m default voxel), so only the arches are creases.
                sdf(serde_json::json!({"blend": 0.15, "mirror": "x", "shapes": [
                    {"shape": "silhouette", "round": 0.15, "side": side, "top": top, "front": front},
                    {"shape": "cylinder", "op": "subtract", "blend": 0.03, "radius": [0.4], "height": 0.5,
                     "at": [0.8, 0.33, 1.35], "rotation": [0.0, 0.0, 0.70710677, 0.70710677]}]}))
            }),
        ];
        let skin: SkinDef = serde_json::from_value(serde_json::json!({"chains": [[[0, 0.1, 0, [0.03, 0.02]], [0.1, 0.12, 0.02, [0.025, 0.015], 20], [0.2, 0.1, 0.05, [0.015, 0.01]]]]}))
            .unwrap_or_else(|e| panic!("{e}"));
        out.push(("oval skin limb", skin_build(&skin)));
        let body: crate::body::BodyDef =
            serde_json::from_value(serde_json::json!({"preset": "quadruped"}))
                .unwrap_or_else(|e| panic!("{e}"));
        out.push((
            "quadruped",
            crate::body::build("q", &body, &no_parts).unwrap_or_else(|e| panic!("{e}")),
        ));
        out
    }

    fn skin_build(d: &SkinDef) -> Part {
        skin("k", d, &no_parts).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Tight smooth bends at coarse resolutions are not creases: no
    /// vertex of a smooth shape gets split normals. The car body has real
    /// creases only round its unrounded wheel-arch cutters.
    #[test]
    fn finished_smooth_shapes_have_no_split_normals() {
        for (name, p) in smooth_shapes() {
            let n = split_verts(&p);
            eprintln!("{name}: {n} split vertices of {}", p.verts.len());
            if name == "car body" {
                // Splits away from the cutters (outside 0.15 m of either).
                let far = p
                    .verts
                    .iter()
                    .map(|v| Vec3::from(*v))
                    .filter(|v| {
                        let r = Vec3::new(0.0, v.y - 0.33, v.z.abs() - 1.35).length();
                        !(r < 0.55 && (v.x.abs() - 0.8).abs() < 0.4)
                    })
                    .count();
                let all = p.verts.len();
                let uniq_far = {
                    let mut seen = std::collections::HashSet::new();
                    p.verts
                        .iter()
                        .map(|v| Vec3::from(*v))
                        .filter(|v| {
                            let r = Vec3::new(0.0, v.y - 0.33, v.z.abs() - 1.35).length();
                            !(r < 0.55 && (v.x.abs() - 0.8).abs() < 0.4)
                        })
                        .for_each(|v| {
                            seen.insert(v.to_array().map(|x| (x * 1e5).round() as i64));
                        });
                    seen.len()
                };
                eprintln!(
                    "car body: {} split vertices away from the arches (of {all})",
                    far - uniq_far
                );
                assert_eq!(far - uniq_far, 0, "car body splits away from its arches");
            } else {
                assert_eq!(n, 0, "{name}: {n} split vertices");
            }
        }
    }

    /// A box with a tilted cylinder hole, subtracted with `blend` (in
    /// cells): along the top rim (the unblended crease of the box top and
    /// the cylinder), the spread (max - min) of the surface's distance from
    /// the crease, in cells, and the rim corners shaded > 15 degrees off.
    fn blended_rim(blend_cells: f32) -> (f32, f32, usize) {
        let res = 0.01f32;
        let tilt = Quat::IDENTITY;
        let def: SdfDef = serde_json::from_value(serde_json::json!({
            "resolution": res,
            "shapes": [
                {"shape": "box", "size": [0.6, 0.4, 0.6], "round": 0.03},
                {"shape": "cylinder", "op": "subtract", "blend": blend_cells * res, "radius": [0.1],
                 "height": 1.0, "rotation": tilt.to_array()}
            ]
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let (_, fin, _, _) = build_probed(&def, &[]);
        let axis = tilt * Vec3::Y;
        // The top rim: on y = 0.2, distance to the axis = 0.1.
        let mut dists = Vec::new();
        let mut rim = Vec::new();
        for k in 0..72 {
            let a = k as f32 / 72.0 * std::f32::consts::TAU;
            let dir = Vec3::new(a.cos(), 0.0, a.sin());
            let c = Vec3::new(0.0, 0.2, 0.0);
            let off = |t: f32| {
                let p = c + dir * t;
                (p - axis * p.dot(axis)).length() - 0.1
            };
            let (mut lo, mut hi) = (0.0f32, 0.3f32);
            for _ in 0..40 {
                let m = 0.5 * (lo + hi);
                if off(m) < 0.0 { lo = m } else { hi = m }
            }
            let p = c + dir * lo;
            rim.push(p);
            dists.push(surface_dist(&fin, p) / res);
        }
        let spread = dists.iter().cloned().fold(f32::MIN, f32::max)
            - dists.iter().cloned().fold(f32::MAX, f32::min);
        let mean = dists.iter().sum::<f32>() / dists.len() as f32;
        // Rim corners shaded off the field's gradient at their face centre.
        let h = 0.02 * res;
        let mut pts = Vec::new();
        let mut tris = Vec::new();
        for t in fin.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| Vec3::from(fin.verts[i as usize]));
            let m = (a + b + c) / 3.0;
            if (b - a).cross(c - a).length() * 0.5 < 0.05 * res * res
                || rim.iter().all(|r| r.distance(m) > 2.0 * res)
            {
                continue;
            }
            tris.push(*t);
            for d in [Vec3::X, Vec3::Y, Vec3::Z] {
                pts.push(m + d * h);
                pts.push(m - d * h);
            }
        }
        let (_, _, f, _) = build_probed(&def, &pts);
        let cos = 15f32.to_radians().cos();
        let mut bad = 0;
        for (i, t) in tris.iter().enumerate() {
            let g = Vec3::new(
                f[6 * i] - f[6 * i + 1],
                f[6 * i + 2] - f[6 * i + 3],
                f[6 * i + 4] - f[6 * i + 5],
            )
            .normalize_or_zero();
            bad += t
                .iter()
                .filter(|&&j| Vec3::from(fin.normals[j as usize]).dot(g) < cos)
                .count();
        }
        (spread, mean, bad)
    }

    #[test]
    fn finished_blended_rims_are_clean() {
        for b in [0.5, 1.0, 1.5, 2.0, 3.0] {
            let (spread, mean, bad) = blended_rim(b);
            eprintln!(
                "blend {b} cells: rim distance spread {spread:.3} res (mean {mean:.3}), {bad} corners shaded off"
            );
            // The rim runs at an even distance from the crease (no zigzag).
            assert!(spread < 0.1, "blend {b}: rim zigzags by {spread} res");
            if b < SHARPEN_CELLS {
                // Meshed sharp: on the crease, shaded per side.
                assert!(mean < 0.05, "blend {b}: rim {mean} res off the crease");
                assert!(bad < 10, "blend {b}: {bad} rim corners shaded off");
            }
        }
    }

    /// Tight smooth valleys (a quadruped's hips and neck) don't shade as
    /// specks: no corner normal far off its face.
    #[test]
    fn finished_tight_valleys_shade_without_specks() {
        let body: crate::body::BodyDef =
            serde_json::from_value(serde_json::json!({"preset": "quadruped"}))
                .unwrap_or_else(|e| panic!("{e}"));
        let p = crate::body::build("q", &body, &no_parts).unwrap_or_else(|e| panic!("{e}"));
        let mut bad = 0;
        let mut worst = 0.0f32;
        for t in p.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]));
            let n = (b - a).cross(c - a).normalize_or_zero();
            for &j in t {
                let d = Vec3::from(p.normals[j as usize]).dot(n);
                if d < 0.5 {
                    bad += 1;
                }
                worst = worst.min(d);
            }
        }
        eprintln!("cat corners over 60 deg off their face: {bad}, worst dot {worst}");
        assert!(
            bad < 25 && worst > -0.05,
            "{bad} corners off, worst {worst}"
        );
    }
}
