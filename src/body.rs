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

use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::sdf::{SdfDef, Sect, Shape};

fn none<T>(v: &Option<T>) -> bool {
    v.is_none()
}

pub const PRESETS: &[&str] = &["biped", "quadruped", "bird", "fish", "serpent", "car"];

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
}

/// (default height, what it measures, default head_ratio).
pub fn defaults(preset: &str) -> (f32, &'static str, f32) {
    match preset {
        "biped" => (1.75, "standing height", 1.0 / 7.5),
        "quadruped" => (0.25, "shoulder height", 0.36),
        "bird" => (0.25, "standing height", 0.2),
        "fish" => (0.4, "length", 0.25),
        "serpent" => (2.0, "length", 0.03),
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

struct Wheel {
    j: usize,
    r: f32,
    w: f32,
}

#[derive(Default)]
struct Pre {
    joints: Vec<(String, Option<usize>, Vec3)>,
    chains: Vec<Vec<Pt>>,
    dots: Vec<Dot>,
    wheels: Vec<Wheel>,
    /// Flat floor cut (car underside), y.
    floor: Option<f32>,
    blend: f32,
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
        }
    }
    fn chain(&mut self, pts: Vec<Pt>) {
        self.chains.push(pts);
    }
    fn sects(&self) -> Vec<Vec<Sect>> {
        self.chains
            .iter()
            .map(|c| {
                c.iter()
                    .map(|p| {
                        let q = self.at(p.at);
                        if p.n == 2.0 {
                            Sect::Oval(q.x, q.y, q.z, p.r)
                        } else {
                            Sect::Super(q.x, q.y, q.z, p.r, 0.0, p.n)
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
                match &mut p.at {
                    At::Off(_, d) => *d *= s,
                    At::J(_) | At::Mid(..) => {}
                }
            }
        }
        for d in &mut self.dots {
            d.r *= s;
            d.blend *= s;
            if let At::Off(_, o) = &mut d.at {
                *o *= s;
            }
        }
        for w in &mut self.wheels {
            w.r *= s;
            w.w *= s;
        }
        if let Some(f) = &mut self.floor {
            *f *= s;
        }
        self.blend *= s;
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

/// A person about 1.75 m, head 1/7.5 of the height, arms relaxed a little
/// out from the body, facing +z (left = +x).
fn biped(p: &P) -> Pre {
    let (k, b, l) = (p.k, p.b, p.leg);
    let bx = 1.0 + (b - 1.0) * 0.5;
    let mut m = Pre {
        blend: 0.024,
        ..Pre::default()
    };
    let hy = 0.93 * l;
    let up = hy - 0.93;
    let hip = m.j("hip", None, v(0.0, 0.95 + up, -0.006));
    let spine = m.j("spine", Some(hip), v(0.0, 1.10 + up, -0.012));
    let chest = m.j("chest", Some(spine), v(0.0, 1.28 + up, 0.01));
    let neck = m.j("neck", Some(chest), v(0.0, 1.475 + up, -0.022));
    let head = m.j("head", Some(neck), m.pos(neck) + k * v(0.0, 0.115, 0.027));
    let jaw = m.j(
        "jaw",
        Some(head),
        m.pos(head) + k * v(0.0, -0.058, 0.04 + 0.012 * p.snout),
    );
    // Torso: pelvis, waist, ribcage, up into the neck.
    m.chain(vec![
        pt(At::J(hip), 0.158 * b, 0.11 * b),
        pt(At::J(spine), 0.133 * b, 0.092 * b),
        pt(At::J(chest), 0.152 * b, 0.112 * b),
        pt(At::Mid(chest, neck, 0.5), 0.165 * b, 0.1 * b),
        pt(At::Mid(chest, neck, 0.78), 0.12 * b, 0.075 * b),
        pt(At::J(neck), 0.054 * bx, 0.056 * bx),
        pt(At::Off(head, k * v(0.0, -0.03, -0.01)), 0.05 * k, 0.05 * k),
    ]);
    // Skull: chin up to the crown; jaw and nose forward.
    m.chain(vec![
        pt(
            At::Off(head, k * v(0.0, -0.045, 0.004)),
            0.058 * k,
            0.07 * k,
        ),
        pt(At::J(head), 0.074 * k, 0.09 * k),
        pt(At::Off(head, k * v(0.0, 0.07, -0.012)), 0.074 * k, 0.09 * k),
    ]);
    m.chain(vec![
        pt(At::Off(head, k * v(0.0, -0.01, 0.02)), 0.064 * k, 0.05 * k),
        pt(At::J(jaw), 0.036 * k, 0.026 * k),
    ]);
    m.chain(vec![
        pt(At::Off(head, k * v(0.0, 0.0, 0.05)), 0.016 * k, 0.02 * k),
        pt(
            At::Off(head, k * v(0.0, -0.012, 0.08 + 0.012 * p.snout)),
            0.012 * k,
            0.011 * k,
        ),
    ]);
    // Face: brow ridge, lips, chin; per side cheek, ear, eye socket and
    // eyeball. All sized well above the default voxel.
    let h = |x: f32, y: f32, z: f32| At::Off(head, k * v(x, y, z));
    m.chain(vec![
        pt(h(-0.044, 0.03, 0.066), 0.012 * k, 0.011 * k),
        pt(h(0.0, 0.032, 0.078), 0.014 * k, 0.012 * k),
        pt(h(0.044, 0.03, 0.066), 0.012 * k, 0.011 * k),
    ]);
    m.chain(vec![
        pt(h(-0.024, -0.04, 0.074), 0.008 * k, 0.007 * k),
        pt(h(0.0, -0.037, 0.084), 0.011 * k, 0.009 * k),
        pt(h(0.024, -0.04, 0.074), 0.008 * k, 0.007 * k),
    ]);
    m.chain(vec![pt(h(0.0, -0.068, 0.062), 0.022 * k, 0.016 * k)]);
    for (s, _) in SIDES {
        m.chain(vec![pt(h(0.042 * s, -0.014, 0.05), 0.024 * k, 0.022 * k)]);
        m.chain(vec![
            pt(h(0.072 * s, 0.014, -0.006), 0.01 * k, 0.019 * k),
            pt(h(0.077 * s, -0.004, -0.01), 0.01 * k, 0.02 * k),
            pt(h(0.072 * s, -0.024, -0.002), 0.009 * k, 0.012 * k),
        ]);
        m.dots.push(Dot {
            at: h(0.031 * s, 0.012, 0.078),
            r: 0.019 * k,
            subtract: true,
            blend: 0.008 * k,
        });
        m.dots.push(Dot {
            at: h(0.031 * s, 0.011, 0.064),
            r: 0.014 * k,
            subtract: false,
            blend: 0.003 * k,
        });
    }
    for (s, sfx) in SIDES {
        let n = |b: &str| format!("{b}{sfx}");
        let sh = m.j(
            &n("shoulder"),
            Some(chest),
            v(0.18 * s * bx, 1.43 + up, -0.025),
        );
        let el = m.j(&n("elbow"), Some(sh), v(0.203 * s * bx, 1.175 + up, -0.045));
        let wr = m.j(&n("wrist"), Some(el), v(0.222 * s * bx, 0.962 + up, -0.012));
        let hd = m.j(&n("hand"), Some(wr), v(0.228 * s * bx, 0.89 + up, 0.004));
        m.chain(vec![
            pt(At::Mid(neck, sh, 0.25), 0.05 * b, 0.04 * b),
            pt(At::J(sh), 0.05 * b, 0.05 * b),
        ]);
        m.chain(vec![
            pt(At::J(sh), 0.048 * b, 0.05 * b),
            pt(At::Mid(sh, el, 0.45), 0.044 * b, 0.046 * b),
            pt(At::J(el), 0.034 * b, 0.036 * b),
            pt(At::Mid(el, wr, 0.3), 0.036 * b, 0.037 * b),
            pt(At::J(wr), 0.021, 0.028),
            pt(At::J(hd), 0.016, 0.04),
            pt(At::Off(hd, v(0.003 * s, -0.068, 0.006)), 0.011, 0.028),
        ]);
        let th = m.j(&n("thigh"), Some(hip), v(0.09 * s * bx, hy - 0.02, 0.0));
        let kn = m.j(&n("knee"), Some(th), v(0.095 * s * bx, 0.5 * l, 0.018));
        let an = m.j(&n("ankle"), Some(kn), v(0.1 * s * bx, 0.085, -0.025));
        let ball = m.j(&n("ball"), Some(an), v(0.105 * s * bx, 0.03, 0.12));
        let toe = m.j(&n("toe"), Some(ball), v(0.11 * s * bx, 0.026, 0.185));
        m.chain(vec![
            pt(At::J(th), 0.085 * b, 0.09 * b),
            pt(At::Mid(th, kn, 0.45), 0.071 * b, 0.075 * b),
            pt(At::J(kn), 0.05 * b, 0.053 * b),
            pt(At::Mid(kn, an, 0.3), 0.052 * b, 0.057 * b),
            pt(At::J(an), 0.031, 0.035),
        ]);
        // Buttock.
        m.chain(vec![pt(
            At::Off(th, v(-0.012 * s, 0.045, -0.04)),
            0.062 * b,
            0.062 * b,
        )]);
        m.chain(vec![
            pt(At::Off(an, v(0.0, -0.035, -0.03)), 0.034, 0.04),
            pt(At::J(ball), 0.044, 0.028),
            pt(At::J(toe), 0.04, 0.022),
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

/// Lowers a body item to an sdf and its joints.
pub fn lower(def: &BodyDef) -> Result<Lowered, String> {
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
        }
    }
    let g = m.extent(Vec3::Y).0;
    m.shift_y(-g);
    // The sdf.
    let blend = def.blend.unwrap_or(m.blend);
    let mut shapes: Vec<Shape> = m
        .sects()
        .into_iter()
        .map(|c| Shape {
            shape: "chain".into(),
            points: Some(c),
            ..Shape::default()
        })
        .collect();
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
            height: Some(w.w * 0.25),
            ..Shape::default()
        });
    }
    // Default voxel: 1/135 of the largest extent (thin tips such as ears
    // and fin edges would otherwise drive it to the sample limit), coarser
    // when only a light decimated mesh is kept.
    let (ex, ey, ez) = (m.extent(Vec3::X), m.extent(Vec3::Y), m.extent(Vec3::Z));
    let e = Vec3::new(ex.1 - ex.0, ey.1 - ey.0, ez.1 - ez.0);
    let mut res = e.max_element() / 135.0;
    if let Some(t) = def.target_tris.or(def.tris.filter(|_| !def.bake)) {
        let area = 0.55 * 2.0 * (e.x * e.y + e.y * e.z + e.z * e.x);
        let want = (40.0 * t as f32).max(6000.0);
        res = res.max((2.2 * area / want).sqrt().min(e.max_element() / 60.0));
    }
    let sdf = SdfDef {
        shapes,
        blend: Some(blend),
        resolution: Some(def.resolution.unwrap_or(res)),
        tris: def.tris,
        bake: def.bake,
        target_tris: def.target_tris,
        warps: crate::sdf::resolve_joint_anchors(&def.warps, |n| {
            m.joints.iter().find(|j| j.0 == n).map(|j| j.2.to_array())
        }),
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
    let low = lower(def).ok()?;
    let root = low.joints.iter().find(|j| j.parent.is_none())?;
    let mut r: Vec<f32> = low.joints.iter().map(|j| j.radius).collect();
    r.sort_by(f32::total_cmp);
    Some(root.radius.max(r[r.len() / 2]))
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
    lower(&def)
        .map(|l| l.joints.into_iter().map(|j| j.name).collect())
        .unwrap_or_default()
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

    #[test]
    fn every_preset_is_closed_grounded_and_sized() {
        for &p in PRESETS {
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
