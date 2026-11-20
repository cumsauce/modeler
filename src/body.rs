//! `body`: parametric archetypes (biped, quadruped, bird, fish, serpent,
//! car) lowered to an `sdf` of skin chains with elliptical sections.
//!
//! Each preset is written once at a reference size as named joints (a tree:
//! every joint but the root has a parent) and chains through them. A chain
//! point is a joint, a point between two joints or a joint plus an offset,
//! each with its own section, so a limb can start inside the body with its
//! own size while the joint stays shared. The proportion params move and
//! size joints inside the preset; then the whole thing is grounded on y = 0
//! and scaled so the preset's measure equals `height`; then the per-joint
//! overrides apply (offsets carry a joint's descendants) and it is grounded
//! again. The joints become the skeleton (`<name>_rig`) and anchors
//! (`cat:paw_l`).

use std::collections::BTreeMap;

use bevy::math::{Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::sdf::{SdfDef, Sect, Shape};

fn none<T>(v: &Option<T>) -> bool {
    v.is_none()
}

pub const PRESETS: &[&str] = &[
    "biped",
    "quadruped",
    "bird",
    "fish",
    "serpent",
    "car",
    "human",
];

/// A per-joint edit: its sections scaled, it (and every joint below it)
/// moved by `offset` meters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JointEdit {
    #[serde(default, skip_serializing_if = "none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub offset: Option<[f32; 3]>,
}

/// The parameters of a `body` op.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct BodyDef {
    pub preset: String,
    #[serde(default, skip_serializing_if = "none")]
    pub height: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub head_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub build: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub leg_length: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub tail: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub snout: Option<f32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub joints: BTreeMap<String, JointEdit>,
    #[serde(default, skip_serializing_if = "none")]
    pub blend: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub resolution: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub tris: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bake: bool,
    /// false: no automatic `<name>_rig` skeleton.
    #[serde(default, skip_serializing_if = "none")]
    pub rig: Option<bool>,
    /// false: no automatic smooth bind of the body to `<name>_rig`.
    #[serde(default, skip_serializing_if = "none")]
    pub bind: Option<bool>,
    /// Field warps, as on sdf (local sculpt warps may be centred on a
    /// joint by name: `"at": "head"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warps: Vec<crate::deform::Deform>,
    /// Set by the graph (never read or saved), as on sdf.
    #[serde(default, skip_deserializing, skip_serializing_if = "none")]
    pub target_tris: Option<u32>,
    // ---- `human` (the template preset) only
    /// 0 female .. 1 male (default 0.5).
    #[serde(default, skip_serializing_if = "none")]
    pub gender: Option<f32>,
    /// Years (> 1), or MakeHuman's 0..1 (default 25 years).
    #[serde(default, skip_serializing_if = "none")]
    pub age: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub muscle: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub weight: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub proportions: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub breast_size: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub breast_firmness: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub ethnicity: Option<BTreeMap<String, f32>>,
    /// Detail targets by name, -1..1.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shape: BTreeMap<String, f32>,
    /// Expression units, 0..1.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expression: BTreeMap<String, f32>,
    /// Catmull-Clark levels over the quad cage (0..2).
    #[serde(default, skip_serializing_if = "none")]
    pub detail: Option<u32>,
    /// Extras to make (eyes, teeth, tongue; default all).
    #[serde(default, skip_serializing_if = "none")]
    pub parts: Option<Vec<String>>,
    /// Named morph targets (exported as glTF morph targets).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shapes: BTreeMap<String, crate::human::ShapeKey>,
    /// `rig: "full"`: the whole template skeleton instead of the game rig.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rig_full: bool,
    /// Iris colour of the built-in eye material (RGBA 0..1).
    #[serde(default, skip_serializing_if = "none")]
    pub eye_color: Option<[f32; 4]>,
    /// Pupil size of the built-in eye material (1 = normal).
    #[serde(default, skip_serializing_if = "none")]
    pub pupil: Option<f32>,
    /// Body measurements to hit (meters), solved with the measure targets.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub measure: BTreeMap<String, f32>,
    /// Skin tone: 0 (light) .. 1 (dark) along a melanin ramp, or a colour.
    #[serde(default, skip_serializing_if = "none")]
    pub skin_tone: Option<crate::human::look::SkinTone>,
    /// Hair: a style name or `{style, length, color, part, fringe, volume, cards, tris, seed}`.
    #[serde(default, skip_serializing_if = "none")]
    pub hair: Option<crate::human::hair::HairIn>,
    /// Eyebrows: `false` or `{thickness, arch, color}` (on by default).
    #[serde(default, skip_serializing_if = "none")]
    pub brows: Option<crate::human::hair::Toggle<crate::human::hair::BrowsDef>>,
    /// Eyelashes: `false` or `{length, curl, color}` (on by default).
    #[serde(default, skip_serializing_if = "none")]
    pub lashes: Option<crate::human::hair::Toggle<crate::human::hair::LashesDef>>,
}

/// The human-only fields, for errors on other presets.
fn human_fields(d: &BodyDef) -> Vec<&'static str> {
    let mut v = Vec::new();
    for (k, set) in [
        ("gender", d.gender.is_some()),
        ("age", d.age.is_some()),
        ("muscle", d.muscle.is_some()),
        ("weight", d.weight.is_some()),
        ("proportions", d.proportions.is_some()),
        ("breast_size", d.breast_size.is_some()),
        ("breast_firmness", d.breast_firmness.is_some()),
        ("ethnicity", d.ethnicity.is_some()),
        ("shape", !d.shape.is_empty()),
        ("expression", !d.expression.is_empty()),
        ("detail", d.detail.is_some()),
        ("parts", d.parts.is_some()),
        ("shapes", !d.shapes.is_empty()),
        ("rig: \"full\"", d.rig_full),
        ("eye_color", d.eye_color.is_some()),
        ("pupil", d.pupil.is_some()),
        ("measure", !d.measure.is_empty()),
        ("skin_tone", d.skin_tone.is_some()),
        ("hair", d.hair.is_some()),
        ("brows", d.brows.is_some()),
        ("lashes", d.lashes.is_some()),
    ] {
        if set {
            v.push(k);
        }
    }
    v
}

/// (default height, what it measures, default head_ratio).
pub fn defaults(preset: &str) -> (f32, &'static str, f32) {
    match preset {
        "biped" => (1.75, "standing height", 1.0 / 7.5),
        "quadruped" => (0.25, "shoulder height", 0.36),
        "bird" => (0.25, "standing height", 0.2),
        "fish" => (0.4, "length", 0.25),
        "serpent" => (2.0, "length", 0.03),
        "human" => (1.7, "standing height", 1.0 / 7.5),
        _ => (4.3, "length", 0.12),
    }
}

impl BodyDef {
    pub fn validate(&self) -> Result<(), String> {
        if !PRESETS.contains(&self.preset.as_str()) {
            return Err(format!(
                "preset: `{}` is not a body preset (use {})",
                self.preset,
                PRESETS.join(", ")
            ));
        }
        if self.preset == "human" {
            for (k, set) in [
                ("head_ratio", self.head_ratio.is_some()),
                ("build", self.build.is_some()),
                ("leg_length", self.leg_length.is_some()),
                ("tail", self.tail.is_some()),
                ("snout", self.snout.is_some()),
                ("joints", !self.joints.is_empty()),
                ("blend", self.blend.is_some()),
                ("resolution", self.resolution.is_some()),
            ] {
                if set {
                    return Err(format!(
                        "{k}: not a field of the human preset (it takes gender, age, muscle, weight, height, proportions, ethnicity, shape, expression, shapes, detail, parts, warps, tris)"
                    ));
                }
            }
            if self
                .height
                .is_some_and(|h| !h.is_finite() || !(0.3..=3.0).contains(&h))
            {
                return Err("height: a human's stature, 0.3..3 m".into());
            }
            if let Some(t) = self.tris
                && t < 100
            {
                return Err(format!("tris: at least 100 for a human, got {t}"));
            }
            if self.bake && self.tris.is_none() {
                return Err("bake: needs `tris`".into());
            }
            return crate::human::validate(self);
        }
        let hf = human_fields(self);
        if !hf.is_empty() {
            return Err(format!(
                "{}: only the human preset takes {}",
                hf.join(", "),
                if hf.len() == 1 { "it" } else { "them" }
            ));
        }
        let pos = |f: &str, v: Option<f32>, lo: f32, hi: f32| -> Result<(), String> {
            match v {
                Some(x) if !x.is_finite() || x < lo || x > hi => {
                    Err(format!("{f}: must be in {lo}..{hi}, got {x}"))
                }
                _ => Ok(()),
            }
        };
        pos("height", self.height, 1e-3, 1e4)?;
        pos("head_ratio", self.head_ratio, 1e-3, 2.0)?;
        pos("build", self.build, 0.3, 3.0)?;
        pos("leg_length", self.leg_length, 0.2, 3.0)?;
        pos("tail", self.tail, 0.0, 4.0)?;
        pos("snout", self.snout, 0.0, 4.0)?;
        pos("blend", self.blend, 0.0, 1e3)?;
        if self.resolution.is_some_and(|r| !r.is_finite() || r <= 0.0) {
            return Err("resolution: must be > 0 meters".into());
        }
        match self.tris {
            Some(t) if t < 4 => return Err(format!("tris: at least 4 triangles, got {t}")),
            None if self.bake => return Err("bake: needs `tris`".into()),
            _ => {}
        }
        for (k, e) in &self.joints {
            if e.scale.is_some_and(|s| !s.is_finite() || s <= 0.0) {
                return Err(format!("joints.{k}.scale: must be > 0"));
            }
            if e.offset.is_some_and(|o| o.iter().any(|v| !v.is_finite())) {
                return Err(format!("joints.{k}.offset: must be finite"));
            }
        }
        crate::sdf::check_warps(&self.warps)?;
        // Joint names depend on the preset: lower once to check them.
        lower(self).map(|_| ())
    }

    pub fn summary(&self) -> String {
        let mut s = self.preset.clone();
        for (k, v) in [
            ("height", self.height),
            ("head_ratio", self.head_ratio),
            ("build", self.build),
            ("leg_length", self.leg_length),
            ("tail", self.tail),
            ("snout", self.snout),
            ("blend", self.blend),
            ("resolution", self.resolution),
        ] {
            if let Some(v) = v {
                s += &format!(" {k}={v}");
            }
        }
        for (k, v) in [
            ("gender", self.gender),
            ("age", self.age),
            ("muscle", self.muscle),
            ("weight", self.weight),
            ("proportions", self.proportions),
            ("breast_size", self.breast_size),
            ("breast_firmness", self.breast_firmness),
        ] {
            if let Some(v) = v {
                s += &format!(" {k}={v}");
            }
        }
        if let Some(d) = self.detail {
            s += &format!(" detail={d}");
        }
        if !self.shape.is_empty() {
            s += &format!(" shape[{}]", self.shape.len());
        }
        if !self.expression.is_empty() {
            s += &format!(" expression[{}]", self.expression.len());
        }
        if !self.shapes.is_empty() {
            let k: Vec<&str> = self.shapes.keys().map(String::as_str).collect();
            s += &format!(" shapes[{}]", k.join(", "));
        }
        if self.rig_full {
            s += " rig=full";
        }
        for w in &self.warps {
            s += &format!(" warp({})", w.summary());
        }
        if !self.joints.is_empty() {
            let k: Vec<&str> = self.joints.keys().map(String::as_str).collect();
            s += &format!(" joints[{}]", k.join(", "));
        }
        if let Some(t) = self.tris {
            s += &format!(" tris={t}");
        }
        if self.bake {
            s += " bake";
        }
        if self.rig == Some(false) {
            s += " rig=false";
        }
        if self.bind == Some(false) {
            s += " bind=false";
        }
        s
    }
}

/// A joint of the lowered body: name, parent, world position, radius.
#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    pub name: String,
    pub parent: Option<String>,
    pub at: Vec3,
    pub radius: f32,
}

pub struct Lowered {
    pub sdf: SdfDef,
    pub joints: Vec<Joint>,
}

// ------------------------------------------------------------- preset model

#[derive(Clone, Copy)]
enum At {
    J(usize),
    /// Between two joints, 0 = the first.
    Mid(usize, usize, f32),
    /// A joint plus an offset (scaled with the body).
    Off(usize, Vec3),
    /// Between two joints plus an offset (scaled with the body).
    MidOff(usize, usize, f32, Vec3),
}

#[derive(Clone, Copy)]
struct Pt {
    at: At,
    r: [f32; 2],
    /// Superellipse exponent of the section (2 = ellipse).
    n: f32,
}

/// A small sphere carved out of (or added onto) the skin after the chains:
/// eye sockets, eyeballs.
#[derive(Clone, Copy)]
struct Dot {
    at: At,
    r: f32,
    subtract: bool,
    /// Smooth radius (scaled with the body).
    blend: f32,
}

/// An ellipsoid (axis-aligned radii) joined after the chains: muscle
/// masses, skull, cheekbones.
#[derive(Clone, Copy)]
struct Ell {
    at: At,
    r: Vec3,
    /// Smooth radius (scaled with the body); `None` = the item's blend.
    blend: Option<f32>,
    /// Joined before the chains (torso masses the limbs then blend into),
    /// rather than after them.
    early: bool,
}

struct Wheel {
    j: usize,
    r: f32,
    w: f32,
}

#[derive(Default)]
struct Pre {
    joints: Vec<(String, Option<usize>, Vec3)>,
    chains: Vec<Vec<Pt>>,
    /// Smooth radius of a chain's join (scaled with the body), by chain
    /// index; the rest use `blend`.
    chain_blends: Vec<(usize, f32)>,
    /// Joint radii (scaled with the body) for joints that no chain section
    /// sits on (the biped's ellipsoid torso).
    rads: Vec<(usize, f32)>,
    dots: Vec<Dot>,
    ells: Vec<Ell>,
    wheels: Vec<Wheel>,
    /// Flat floor cut (car underside), y.
    floor: Option<f32>,
    blend: f32,
    /// Resample chains finely before meshing: the sdf samples a straight
    /// oval chain only every ~0.75 radii, so a radius that swells and
    /// narrows (ribcage, waist, calf) would show a crease ring at every
    /// sample.
    dense: bool,
}

impl Pre {
    fn j(&mut self, name: &str, parent: Option<usize>, p: Vec3) -> usize {
        self.joints.push((name.to_string(), parent, p));
        self.joints.len() - 1
    }
    fn pos(&self, i: usize) -> Vec3 {
        self.joints[i].2
    }
    fn at(&self, a: At) -> Vec3 {
        match a {
            At::J(i) => self.pos(i),
            At::Mid(a, b, t) => self.pos(a).lerp(self.pos(b), t),
            At::Off(i, d) => self.pos(i) + d,
            At::MidOff(a, b, t, d) => self.pos(a).lerp(self.pos(b), t) + d,
        }
    }
    fn chain(&mut self, pts: Vec<Pt>) {
        self.chains.push(pts);
    }
    fn sects(&self) -> Vec<Vec<Sect>> {
        self.chains
            .iter()
            .map(|c| {
                let pts: Vec<(Vec3, [f32; 2], f32)> =
                    c.iter().map(|p| (self.at(p.at), p.r, p.n)).collect();
                let pts = if self.dense { densify(&pts) } else { pts };
                pts.into_iter()
                    .map(|(q, r, n)| {
                        if n == 2.0 {
                            Sect::Oval(q.x, q.y, q.z, r)
                        } else {
                            Sect::Super(q.x, q.y, q.z, r, 0.0, n)
                        }
                    })
                    .collect()
            })
            .collect()
    }
    /// Lowest and highest reach along `dir`.
    fn extent(&self, dir: Vec3) -> (f32, f32) {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for c in self.sects() {
            let (a, b) = crate::sdf::chain_extent(&c, true, dir);
            lo = lo.min(a);
            hi = hi.max(b);
        }
        for e in &self.ells {
            let c = self.at(e.at).dot(dir);
            let r = (e.r * dir).length();
            lo = lo.min(c - r);
            hi = hi.max(c + r);
        }
        for w in &self.wheels {
            let c = self.pos(w.j);
            let e = 0.5 * w.w * dir.x.abs() + w.r * (dir.y * dir.y + dir.z * dir.z).sqrt();
            lo = lo.min(c.dot(dir) - e);
            hi = hi.max(c.dot(dir) + e);
        }
        (lo, hi)
    }
    /// Scales everything about the origin.
    fn scale(&mut self, s: f32) {
        for j in &mut self.joints {
            j.2 *= s;
        }
        for c in &mut self.chains {
            for p in c {
                p.r = [p.r[0] * s, p.r[1] * s];
                scale_at(&mut p.at, s);
            }
        }
        for d in &mut self.dots {
            d.r *= s;
            d.blend *= s;
            scale_at(&mut d.at, s);
        }
        for e in &mut self.ells {
            e.r *= s;
            if let Some(b) = &mut e.blend {
                *b *= s;
            }
            scale_at(&mut e.at, s);
        }
        for w in &mut self.wheels {
            w.r *= s;
            w.w *= s;
        }
        if let Some(f) = &mut self.floor {
            *f *= s;
        }
        self.blend *= s;
        for c in &mut self.chain_blends {
            c.1 *= s;
        }
        for r in &mut self.rads {
            r.1 *= s;
        }
    }
    fn shift_y(&mut self, dy: f32) {
        for j in &mut self.joints {
            j.2.y += dy;
        }
        if let Some(f) = &mut self.floor {
            *f += dy;
        }
    }
    fn descendants(&self, root: usize) -> Vec<usize> {
        let mut out = vec![root];
        let mut i = 0;
        while i < out.len() {
            let a = out[i];
            for (k, j) in self.joints.iter().enumerate() {
                if j.1 == Some(a) && !out.contains(&k) {
                    out.push(k);
                }
            }
            i += 1;
        }
        out
    }
}

/// A chain's points resampled along a uniform Catmull-Rom (positions and
/// radii; ends extrapolated straight with level radii, as the sdf does),
/// about a fifth of the local radius apart.
fn densify(p: &[(Vec3, [f32; 2], f32)]) -> Vec<(Vec3, [f32; 2], f32)> {
    let n = p.len();
    if n < 3 {
        return p.to_vec();
    }
    let get = |i: isize| -> (Vec3, Vec2) {
        let c = |k: usize| (p[k].0, Vec2::from(p[k].1));
        if i < 0 {
            (2.0 * p[0].0 - p[1].0, c(1).1)
        } else if i as usize >= n {
            (2.0 * p[n - 1].0 - p[n - 2].0, c(n - 2).1)
        } else {
            c(i as usize)
        }
    };
    let cr = |a: f32, b: f32, c: f32, d: f32, t: f32| {
        0.5 * (2.0 * b
            + (c - a) * t
            + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t
            + (3.0 * b - a - 3.0 * c + d) * t * t * t)
    };
    let mut out = Vec::new();
    for s in 0..n - 1 {
        let i = s as isize;
        let q = [get(i - 1), get(i), get(i + 1), get(i + 2)];
        let rmin = q[1].1.min_element().min(q[2].1.min_element()).max(1e-4);
        let len = q[1].0.distance(q[2].0);
        let k = ((len / (0.5 * rmin).max(0.03)).ceil() as usize).clamp(1, 12);
        for j in 0..k {
            let t = j as f32 / k as f32;
            let pos = Vec3::new(
                cr(q[0].0.x, q[1].0.x, q[2].0.x, q[3].0.x, t),
                cr(q[0].0.y, q[1].0.y, q[2].0.y, q[3].0.y, t),
                cr(q[0].0.z, q[1].0.z, q[2].0.z, q[3].0.z, t),
            );
            let lo = 0.5 * q[1].1.min(q[2].1);
            let r = Vec2::new(
                cr(q[0].1.x, q[1].1.x, q[2].1.x, q[3].1.x, t),
                cr(q[0].1.y, q[1].1.y, q[2].1.y, q[3].1.y, t),
            )
            .max(lo);
            let e = p[s].2 + (p[s + 1].2 - p[s].2) * t;
            out.push((pos, r.to_array(), e));
        }
    }
    out.push(p[n - 1]);
    out
}

fn scale_at(a: &mut At, s: f32) {
    match a {
        At::Off(_, d) | At::MidOff(_, _, _, d) => *d *= s,
        At::J(_) | At::Mid(..) => {}
    }
}

fn v(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

fn pt(at: At, rx: f32, ry: f32) -> Pt {
    Pt {
        at,
        r: [rx, ry],
        n: 2.0,
    }
}

/// A superellipse section (`n` > 2 is boxier).
fn ptn(at: At, rx: f32, ry: f32, n: f32) -> Pt {
    Pt { at, r: [rx, ry], n }
}

struct P {
    k: f32,
    b: f32,
    leg: f32,
    tail: f32,
    snout: f32,
}

const SIDES: [(f32, &str); 2] = [(1.0, "_l"), (-1.0, "_r")];

/// An adult mannequin built to the ANSUR II male medians (2012 US Army
/// Anthropometric Survey, NATICK/TR-15/007; stature 175.5 cm), head 1/7.5
/// of the height, arms relaxed a little out from the body, facing +z
/// (left = +x). Values are metres at that stature; `lower` scales the
/// figure to `height`, so every landmark keeps its fraction of stature.
///
/// Built from smooth pieces blended together rather than long chains: a
/// chain whose radius narrows and swells again (a waist, a knee) meshes
/// with a crease ring at every sample, while ellipsoids and single tapered
/// segments joined by smooth unions give soft valleys. Ribcage, abdomen and
/// pelvis are ellipsoids; each limb bone is one tapered segment plus a
/// muscle mass (deltoid, biceps, forearm, quadriceps, calf).
fn biped(p: &P) -> Pre {
    let (k, b, l) = (p.k, p.b, p.leg);
    let bx = 1.0 + (b - 1.0) * 0.5;
    let mut m = Pre {
        blend: 0.022,
        dense: true,
        ..Pre::default()
    };
    // Muscle masses grow in girth (x, z) with `build`, not in length.
    let g = |x: f32, y: f32, z: f32| v(x * b, y, z * b);
    let ell = |at: At, r: Vec3, blend: Option<f32>| Ell {
        at,
        r,
        blend,
        early: true,
    };
    // Trochanter (hip joint level) at 0.512 of stature.
    let hy = 0.9 * l;
    let up = hy - 0.9;
    let hip = m.j("hip", None, v(0.0, 0.96 + up, -0.005));
    let spine = m.j("spine", Some(hip), v(0.0, 1.09 + up, -0.015));
    let chest = m.j("chest", Some(spine), v(0.0, 1.27 + up, 0.0));
    let neck = m.j("neck", Some(chest), v(0.0, 1.48 + up, -0.035));
    let head = m.j("head", Some(neck), m.pos(neck) + k * v(0.0, 0.12, 0.035));
    let jaw = m.j(
        "jaw",
        Some(head),
        m.pos(head) + k * v(0.0, -0.06, 0.05 + 0.01 * (p.snout - 1.0)),
    );
    // Torso. Pelvis: hip breadth 34.4 with the thighs, buttock depth 24.6.
    // Waist (navel, 0.601): 29.5 x 21 (a lean mannequin; `build` 1.15
    // reaches the soldier median). Ribcage: an egg widest low, chest
    // breadth 28.9 and depth 25.3 at the nipples (0.734), its top at the
    // clavicles.
    let tb = Some(0.065 * bx);
    m.ells.push(ell(
        At::Off(hip, v(0.0, -0.025, 0.008)),
        g(0.155, 0.09, 0.105),
        None,
    ));
    m.ells.push(ell(
        At::Off(spine, v(0.0, -0.03, 0.0)),
        g(0.148, 0.12, 0.105),
        tb,
    ));
    m.ells.push(ell(
        At::Off(chest, v(0.0, -0.02, -0.01)),
        g(0.142, 0.185, 0.125),
        tb,
    ));
    m.rads.push((hip, 0.13 * b));
    m.rads.push((spine, 0.15 * b));
    m.rads.push((chest, 0.17 * b));
    for (s, _) in SIDES {
        // Glutes, pectoral plate, shoulder blade, latissimus.
        m.ells.push(ell(
            At::Off(hip, v(0.072 * s, -0.06, -0.058)),
            g(0.07, 0.085, 0.082),
            Some(0.045 * bx),
        ));
        m.ells.push(ell(
            At::Off(chest, v(0.06 * s, 0.03, 0.06)),
            g(0.07, 0.055, 0.05),
            Some(0.04 * bx),
        ));
        m.ells.push(ell(
            At::Off(chest, v(0.07 * s, 0.06, -0.07)),
            g(0.06, 0.08, 0.045),
            Some(0.035 * bx),
        ));
        m.ells.push(ell(
            At::Off(chest, v(0.09 * s, -0.03, -0.03)),
            g(0.056, 0.1, 0.07),
            Some(0.04 * bx),
        ));
    }
    // Upper chest and shoulder girdle, out under the deltoids.
    m.ells.push(ell(
        At::Off(chest, v(0.0, 0.11, -0.012)),
        g(0.165, 0.06, 0.095),
        Some(0.04 * bx),
    ));
    // Neck: 12.6 across (39.5 around), 13.8 at its base.
    m.chain(vec![
        pt(At::Mid(chest, neck, 0.75), 0.064 * bx, 0.062 * bx),
        pt(At::Off(head, k * v(0.0, -0.045, -0.03)), 0.06 * k, 0.06 * k),
    ]);
    m.rads.push((neck, 0.063 * bx));
    // Head about its joint: crown +0.155, eyes +0.04 (half the head),
    // nose base -0.018, chin -0.078; cranium 15.4 wide x 20.0 long,
    // cheekbones 14.3, pupils 6.4 apart, ears from brow to nose base.
    let h = |x: f32, y: f32, z: f32| At::Off(head, k * v(x, y, z));
    let hb = |x: f32| Some(x * k);
    m.ells
        .push(ell(h(0.0, 0.055, -0.02), k * v(0.077, 0.1, 0.095), None));
    m.ells.push(ell(
        h(0.0, 0.0, 0.028),
        k * v(0.062, 0.062, 0.056),
        hb(0.02),
    ));
    m.chain(vec![
        pt(h(0.0, -0.005, 0.03), 0.048 * k, 0.045 * k),
        pt(At::J(jaw), 0.022 * k, 0.02 * k),
    ]);
    m.rads.push((head, 0.1 * k));
    m.rads.push((jaw, 0.04 * k));
    // Brow and nose.
    m.chain(vec![
        pt(h(-0.04, 0.056, 0.07), 0.012 * k, 0.01 * k),
        pt(h(0.0, 0.06, 0.08), 0.012 * k, 0.01 * k),
        pt(h(0.04, 0.056, 0.07), 0.012 * k, 0.01 * k),
    ]);
    m.chain(vec![
        pt(h(0.0, 0.045, 0.082), 0.009 * k, 0.009 * k),
        pt(
            h(0.0, 0.0, 0.09 + 0.008 * (p.snout - 1.0)),
            0.01 * k,
            0.01 * k,
        ),
    ]);
    m.ells.push(ell(
        h(0.0, -0.014, 0.07),
        k * v(0.018, 0.013, 0.018),
        hb(0.02),
    ));
    // Closed mouth: one soft mass for the lips.
    m.ells.push(ell(
        h(0.0, -0.042, 0.06),
        k * v(0.026, 0.02, 0.02),
        hb(0.02),
    ));
    for (s, _) in SIDES {
        // Mandible from the angle under the ear to the chin.
        m.chain(vec![
            pt(h(0.058 * s, -0.02, -0.015), 0.017 * k, 0.017 * k),
            pt(h(0.016 * s, -0.066, 0.05), 0.015 * k, 0.015 * k),
        ]);
        m.chain(vec![
            pt(h(0.077 * s, 0.05, -0.016), 0.008 * k, 0.016 * k),
            pt(h(0.081 * s, 0.02, -0.022), 0.009 * k, 0.018 * k),
            pt(h(0.076 * s, -0.012, -0.012), 0.008 * k, 0.011 * k),
        ]);
        // Shallow sockets; the eyeballs sit nearly flush.
        m.dots.push(Dot {
            at: h(0.032 * s, 0.044, 0.096),
            r: 0.02 * k,
            subtract: true,
            blend: 0.018 * k,
        });
        m.dots.push(Dot {
            at: h(0.032 * s, 0.04, 0.066),
            r: 0.013 * k,
            subtract: false,
            blend: 0.012 * k,
        });
    }
    for (s, sfx) in SIDES {
        let n = |b: &str| format!("{b}{sfx}");
        // Shoulder joint under the acromion (biacromial 41.5), bideltoid
        // 50.9 the widest point; elbow at 0.617, wrist at 0.483 (level with
        // the crotch), fingertips at 0.373.
        let cl = m.j(&n("clavicle"), Some(chest), v(0.03 * s, 1.43 + up, 0.0));
        let sh = m.j(
            &n("shoulder"),
            Some(cl),
            v(0.175 * s * bx, 1.42 + up, -0.02),
        );
        let el = m.j(&n("elbow"), Some(sh), v(0.255 * s * bx, 1.11 + up, -0.04));
        let wr = m.j(&n("wrist"), Some(el), v(0.288 * s * bx, 0.885 + up, -0.01));
        let hd = m.j(&n("hand"), Some(wr), v(0.294 * s * bx, 0.825 + up, 0.0));
        m.chain(vec![
            pt(At::J(cl), 0.022 * b, 0.022 * b),
            pt(
                At::Off(sh, v(-0.02 * s, 0.018, 0.012)),
                0.024 * b,
                0.022 * b,
            ),
        ]);
        m.chain(vec![
            pt(
                At::Off(neck, v(0.035 * s, -0.01, -0.02)),
                0.035 * b,
                0.03 * b,
            ),
            pt(At::Off(sh, v(-0.04 * s, 0.025, -0.01)), 0.04 * b, 0.022 * b),
        ]);
        m.ells.push(ell(
            At::Off(sh, v(0.026 * s * b, -0.025, 0.0)),
            g(0.052, 0.075, 0.058),
            Some(0.035 * bx),
        ));
        // Upper arm 10 across (relaxed biceps 32 around).
        m.chain_blends.push((m.chains.len(), 0.018 * bx));
        m.chain(vec![
            pt(At::J(sh), 0.045 * b, 0.048 * b),
            pt(At::J(el), 0.034 * b, 0.036 * b),
        ]);
        m.ells.push(ell(
            At::MidOff(sh, el, 0.5, v(0.0, 0.0, 0.005)),
            g(0.045, 0.09, 0.048),
            Some(0.018 * bx),
        ));
        // Forearm 9 across, widest a third below the elbow; wrist 6.2 x 4.6
        // (palms facing the thighs: the width runs front to back).
        m.chain(vec![
            pt(At::J(el), 0.038 * b, 0.04 * b),
            pt(At::J(wr), 0.021, 0.028),
        ]);
        m.ells.push(ell(
            At::Mid(el, wr, 0.3),
            g(0.04, 0.08, 0.044),
            Some(0.03 * bx),
        ));
        m.ells.push(ell(
            At::Off(el, v(0.0, 0.0, -0.024)),
            v(0.018, 0.024, 0.016),
            Some(0.012),
        ));
        // Hand 19.3 long, 8.8 broad: palm, fingers, thumb mass in front. The
        // hand bone's envelope reaches the fingertips.
        m.rads.push((hd, 0.06));
        m.chain(vec![
            pt(At::J(wr), 0.02, 0.03),
            pt(At::J(hd), 0.016, 0.044),
            pt(At::Off(hd, v(0.002 * s, -0.05, 0.004)), 0.014, 0.042),
            pt(At::Off(hd, v(0.004 * s, -0.11, 0.008)), 0.01, 0.03),
            pt(At::Off(hd, v(0.004 * s, -0.124, 0.008)), 0.009, 0.015),
        ]);
        m.chain(vec![
            pt(At::Off(wr, v(0.002 * s, -0.03, 0.022)), 0.016, 0.016),
            pt(At::Off(hd, v(0.008 * s, -0.03, 0.05)), 0.011, 0.011),
            pt(At::Off(hd, v(0.01 * s, -0.06, 0.056)), 0.009, 0.009),
        ]);
        // Leg: hip joints 8.5 off centre, knees and ankles closer in. Thigh
        // 18.5 x 21 at the top, 13 above the knee; calf 12.5 high on the
        // shin; ankle 7.5 across the malleoli at 0.042.
        let th = m.j(&n("thigh"), Some(hip), v(0.095 * s * bx, hy, 0.0));
        let kn = m.j(&n("knee"), Some(th), v(0.08 * s * bx, 0.48 * l, 0.012));
        let an = m.j(&n("ankle"), Some(kn), v(0.075 * s * bx, 0.085, -0.03));
        let ball = m.j(&n("ball"), Some(an), v(0.085 * s * bx, 0.022, 0.11));
        let toe = m.j(&n("toe"), Some(ball), v(0.09 * s * bx, 0.02, 0.165));
        m.chain(vec![
            pt(At::J(th), 0.072 * b, 0.09 * b),
            pt(At::J(kn), 0.062 * b, 0.06 * b),
        ]);
        m.ells.push(ell(
            At::MidOff(th, kn, 0.3, v(0.014 * s, 0.0, 0.01)),
            g(0.086, 0.19, 0.1),
            Some(0.03 * bx),
        ));
        m.chain(vec![
            pt(At::J(kn), 0.048 * b, 0.05 * b),
            pt(At::J(an), 0.033, 0.035),
        ]);
        m.ells.push(ell(
            At::MidOff(kn, an, 0.3, v(0.0, 0.0, -0.015)),
            g(0.062, 0.11, 0.06),
            Some(0.03 * bx),
        ));
        m.ells.push(ell(
            At::Off(kn, v(0.0, 0.008, 0.035)),
            v(0.024, 0.028, 0.015),
            Some(0.012),
        ));
        // Foot 27.1 long, 10.2 broad, heel 7.2.
        m.ells.push(ell(
            At::Off(an, v(0.0, -0.052, -0.035)),
            v(0.034, 0.033, 0.04),
            Some(0.02),
        ));
        m.chain(vec![
            pt(At::Off(an, v(0.0, -0.045, -0.02)), 0.035, 0.032),
            pt(At::J(ball), 0.05, 0.022),
        ]);
        m.chain(vec![
            pt(At::J(ball), 0.048, 0.021),
            pt(At::Off(toe, v(0.0, 0.0, 0.006)), 0.035, 0.016),
        ]);
    }
    if p.tail > 0.0 {
        let t1 = m.j("tail_1", Some(hip), m.pos(hip) + v(0.0, -0.05, -0.1));
        let t2 = m.j(
            "tail_2",
            Some(t1),
            m.pos(t1) + p.tail * v(0.0, -0.25, -0.25),
        );
        m.chain(vec![
            pt(At::Off(hip, v(0.0, -0.03, -0.04)), 0.04, 0.04),
            pt(At::J(t1), 0.035, 0.035),
            pt(At::J(t2), 0.012, 0.012),
        ]);
    }
    m
}

/// A cat: 0.25 m at the shoulder, about 0.46 m nose to rump, tail raised.
fn quadruped(p: &P) -> Pre {
    let (k, b, l) = (p.k, p.b, p.leg);
    let mut m = Pre {
        blend: 0.02,
        ..Pre::default()
    };
    let by = 0.06 + 0.12 * l;
    let hip = m.j("hip", None, v(0.0, by + 0.005, -0.13));
    let spine = m.j("spine", Some(hip), v(0.0, by + 0.01, -0.02));
    let chest = m.j("chest", Some(spine), v(0.0, by, 0.09));
    let neck = m.j("neck", Some(chest), v(0.0, by + 0.05, 0.155));
    let head = m.j("head", Some(neck), m.pos(neck) + k * v(0.0, 0.042, 0.045));
    let snout = m.j(
        "snout",
        Some(head),
        m.pos(head) + k * v(0.0, -0.014, 0.022 + 0.018 * p.snout),
    );
    m.chain(vec![
        pt(At::Off(hip, v(0.0, -0.005, -0.045)), 0.044 * b, 0.048 * b),
        pt(At::J(hip), 0.056 * b, 0.062 * b),
        pt(At::J(spine), 0.054 * b, 0.068 * b),
        pt(At::J(chest), 0.058 * b, 0.07 * b),
        // Tapers away inside the neck's root, so its cap's rim isn't on
        // the surface.
        pt(At::Off(chest, v(0.0, 0.0, 0.035)), 0.04 * b, 0.046 * b),
    ]);
    // Neck: its own chain out of the chest, melted in by the blend. In the
    // torso chain the turn up at the chest bent tighter than the chest's
    // radius, so the tube folded over itself there: a crease across the
    // withers. Withers: an even taper from the chest into the neck.
    m.chain(vec![
        // Rooted deep in the chest: a cap's rim at the surface shades as a
        // ring (the cap's curvature starts there).
        pt(At::Off(chest, v(0.0, -0.02, -0.03)), 0.042 * b, 0.048 * b),
        pt(At::Mid(chest, neck, 0.45), 0.048 * b, 0.056 * b),
        pt(At::J(neck), 0.034 * b, 0.037 * b),
        pt(At::Off(head, k * v(0.0, -0.01, -0.01)), 0.03 * k, 0.03 * k),
    ]);
    // Skull: round cranium, short muzzle.
    m.chain(vec![
        pt(
            At::Off(head, k * v(0.0, 0.004, -0.022)),
            0.036 * k,
            0.034 * k,
        ),
        pt(At::J(head), 0.042 * k, 0.038 * k),
        pt(At::J(snout), 0.02 * k, 0.017 * k),
    ]);
    for (s, sfx) in SIDES {
        let n = |b: &str| format!("{b}{sfx}");
        let ear = m.j(
            &n("ear"),
            Some(head),
            m.pos(head) + k * v(0.024 * s, 0.045, -0.01),
        );
        m.chain(vec![
            pt(
                At::Off(ear, k * v(-0.004 * s, -0.03, 0.004)),
                0.017 * k,
                0.007 * k,
            ),
            pt(At::J(ear), 0.003 * k, 0.002 * k),
        ]);
        let sh = m.j(
            &n("shoulder"),
            Some(chest),
            v(0.036 * s * b, by + 0.005, 0.085),
        );
        let el = m.j(
            &n("elbow"),
            Some(sh),
            v(0.04 * s * b, 0.025 + 0.085 * l, 0.068),
        );
        let wr = m.j(&n("wrist"), Some(el), v(0.038 * s * b, 0.035, 0.088));
        let paw = m.j(&n("paw"), Some(wr), v(0.038 * s * b, 0.013, 0.104));
        m.chain(vec![
            // The root no wider than the shoulder: a bulb there showed as
            // a rim across the shoulder.
            pt(At::Off(sh, v(0.0, 0.008, 0.0)), 0.026 * b, 0.034 * b),
            pt(At::J(sh), 0.026 * b, 0.034 * b),
            pt(At::J(el), 0.018 * b, 0.021 * b),
            pt(At::J(wr), 0.012, 0.014),
            pt(At::J(paw), 0.016, 0.019),
        ]);
        let th = m.j(&n("thigh"), Some(hip), v(0.04 * s * b, by - 0.005, -0.14));
        let kn = m.j(
            &n("knee"),
            Some(th),
            v(0.046 * s * b, 0.02 + 0.09 * l, -0.105),
        );
        let hk = m.j(
            &n("hock"),
            Some(kn),
            v(0.04 * s * b, 0.01 + 0.045 * l, -0.178),
        );
        let ft = m.j(&n("foot"), Some(hk), v(0.04 * s * b, 0.013, -0.158));
        m.chain(vec![
            pt(At::Off(th, v(0.0, 0.015, 0.0)), 0.034 * b, 0.05 * b),
            pt(At::J(kn), 0.021 * b, 0.025 * b),
            pt(At::J(hk), 0.011, 0.013),
            pt(At::J(ft), 0.015, 0.019),
        ]);
    }
    if p.tail > 0.0 {
        let t = p.tail;
        let t1 = m.j("tail_1", Some(hip), m.pos(hip) + v(0.0, 0.02, -0.06));
        let t2 = m.j("tail_2", Some(t1), m.pos(t1) + t * v(0.0, 0.02, -0.1));
        let t3 = m.j("tail_3", Some(t2), m.pos(t2) + t * v(0.0, 0.07, -0.07));
        let t4 = m.j("tail_4", Some(t3), m.pos(t3) + t * v(0.0, 0.07, 0.0));
        m.chain(vec![
            pt(At::Off(hip, v(0.0, 0.012, -0.03)), 0.017, 0.017),
            pt(At::J(t1), 0.014, 0.014),
            pt(At::J(t2), 0.012, 0.012),
            pt(At::J(t3), 0.01, 0.01),
            pt(At::J(t4), 0.008, 0.008),
        ]);
    }
    m
}

/// A small perching bird (pigeon-like), 0.25 m tall: folded wings, a flat
/// tail, thin legs and toes.
fn bird(p: &P) -> Pre {
    let (k, b, l) = (p.k, p.b, p.leg);
    let mut m = Pre {
        blend: 0.008,
        ..Pre::default()
    };
    let by = 0.1 + 0.03 * l;
    let hip = m.j("hip", None, v(0.0, by, -0.03));
    let chest = m.j("chest", Some(hip), v(0.0, by + 0.022, 0.035));
    let neck = m.j("neck", Some(chest), v(0.0, by + 0.075, 0.062));
    let head = m.j("head", Some(neck), m.pos(neck) + k * v(0.0, 0.04, 0.015));
    let beak = m.j(
        "beak",
        Some(head),
        m.pos(head) + k * v(0.0, -0.008, 0.02 + 0.022 * p.snout),
    );
    let tl = p.tail.max(0.05);
    let t1 = m.j("tail_1", Some(hip), m.pos(hip) + v(0.0, -0.012, -0.06));
    let t2 = m.j("tail_2", Some(t1), m.pos(t1) + tl * v(0.0, -0.022, -0.085));
    m.chain(vec![
        pt(At::J(t2), 0.03, 0.004),
        pt(At::J(t1), 0.024 * b, 0.012 * b),
        pt(At::J(hip), 0.044 * b, 0.045 * b),
        pt(At::J(chest), 0.047 * b, 0.05 * b),
        pt(At::J(neck), 0.021 * b, 0.022 * b),
        pt(At::Mid(neck, head, 0.6), 0.017 * k, 0.017 * k),
    ]);
    m.chain(vec![
        pt(At::J(head), 0.02 * k, 0.019 * k),
        pt(At::Off(head, k * v(0.0, -0.006, 0.02)), 0.01 * k, 0.009 * k),
        pt(At::J(beak), 0.003 * k, 0.0025 * k),
    ]);
    m.chain(vec![pt(
        At::Off(head, k * v(0.0, 0.0, -0.004)),
        0.024 * k,
        0.024 * k,
    )]);
    for (s, sfx) in SIDES {
        let n = |b: &str| format!("{b}{sfx}");
        let w = m.j(&n("wing"), Some(chest), v(0.036 * s * b, by + 0.032, 0.03));
        let wt = m.j(&n("wingtip"), Some(w), v(0.03 * s * b, by - 0.008, -0.125));
        m.chain(vec![
            pt(At::J(w), 0.013 * b, 0.026),
            pt(At::Mid(w, wt, 0.35), 0.012 * b, 0.032),
            pt(At::J(wt), 0.003, 0.008),
        ]);
        let lg = m.j(&n("leg"), Some(hip), v(0.02 * s * b, by - 0.025, 0.0));
        let an = m.j(
            &n("ankle"),
            Some(lg),
            v(0.022 * s, 0.012 + 0.025 * l, 0.006),
        );
        let toe = m.j(&n("toe"), Some(an), v(0.024 * s, 0.003, 0.042));
        let back = m.j(&n("toe_back"), Some(an), v(0.022 * s, 0.003, -0.02));
        m.chain(vec![
            pt(At::J(lg), 0.013, 0.016),
            pt(At::Mid(lg, an, 0.4), 0.007, 0.008),
            pt(At::J(an), 0.0035, 0.0035),
        ]);
        for t in [toe, back] {
            m.chain(vec![
                pt(At::J(an), 0.0035, 0.0035),
                pt(At::J(t), 0.0025, 0.0022),
            ]);
        }
        let side = m.j(&n("toe_side"), Some(an), v(0.04 * s, 0.003, 0.03));
        m.chain(vec![
            pt(At::J(an), 0.003, 0.003),
            pt(At::J(side), 0.0022, 0.002),
        ]);
    }
    m
}

/// A 0.4 m fish resting upright: a tall, narrow spindle, forked tail,
/// dorsal and pectoral fins.
fn fish(p: &P) -> Pre {
    let (k, b) = (p.k, p.b);
    let mut m = Pre {
        blend: 0.01,
        ..Pre::default()
    };
    let body = m.j("body", None, v(0.0, 0.075, 0.03));
    let head = m.j("head", Some(body), m.pos(body) + k * v(0.0, 0.0, 0.09));
    let snout = m.j(
        "snout",
        Some(head),
        m.pos(head) + k * v(0.0, -0.005, 0.035 + 0.03 * p.snout),
    );
    let t = p.tail.max(0.05);
    let t1 = m.j("tail_1", Some(body), m.pos(body) + t * v(0.0, 0.0, -0.1));
    let t2 = m.j("tail_2", Some(t1), m.pos(t1) + t * v(0.0, 0.0, -0.08));
    m.chain(vec![
        pt(At::J(snout), 0.009 * k, 0.011 * k),
        pt(At::J(head), 0.031 * b, 0.047 * b),
        pt(At::J(body), 0.04 * b, 0.062 * b),
        pt(At::J(t1), 0.025 * b, 0.04 * b),
        pt(At::J(t2), 0.007, 0.014),
    ]);
    let fin = m.j("tail_fin", Some(t2), m.pos(t2) + t * v(0.0, 0.0, -0.05));
    for dy in [0.07f32, -0.062] {
        m.chain(vec![
            pt(At::J(t2), 0.0045, 0.012),
            pt(At::Mid(t2, fin, 0.6), 0.0038, 0.02),
            pt(At::Off(fin, t * v(0.0, dy, -0.025)), 0.003, 0.01),
        ]);
    }
    let dorsal = m.j("dorsal", Some(body), m.pos(body) + v(0.0, 0.07, -0.02));
    m.chain(vec![
        pt(At::Off(body, v(0.0, 0.05, 0.04)), 0.0035, 0.01),
        pt(At::J(dorsal), 0.0032, 0.016),
        pt(At::Off(body, v(0.0, 0.05, -0.075)), 0.003, 0.006),
    ]);
    let anal = m.j("anal_fin", Some(body), m.pos(body) + v(0.0, -0.055, -0.085));
    m.chain(vec![
        pt(At::Off(body, v(0.0, -0.045, -0.055)), 0.0025, 0.008),
        pt(At::J(anal), 0.002, 0.008),
    ]);
    for (s, sfx) in SIDES {
        let f = m.j(
            &format!("fin{sfx}"),
            Some(head),
            m.pos(head) + v(0.026 * s * b, -0.03, -0.035),
        );
        m.chain(vec![
            pt(At::Off(f, v(-0.008 * s, 0.006, 0.01)), 0.009, 0.003),
            pt(At::J(f), 0.011, 0.0025),
            pt(At::Off(f, v(0.022 * s, -0.012, -0.035)), 0.006, 0.0018),
        ]);
    }
    m
}

/// A 2 m snake lying in an S on the ground, wider than tall, tapering to
/// the tail; a flat, wedge head.
fn serpent(p: &P) -> Pre {
    let (k, b) = (p.k, p.b);
    let mut m = Pre {
        blend: 0.01,
        ..Pre::default()
    };
    let n = 8usize;
    let x = |z: f32| 0.2 * (z * 3.4).sin();
    let mut prev: Option<usize> = None;
    let mut body = Vec::new();
    // Head first (the root), then down the body.
    let head = m.j("head", None, v(x(0.95), 0.025, 0.95));
    let snout = m.j(
        "snout",
        Some(head),
        m.pos(head) + k * v(0.0, -0.002, 0.03 + 0.02 * p.snout),
    );
    let neck = m.j("neck", Some(head), v(x(0.86), 0.022, 0.86));
    prev.replace(neck);
    for i in 0..n {
        let z = 0.74 - 0.2 * i as f32;
        let j = m.j(&format!("spine_{}", i + 1), prev, v(x(z), 0.03, z));
        body.push(j);
        prev = Some(j);
    }
    let t = p.tail.max(0.05);
    let mut tails = Vec::new();
    let mut z = 0.74 - 0.2 * (n - 1) as f32;
    for i in 0..3 {
        z -= 0.17 * t;
        let j = m.j(&format!("tail_{}", i + 1), prev, v(x(z) * 0.8, 0.02, z));
        tails.push(j);
        prev = Some(j);
    }
    let kh = k;
    let mut c = vec![
        pt(At::J(snout), 0.02 * kh, 0.012 * kh),
        pt(At::J(head), 0.042 * kh, 0.024 * kh),
        pt(At::J(neck), 0.026 * b, 0.021 * b),
    ];
    for (i, &j) in body.iter().enumerate() {
        let f = 1.0 - 0.25 * (i as f32 / (n - 1) as f32 - 0.35).abs();
        c.push(pt(At::J(j), 0.046 * b * f, 0.034 * b * f));
    }
    for (i, &j) in tails.iter().enumerate() {
        let f = [0.6, 0.35, 0.12][i];
        c.push(pt(At::J(j), 0.04 * b * f, 0.03 * b * f));
    }
    m.chain(c);
    m
}

/// A 4.3 m hatchback-ish car: rounded body, cabin, four wheels in arches.
/// build = width, leg_length = wheel size, snout = front overhang, tail =
/// rear overhang, head_ratio = cabin height.
fn car(p: &P) -> Pre {
    let (bw, kh) = (p.b, p.k);
    let mut m = Pre {
        blend: 0.07,
        floor: Some(0.26),
        ..Pre::default()
    };
    let wr = 0.33 * p.leg;
    let zf = 1.35 + 0.82 * p.snout;
    let zr = -1.35 - 0.72 * p.tail;
    let body = m.j("body", None, v(0.0, 0.62, 0.0));
    let cabin = m.j("cabin", Some(body), v(0.0, 1.12, -0.25));
    // Body: one superellipse run (n 5: flat flanks and top, rounded
    // corners), softer (n 3) at the bumpers.
    let o = |x: f32, y: f32, z: f32| At::Off(body, v(x, y, z));
    m.chain(vec![
        ptn(o(0.0, -0.02, zf), 0.8 * bw, 0.26, 3.0),
        ptn(o(0.0, 0.02, zf - 0.4), 0.88 * bw, 0.37, 4.5),
        ptn(o(0.0, 0.05, 0.4), 0.9 * bw, 0.42, 5.0),
        ptn(o(0.0, 0.06, -0.9), 0.9 * bw, 0.42, 5.0),
        ptn(o(0.0, 0.07, zr + 0.35), 0.89 * bw, 0.4, 4.5),
        ptn(o(0.0, 0.07, zr), 0.82 * bw, 0.3, 3.0),
    ]);
    // Cabin: rounder (n 2.6), narrower than the body (tumblehome).
    let ch = 0.26 * kh;
    let c = |y: f32, z: f32| At::Off(cabin, v(0.0, y, z));
    m.chain(vec![
        ptn(c(-0.2, 1.05), 0.7 * bw, 0.05, 2.6),
        ptn(c(0.02, 0.45), 0.74 * bw, ch, 2.6),
        ptn(c(0.04, -0.35), 0.74 * bw, ch, 2.6),
        ptn(c(-0.16, -1.15), 0.7 * bw, 0.06, 2.6),
    ]);
    for (s, sfx) in SIDES {
        for (zz, w) in [(1.35, "wheel_front"), (-1.35, "wheel_back")] {
            let j = m.j(&format!("{w}{sfx}"), Some(body), v(0.72 * s * bw, wr, zz));
            m.wheels.push(Wheel { j, r: wr, w: 0.23 });
        }
    }
    m
}

/// Local sculpt warps on a body are symmetric about x = 0 by default (as
/// under an sdf `mirror`): each is followed by its x-reflection (centre,
/// direction and normal), unless it says `mirror: false`, sits on the
/// centre line, or its centre is still an unresolved anchor.
pub(crate) fn mirror_local_warps(warps: Vec<crate::deform::Deform>) -> Vec<crate::deform::Deform> {
    let mut out = Vec::with_capacity(warps.len());
    for w in warps {
        let twin = match &w.at {
            Some(crate::deform::WarpAt::Point(p))
                if crate::deform::LOCAL_MODES.contains(&w.mode.as_str())
                    && w.mirror != Some(false)
                    && p[0].abs() > 1e-4 =>
            {
                let mut t = w.clone();
                t.at = Some(crate::deform::WarpAt::Point([-p[0], p[1], p[2]]));
                if let Some(d) = &mut t.direction {
                    d[0] = -d[0];
                }
                if let Some(n) = &mut t.normal {
                    n[0] = -n[0];
                }
                Some(t)
            }
            _ => None,
        };
        out.push(w);
        out.extend(twin);
    }
    out
}

/// Lowers a body item to an sdf and its joints.
pub fn lower(def: &BodyDef) -> Result<Lowered, String> {
    if def.preset == "human" {
        return Err("the human preset is a template, not a field (no sdf lowering)".into());
    }
    let (h0, _, hr0) = defaults(&def.preset);
    let p = P {
        k: def.head_ratio.map_or(1.0, |h| h / hr0),
        b: def.build.unwrap_or(1.0),
        leg: def.leg_length.unwrap_or(1.0),
        tail: def
            .tail
            .unwrap_or(if def.preset == "biped" { 0.0 } else { 1.0 }),
        snout: def.snout.unwrap_or(1.0),
    };
    let mut m = match def.preset.as_str() {
        "biped" => biped(&p),
        "quadruped" => quadruped(&p),
        "bird" => bird(&p),
        "fish" => fish(&p),
        "serpent" => serpent(&p),
        "car" => car(&p),
        o => return Err(format!("preset: no preset `{o}`")),
    };
    // Ground, then scale so the preset's measure is `height`.
    let g = m.extent(Vec3::Y).0;
    m.shift_y(-g);
    let measure = match def.preset.as_str() {
        "biped" | "bird" => m.extent(Vec3::Y).1,
        "quadruped" => {
            let chest = m.joints.iter().position(|j| j.0 == "chest").unwrap_or(0);
            let ry = m.chains[0]
                .iter()
                .find(|q| matches!(q.at, At::J(i) if i == chest))
                .map_or(0.07, |q| q.r[1]);
            m.pos(chest).y + ry
        }
        _ => {
            let (a, b) = m.extent(Vec3::Z);
            b - a
        }
    };
    let s = def.height.unwrap_or(h0) / measure.max(1e-6);
    m.scale(s);
    // Per-joint overrides.
    for (name, e) in &def.joints {
        let Some(i) = m.joints.iter().position(|j| &j.0 == name) else {
            let names: Vec<&str> = m.joints.iter().map(|j| j.0.as_str()).collect();
            return Err(format!(
                "joints: `{name}` is not a joint of the {} preset (joints: {})",
                def.preset,
                names.join(", ")
            ));
        };
        if let Some(o) = e.offset {
            for d in m.descendants(i) {
                m.joints[d].2 += Vec3::from(o);
            }
        }
        if let Some(k) = e.scale {
            for c in &mut m.chains {
                for q in c {
                    if matches!(q.at, At::J(j) if j == i) {
                        q.r = [q.r[0] * k, q.r[1] * k];
                    }
                }
            }
            for w in &mut m.wheels {
                if w.j == i {
                    w.r *= k;
                    w.w *= k;
                }
            }
            for e in &mut m.ells {
                if matches!(e.at, At::J(j) | At::Off(j, _) if j == i) {
                    e.r *= k;
                }
            }
            for r in &mut m.rads {
                if r.0 == i {
                    r.1 *= k;
                }
            }
        }
    }
    let g = m.extent(Vec3::Y).0;
    m.shift_y(-g);
    // The sdf.
    let blend = def.blend.unwrap_or(m.blend);
    let ell_shape = |e: &Ell| Shape {
        shape: "ellipsoid".into(),
        blend: e.blend,
        at: Some(m.at(e.at).to_array()),
        size: Some(e.r.to_array().to_vec()),
        ..Shape::default()
    };
    let mut shapes: Vec<Shape> = m.ells.iter().filter(|e| e.early).map(ell_shape).collect();
    shapes.extend(m.sects().into_iter().enumerate().map(|(i, c)| Shape {
        shape: "chain".into(),
        blend: m.chain_blends.iter().find(|b| b.0 == i).map(|b| b.1),
        points: Some(c),
        ..Shape::default()
    }));
    shapes.extend(m.ells.iter().filter(|e| !e.early).map(ell_shape));
    for d in &m.dots {
        shapes.push(Shape {
            shape: "sphere".into(),
            op: d.subtract.then(|| "subtract".into()),
            blend: Some(d.blend),
            at: Some(m.at(d.at).to_array()),
            radius: Some(vec![d.r]),
            ..Shape::default()
        });
    }
    if let Some(f) = m.floor {
        let (lo, hi) = (m.extent(Vec3::X), m.extent(Vec3::Z));
        let w = 2.0 * (hi.1 - hi.0 + lo.1 - lo.0);
        shapes.push(Shape {
            shape: "box".into(),
            op: Some("subtract".into()),
            blend: Some(0.3 * blend),
            at: Some([0.0, f - w * 0.5, 0.5 * (hi.0 + hi.1)]),
            size: Some(vec![w, w, w]),
            ..Shape::default()
        });
    }
    let x90 = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).to_array();
    for w in &m.wheels {
        let c = m.pos(w.j);
        shapes.push(Shape {
            shape: "cylinder".into(),
            op: Some("subtract".into()),
            blend: Some(0.5 * blend),
            at: Some(c.to_array()),
            rotation: Some(x90),
            radius: Some(vec![w.r * 1.18]),
            height: Some(w.w * 2.2),
            ..Shape::default()
        });
    }
    for w in &m.wheels {
        let c = m.pos(w.j);
        shapes.push(Shape {
            shape: "cylinder".into(),
            blend: Some(0.0),
            at: Some(c.to_array()),
            rotation: Some(x90),
            radius: Some(vec![w.r]),
            height: Some(w.w),
            round: Some(w.r * 0.2),
            ..Shape::default()
        });
        let out = c.x.signum() * 0.5 * w.w;
        shapes.push(Shape {
            shape: "cylinder".into(),
            op: Some("subtract".into()),
            blend: Some(w.r * 0.04),
            at: Some([c.x + out, c.y, c.z]),
            rotation: Some(x90),
            radius: Some(vec![w.r * 0.62]),
            // Half of it cuts in: about 0.07 m on a default car, over two
            // voxels, so the dish meshes with a clean rim and floor (a
            // recess a voxel deep can't).
            height: Some(w.w * 0.6),
            ..Shape::default()
        });
    }
    // Default voxel: 1/135 of the largest extent (thin tips such as ears
    // and fin edges would otherwise drive it to the sample limit), coarser
    // when only a light decimated mesh is kept.
    let (ex, ey, ez) = (m.extent(Vec3::X), m.extent(Vec3::Y), m.extent(Vec3::Z));
    let e = Vec3::new(ex.1 - ex.0, ey.1 - ey.0, ez.1 - ez.0);
    // The biped's face features (nose, lips, eyes) need about 1/160.
    let div = if def.preset == "biped" { 160.0 } else { 135.0 };
    let mut res = e.max_element() / div;
    if let Some(t) = def.target_tris.or(def.tris.filter(|_| !def.bake)) {
        let area = 0.55 * 2.0 * (e.x * e.y + e.y * e.z + e.z * e.x);
        let want = (40.0 * t as f32).max(6000.0);
        res = res.max((2.2 * area / want).sqrt().min(e.max_element() / 60.0));
    }
    // The sdf evaluates each shape only within a band of the item's blend
    // around it, so a shape blended wider than the item would show a seam
    // where the band cuts it off: give every shape its blend explicitly and
    // the item the widest.
    let mut widest = blend;
    for sh in &mut shapes {
        let k = *sh.blend.get_or_insert(blend);
        widest = widest.max(k);
    }
    let sdf = SdfDef {
        shapes,
        blend: Some(widest),
        resolution: Some(def.resolution.unwrap_or(res)),
        tris: def.tris,
        bake: def.bake,
        target_tris: def.target_tris,
        warps: mirror_local_warps(crate::sdf::resolve_joint_anchors(&def.warps, |n| {
            m.joints.iter().find(|j| j.0 == n).map(|j| j.2.to_array())
        })),
        ..SdfDef::default()
    };
    // Joint radii: the largest section at the joint.
    let joints = m
        .joints
        .iter()
        .enumerate()
        .map(|(i, (n, par, at))| {
            let mut r = 0.0f32;
            for c in &m.chains {
                for q in c {
                    if matches!(q.at, At::J(j) if j == i) {
                        r = r.max(0.5 * (q.r[0] + q.r[1]));
                    }
                }
            }
            for w in &m.wheels {
                if w.j == i {
                    r = r.max(w.r);
                }
            }
            for x in &m.rads {
                if x.0 == i {
                    r = r.max(x.1);
                }
            }
            Joint {
                name: n.clone(),
                parent: par.map(|p| m.joints[p].0.clone()),
                at: *at,
                radius: if r > 0.0 { r } else { 0.02 * s },
            }
        })
        .collect();
    Ok(Lowered { sdf, joints })
}

/// The automatic bind's blend: the median bone radius (the bind default)
/// is set by paws, ears and toes and leaves the torso's roots (tail, legs,
/// neck) with a weight step a few edges wide, which tears when the tail
/// wags. Blend over the root joint's radius instead (at least the median),
/// so ±35 degrees at a chain root stays smooth, also on a decimated mesh.
pub fn bind_blend(def: &BodyDef) -> Option<f32> {
    if def.preset == "human" {
        return None;
    }
    let low = lower(def).ok()?;
    let root = low.joints.iter().find(|j| j.parent.is_none())?;
    let mut r: Vec<f32> = low.joints.iter().map(|j| j.radius).collect();
    r.sort_by(f32::total_cmp);
    // The biped blends wider: its armpits and crotch are narrow webs
    // between limbs that swing apart.
    let k = if def.preset == "biped" { 1.5 } else { 1.0 };
    Some(k * root.radius.max(r[r.len() / 2]))
}

/// Meshes a `body` item: the solid with its joints attached (anchors).
pub fn build(
    name: &str,
    def: &BodyDef,
    lookup: &crate::sdf::Lookup,
) -> Result<crate::Part, crate::ModelerError> {
    def.validate()
        .map_err(|e| crate::ModelerError::Invalid(format!("body `{name}`: {e}")))?;
    let low =
        lower(def).map_err(|e| crate::ModelerError::Invalid(format!("body `{name}`: {e}")))?;
    let mut part = crate::sdf::build(name, &low.sdf, lookup).map_err(|e| match e {
        crate::ModelerError::Csg(m) => crate::ModelerError::Csg(m.replacen("sdf `", "body `", 1)),
        o => o,
    })?;
    part.joints = Some(std::sync::Arc::new(
        low.joints
            .iter()
            .map(|j| (j.name.clone(), j.at.to_array()))
            .collect(),
    ));
    Ok(part)
}

/// The joint names of a preset (for docs and errors).
pub fn joint_names(preset: &str) -> Vec<String> {
    let def = BodyDef {
        preset: preset.into(),
        ..BodyDef::default()
    };
    rig_joints(&def)
        .map(|l| l.into_iter().map(|j| j.name).collect())
        .unwrap_or_default()
}

/// The joints of a body item (its skeleton and anchors), any preset.
pub fn rig_joints(def: &BodyDef) -> Result<Vec<Joint>, String> {
    if def.preset == "human" {
        return crate::human::joints(def);
    }
    lower(def).map(|l| l.joints)
}

/// The skeleton bones a body item makes: (name, parent, head, tail,
/// radius). The human template gives real tails; the sdf presets leave
/// them to the children.
pub fn rig_bones(def: &BodyDef) -> Result<Vec<crate::anim::auto::RawBone>, String> {
    if def.preset == "human" {
        return crate::human::rig_raw(def);
    }
    Ok(lower(def)?
        .joints
        .iter()
        .map(|j| {
            (
                j.name.clone(),
                j.parent.clone(),
                j.at,
                None,
                Some(j.radius.max(1e-3)),
            )
        })
        .collect())
}

/// Every part a body item makes (the human template makes extras).
pub fn build_all(
    name: &str,
    def: &BodyDef,
    lookup: &crate::sdf::Lookup,
) -> Result<Vec<crate::Part>, crate::ModelerError> {
    if def.preset == "human" {
        def.validate()
            .map_err(|e| crate::ModelerError::Invalid(format!("body `{name}`: {e}")))?;
        return crate::human::build_with(name, def, Some(lookup))
            .map_err(|e| crate::ModelerError::Invalid(format!("body `{name}`: {e}")));
    }
    build(name, def, lookup).map(|p| vec![p])
}

/// The part names a body item writes.
pub fn outputs(name: &str, def: &BodyDef) -> Vec<String> {
    if def.preset == "human" {
        return crate::human::outputs(name, def);
    }
    vec![name.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(preset: &str) -> BodyDef {
        BodyDef {
            preset: preset.into(),
            ..BodyDef::default()
        }
    }

    /// Width (x) and depth (z) of the surface in a thin slab at height
    /// `y`, over the connected run of vertices around `x0` (or the whole
    /// slab when `x0` is `None`).
    fn slab(part: &crate::Part, y: f32, x0: Option<f32>, half: bool) -> (f32, f32) {
        // Where the triangle edges cross the plane.
        let mut pts: Vec<Vec3> = Vec::new();
        for t in part.indices.as_chunks::<3>().0 {
            let w = t.map(|i| part.world_vert(i as usize));
            for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
                if (a.y - y) * (b.y - y) < 0.0 {
                    pts.push(a.lerp(b, (y - a.y) / (b.y - a.y)));
                }
            }
        }
        if let Some(x0) = x0 {
            // The loop nearest x0: points chained within 1.5 cm.
            let Some(c) = (0..pts.len())
                .min_by(|&a, &b| (pts[a].x - x0).abs().total_cmp(&(pts[b].x - x0).abs()))
            else {
                return (0.0, 0.0);
            };
            let mut keep = vec![false; pts.len()];
            keep[c] = true;
            let mut stack = vec![c];
            while let Some(i) = stack.pop() {
                for j in 0..pts.len() {
                    if !keep[j] && pts[i].distance(pts[j]) < 0.015 {
                        keep[j] = true;
                        stack.push(j);
                    }
                }
            }
            pts = pts
                .iter()
                .zip(&keep)
                .filter(|k| *k.1)
                .map(|k| *k.0)
                .collect();
        }
        if half {
            pts.retain(|p| p.x > 0.0);
        }
        let (mut a, mut b) = (Vec3::MAX, Vec3::MIN);
        for p in &pts {
            a = a.min(*p);
            b = b.max(*p);
        }
        (b.x - a.x, b.z - a.z)
    }

    /// The default biped measured against the ANSUR II male medians (2012
    /// US Army Anthropometric Survey, NATICK/TR-15/007; stature 175.5 cm),
    /// with a leaner waist for a mannequin. Fractions of stature.
    #[test]
    fn the_default_biped_matches_anthropometric_medians() {
        let part = build("guy", &def("biped"), &|n: &str| {
            Err(crate::ModelerError::Invalid(n.into()))
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let verts: Vec<Vec3> = (0..part.verts.len()).map(|i| part.world_vert(i)).collect();
        let (lo, hi) = part.bounds();
        let st = hi.y - lo.y;
        let f = |x: f32| x * st / 1.755;
        let mut fails = Vec::new();
        let mut table = String::from("landmark | target w x d | model w x d | error %\n");
        // The torso and arms are measured with the arms moved out (they
        // hang against the torso); a thigh is the half of the pair at x > 0
        // (the thighs touch at the top).
        let mut spread_def = def("biped");
        for (j, x) in [("shoulder_l", 0.3), ("shoulder_r", -0.3)] {
            spread_def.joints.insert(
                j.into(),
                JointEdit {
                    offset: Some([x, 0.0, 0.0]),
                    ..JointEdit::default()
                },
            );
        }
        let spread = build("guy", &spread_def, &|n: &str| {
            Err(crate::ModelerError::Invalid(n.into()))
        })
        .unwrap_or_else(|e| panic!("{e}"));
        // (name, height, seed x, target width, target depth, tolerance,
        // arms out, half)
        type Row = (&'static str, f32, Option<f32>, f32, f32, f32, bool, bool);
        let rows: [Row; 12] = [
            (
                "head (breadth, length)",
                1.664,
                Some(0.0),
                0.154,
                0.2,
                0.04,
                false,
                false,
            ),
            ("neck", 1.505, Some(0.0), 0.126, 0.126, 0.06, false, false),
            ("bideltoid", 1.39, None, 0.509, 0.0, 0.04, false, false),
            (
                "chest at nipples",
                1.289,
                Some(0.0),
                0.3,
                0.253,
                0.04,
                true,
                false,
            ),
            (
                "waist at navel",
                1.055,
                Some(0.0),
                0.295,
                0.21,
                0.04,
                true,
                false,
            ),
            (
                "hips at buttocks",
                0.885,
                Some(0.0),
                0.344,
                0.246,
                0.04,
                true,
                false,
            ),
            ("thigh top", 0.8, Some(0.09), 0.185, 0.21, 0.06, false, true),
            (
                "lower thigh",
                0.56,
                Some(0.085),
                0.13,
                0.13,
                0.06,
                false,
                true,
            ),
            ("calf", 0.36, Some(0.078), 0.125, 0.125, 0.06, false, true),
            ("upper arm", 1.26, Some(0.526), 0.1, 0.1, 0.06, true, false),
            ("forearm", 1.03, Some(0.57), 0.09, 0.09, 0.06, true, false),
            ("wrist", 0.905, Some(0.587), 0.046, 0.062, 0.08, true, false),
        ];
        for (name, y, x0, w, d, tol, arms_out, half) in rows {
            let src = if arms_out { &spread } else { &part };
            let (mw, md) = slab(src, f(y), x0.map(f), half);
            let (ew, ed) = (mw / f(w) - 1.0, if d > 0.0 { md / f(d) - 1.0 } else { 0.0 });
            table += &format!(
                "{name} | {w:.3} x {d:.3} | {:.3} x {:.3} | {:+.1} / {:+.1}\n",
                mw * 1.755 / st,
                md * 1.755 / st,
                ew * 100.0,
                ed * 100.0
            );
            if ew.abs() > tol || ed.abs() > tol {
                fails.push(name);
            }
        }
        // Landmark heights.
        let low = |pred: &dyn Fn(&Vec3) -> bool| {
            verts
                .iter()
                .filter(|p| pred(p))
                .map(|p| p.y)
                .fold(f32::MAX, f32::min)
        };
        let chin = low(&|p| p.x.abs() < 0.01 && p.z > f(0.05) && p.y > f(1.45));
        let crotch = low(&|p| p.x.abs() < 0.012 && p.y > f(0.5) && p.y < f(1.0));
        let tip = low(&|p| p.x > f(0.24) && p.y > f(0.5));
        let wide = verts
            .iter()
            .filter(|p| p.y > f(1.33))
            .max_by(|a, b| a.x.total_cmp(&b.x))
            .map_or(0.0, |p| p.y);
        // The fingertips get 2.5%: the medians' span (1.032) and fingertip
        // height can't both hold for an arm hanging in the preset's slight
        // A-pose (kept off the thighs for the rig).
        for (name, got, want, tol) in [
            ("chin", chin, 1.523, 0.015),
            ("crotch", crotch, 0.845, 0.015),
            ("fingertip", tip, 0.654, 0.025),
            ("widest shoulder", wide, 1.39, 0.015),
        ] {
            let e = (got - f(want)) / st;
            table += &format!(
                "{name} height | {want:.3} | {:.3} | {:+.1} (of stature)\n",
                got * 1.755 / st,
                e * 100.0
            );
            if e.abs() > tol {
                fails.push(name);
            }
        }
        eprintln!("{table}");
        assert!(fails.is_empty(), "off the medians: {fails:?}\n{table}");
    }

    #[test]
    fn every_preset_is_closed_grounded_and_sized() {
        // The human template has its own tests (src/human/tests.rs) and
        // needs its data file.
        for &p in PRESETS.iter().filter(|p| **p != "human") {
            let d = def(p);
            let t0 = std::time::Instant::now();
            let part = build(p, &d, &|n: &str| {
                Err(crate::ModelerError::Invalid(n.into()))
            })
            .unwrap_or_else(|e| panic!("{p}: {e}"));
            eprintln!("{p}: {:?}", t0.elapsed());
            assert_eq!(
                crate::verify::open_edge_count(&part),
                0,
                "{p} has open edges"
            );
            let (lo, hi) = part.bounds();
            let (h, what, _) = defaults(p);
            assert!(lo.y.abs() < 0.01 * h.max(0.3), "{p}: lowest point {lo}");
            let got = match what {
                "length" => hi.z - lo.z,
                "standing height" => hi.y,
                _ => h,
            };
            assert!((got - h).abs() < 0.04 * h, "{p}: {what} {got} vs {h}");
            eprintln!("{p}: {lo} .. {hi}, {} tris", part.tri_count());
        }
    }

    /// Highest point of the vertices on one side (|x| > 0.1) of a biped
    /// built with `warps`, and the widest reach on that side.
    fn side_ext(warps: serde_json::Value, sign: f32) -> (f32, f32) {
        let mut d = def("biped");
        d.warps = serde_json::from_value(warps).unwrap_or_else(|e| panic!("{e}"));
        let part = build("g", &d, &|n: &str| {
            Err(crate::ModelerError::Invalid(n.into()))
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let (mut y, mut x) = (f32::MIN, 0.0f32);
        for v in &part.verts {
            if v[0] * sign > 0.1 && v[1] > 1.2 {
                y = y.max(v[1]);
                x = x.max(v[0].abs());
            }
        }
        (y, x)
    }

    #[test]
    fn local_warps_on_a_body_are_symmetric_unless_told_not_to() {
        use serde_json::json;
        let w = |m: serde_json::Value| {
            let mut w = json!({"mode": "inflate", "at": "shoulder_l", "radius": 0.15,
                               "amount": 0.05});
            if !m.is_null() {
                w["mirror"] = m;
            }
            json!([w])
        };
        let (y0l, x0l) = side_ext(json!([]), 1.0);
        let (y0r, x0r) = side_ext(json!([]), -1.0);
        assert!((y0l - y0r).abs() < 2e-3 && (x0l - x0r).abs() < 2e-3);
        // Default: both shoulders grow, equally.
        let (yl, xl) = side_ext(w(json!(null)), 1.0);
        let (yr, xr) = side_ext(w(json!(null)), -1.0);
        assert!(
            yl - y0l > 0.015 || xl - x0l > 0.015,
            "left {yl} {xl} from {y0l} {x0l}"
        );
        assert!(
            (yl - yr).abs() < 2e-3 && (xl - xr).abs() < 2e-3,
            "{yl} {yr} {xl} {xr}"
        );
        // mirror: false keeps it on the left.
        let (yl, xl) = side_ext(w(json!(false)), 1.0);
        let (yr, xr) = side_ext(w(json!(false)), -1.0);
        assert!(
            yl - y0l > 0.015 || xl - x0l > 0.015,
            "left {yl} {xl} from {y0l} {x0l}"
        );
        assert!(
            (yr - y0r).abs() < 2e-3 && (xr - x0r).abs() < 2e-3,
            "{yr} {xr}"
        );
    }

    #[test]
    fn a_centre_line_warp_is_applied_once() {
        let mk = |at: &str| -> Vec<crate::deform::Deform> {
            serde_json::from_value(serde_json::json!([{"mode": "inflate", "at": at,
                "radius": 0.2, "amount": 0.03}]))
            .unwrap_or_else(|e| panic!("{e}"))
        };
        let mut d = def("biped");
        d.warps = mk("chest");
        let n = lower(&d).unwrap_or_else(|e| panic!("{e}")).sdf.warps.len();
        assert_eq!(n, 1, "a centre-line warp must not be doubled");
        d.warps = mk("shoulder_l");
        let low = lower(&d).unwrap_or_else(|e| panic!("{e}")).sdf.warps;
        assert_eq!(low.len(), 2);
        let (Some(crate::deform::WarpAt::Point(a)), Some(crate::deform::WarpAt::Point(b))) =
            (&low[0].at, &low[1].at)
        else {
            panic!("anchors not resolved")
        };
        assert!((a[0] + b[0]).abs() < 1e-6 && a[1] == b[1] && a[2] == b[2]);
    }

    #[test]
    fn the_rig_has_the_joints_and_they_are_anchors() {
        let ops = vec![crate::Op::Body {
            name: "cat".into(),
            def: Box::new(def("quadruped")),
        }];
        let parts = crate::graph::evaluate(&ops).unwrap_or_else(|e| panic!("{e}"));
        let model = crate::Model {
            ops: ops.clone(),
            parts: parts.clone(),
            ..crate::Model::default()
        };
        let skels = crate::anim::skeletons(&model).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(skels.len(), 1);
        assert_eq!(skels[0].name, "cat_rig");
        let names = joint_names("quadruped");
        let bones: Vec<&str> = skels[0].bones.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(bones.len(), names.len());
        for n in &names {
            assert!(bones.contains(&n.as_str()), "no bone {n}");
        }
        let cat = parts
            .iter()
            .find(|p| p.name == "cat")
            .unwrap_or_else(|| panic!("no cat"));
        let paw = cat.joint("paw_l").unwrap_or_else(|| panic!("no paw_l"));
        assert!(paw.x > 0.0 && paw.y < 0.03, "{paw}");
        // An unknown joint override names the joints.
        let mut bad = def("quadruped");
        bad.joints.insert("wing_l".into(), JointEdit::default());
        let e = bad.validate().err().unwrap_or_default();
        assert!(e.contains("paw_l"), "{e}");
    }

    #[test]
    fn a_body_is_bound_to_its_rig_and_a_tail_wag_does_not_tear() {
        use serde_json::json;
        let mut s = crate::commands::Session::default();
        s.apply_values(
            None,
            vec![
                json!({"type": "body", "name": "cat", "preset": "quadruped", "tris": 6000}),
                json!({"type": "clip", "name": "wag", "skeleton": "cat_rig", "duration": 1.0,
                       "tracks": {"tail_1": {"rotate": [[0, {"y": 30}], [0.5, {"y": -30}], [1, {"y": 30}]]},
                                  "tail_2": {"rotate": [[0, {"y": 20}], [0.5, {"y": -20}], [1, {"y": 20}]]}}}),
                json!({"type": "primitive", "name": "mat", "kind": "box", "size": [2, 0.01, 2],
                       "translation": [0, -0.005, 0]}),
                json!({"type": "primitive", "name": "ball", "kind": "sphere", "size": 0.02,
                       "translation": [0.5, 0.5, 0.5]}),
                json!({"type": "place", "name": "ball_on", "parts": ["ball"], "on": "mat",
                       "at": "cat:paw_l"}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let cat = s
            .model
            .parts
            .iter()
            .find(|p| p.name == "cat")
            .unwrap_or_else(|| panic!("no cat"));
        assert!(
            cat.rig.as_ref().is_some_and(|b| b.bind == "cat_bind"),
            "not bound"
        );
        let paw = cat.joint("paw_l").unwrap_or_else(|| panic!("no paw_l"));
        let ball = s
            .model
            .parts
            .iter()
            .find(|p| p.name == "ball")
            .unwrap_or_else(|| panic!("no ball"));
        let (lo, hi) = ball.bounds();
        let c = (lo + hi) * 0.5;
        assert!(
            (c.x - paw.x).abs() < 1e-3 && (c.z - paw.z).abs() < 1e-3 && lo.y.abs() < 1e-3,
            "ball at {c}, paw {paw}"
        );
        let spec: crate::commands::VerifySpec = serde_json::from_value(
            json!({"weights": true, "animation": {"clip": "wag", "samples": 8, "stretch": true},
                   "scope": ["cat"]}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let checks = s.verify_checks(&spec);
        assert!(checks.iter().all(|c| c.1), "{checks:?}");
        let lints = crate::anim::check::lints(&s.model);
        assert!(lints.iter().all(|l| !l.contains("no bind")), "{lints:?}");
    }

    /// At full detail (no `tris`), the automatic bind's blend at the tail's
    /// root keeps a ±35 degree wag from tearing the edges there (it used
    /// the median bone radius, set by paws and ears: 2.6x at ±35).
    #[test]
    fn a_full_detail_tail_wags_35_degrees_without_tearing() {
        use serde_json::json;
        let mut s = crate::commands::Session::default();
        s.apply_values(
            None,
            vec![
                json!({"type": "body", "name": "cat", "preset": "quadruped"}),
                json!({"type": "clip", "name": "wag", "skeleton": "cat_rig", "duration": 1.0,
                       "tracks": {"tail_1": {"rotate": [[0, {"y": 35}], [0.5, {"y": -35}], [1, {"y": 35}]]}}}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let spec: crate::commands::VerifySpec = serde_json::from_value(
            json!({"weights": true, "animation": {"clip": "wag", "samples": 8, "stretch": true},
                   "scope": ["cat"]}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let checks = s.verify_checks(&spec);
        assert!(checks.iter().all(|c| c.1), "{checks:?}");
    }
}
