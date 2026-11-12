//! L-systems: a string of symbols rewritten by rules for a few iterations,
//! then read by a 3D turtle into geometry. Trees, bushes, ferns, weeds,
//! coral, roots, lightning, antennae: anything that branches.
//!
//! The turtle (heading H, left L, up U; it starts heading +y):
//! - `F` / `G` draw a branch segment of the current length, `f` moves
//!   without drawing (starting a new branch);
//! - `+` / `-` turn left / right (about U), `&` / `^` pitch down / up
//!   (about L), `\` / `/` roll left / right (about H), `|` turns around;
//! - `[` / `]` push / pop the turtle (a side branch: the child is thinner by
//!   `radius_scale` and shorter by `length_scale`);
//! - `!` thins the current branch by `radius_scale`, `L` places a leaf;
//! - any other symbol (`X`, `A`, ...) only drives rewriting.
//!
//! **Parametric** systems carry numbers on modules: `A(1, 0.1)`. A rule
//! head names them and may test them, `A(l, w) : l > 0.05`, and its
//! successor computes new ones, `!(w)F(l)[&(40)A(l*0.6, w*0.7)]A(l*0.9, w)`.
//! The turtle reads a module's first number as its own argument: `F(l)` /
//! `f(l)` length, `+(a)` and the other turns degrees, `!(w)` sets the
//! radius, `L(s)` scales the leaf. A symbol's rules are tried in order (the
//! rule list's order); the first whose parameter count matches and whose
//! condition holds rewrites it.
//!
//! Every unbroken run of segments becomes ONE tube with mitred rings, so
//! bends are smooth and there are no gaps at joints; branches start inside
//! their parent. The wood is one closed solid per branch, merged into the
//! part `name`; leaves (thin closed blades) go to `name_leaves`.

use std::collections::BTreeMap;

use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::Part;

pub mod expr;
use expr::{Head, Module, Template};

/// Most symbols an expansion may reach (the string, before drawing).
pub const MAX_SYMBOLS: usize = 400_000;
/// Most drawn segments (each is a ring of the tube).
pub const MAX_SEGMENTS: usize = 40_000;

/// One rewrite choice: `to` replaces the module with probability weight `p`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Choice {
    pub p: f32,
    pub to: String,
}

/// An L-system and how its turtle draws (all lengths in meters, angles in
/// degrees).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LSystem {
    pub axiom: String,
    /// Rules in the order they are tried: a head (`A`, `A(l, w)`,
    /// `A(l, w) : l > 0.1`) and its choices (one = deterministic).
    pub rules: Vec<(String, Vec<Choice>)>,
    /// Named numbers usable in rule expressions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub constants: BTreeMap<String, f64>,
    pub iterations: u32,
    pub angle: f32,
    pub length: f32,
    pub length_scale: f32,
    pub radius: f32,
    pub radius_scale: f32,
    /// Radius multiplier per segment along a branch (gentle taper).
    pub taper: f32,
    /// Roll added at every `[` (137.5 spreads branches like phyllotaxis).
    pub branch_roll: f32,
    /// Random +- degrees on every turn, and +- share on every length.
    pub jitter: f32,
    pub length_jitter: f32,
    /// A direction branches bend toward after every segment ([0, -1, 0] =
    /// droop, [0, 1, 0] = reach up), by `tropism_strength`.
    pub tropism: [f32; 3],
    pub tropism_strength: f32,
    pub seed: u64,
    /// Sides of the tube.
    pub sides: u32,
    /// Leaf blade [width, length]; a leaf is placed at every `L`.
    pub leaf_size: [f32; 2],
    #[serde(default)]
    pub leaf: LeafStyle,
    pub translation: [f32; 3],
    pub rotation: Option<[f32; 4]>,
}

/// How leaves look.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LeafStyle {
    /// Outline: ellipse, lance, round, heart, diamond, needle, maple.
    pub shape: String,
    /// Pitch away from the branch, degrees (leaves stand out of the stem).
    pub angle: f32,
    /// Bend along the length, degrees from base to tip (tips droop).
    pub curl: f32,
    /// V-fold along the midrib, degrees (0 = flat).
    pub fold: f32,
    /// Leaf stalk length, meters.
    pub stalk: f32,
}

impl Default for LeafStyle {
    fn default() -> Self {
        LeafStyle {
            shape: "ellipse".into(),
            angle: 0.0,
            curl: 0.0,
            fold: 0.0,
            stalk: 0.0,
        }
    }
}

pub const LEAF_SHAPES: &[&str] = &[
    "ellipse", "lance", "round", "heart", "diamond", "needle", "maple",
];

/// Half-width (as a share of the leaf width) at `t` (0 base .. 1 tip).
fn leaf_profile(shape: &str, t: f32) -> f32 {
    use std::f32::consts::PI;
    let s = (PI * t).sin().max(0.0);
    match shape {
        "lance" => 0.5 * s * (1.0 - t).powf(0.4),
        "round" => 0.5 * s.powf(0.5),
        "heart" => 0.5 * s.powf(0.7) * (1.25 - t * 0.9),
        "diamond" => 0.5 * if t < 0.4 { t / 0.4 } else { (1.0 - t) / 0.6 },
        "needle" => 0.5 * (1.0 - t).powf(0.3) * (t * 20.0).min(1.0),
        // Five lobes: a round blade whose edge dips between lobe tips.
        "maple" => 0.5 * s.powf(0.4) * (0.75 + 0.25 * (t * 5.0 * PI).cos().abs()),
        _ => 0.5 * s.powf(0.8),
    }
}

struct Rule {
    head: Head,
    choices: Vec<(f32, Vec<Template>)>,
}

/// The axiom and rules, parsed and compiled.
struct Compiled {
    axiom: Vec<Module>,
    rules: std::collections::HashMap<char, Vec<Rule>>,
}

fn compile(ls: &LSystem) -> Result<Compiled, String> {
    let axiom = expr::parse_word(&ls.axiom, &[], &ls.constants)
        .map_err(|e| format!("axiom: {e}"))?
        .iter()
        .map(|t| t.eval(&[]))
        .collect();
    let mut rules: std::collections::HashMap<char, Vec<Rule>> = Default::default();
    for (head, choices) in &ls.rules {
        let h = expr::parse_head(head, &ls.constants).map_err(|e| format!("rules: {e}"))?;
        let mut cs = Vec::new();
        for c in choices {
            let w = expr::parse_word(&c.to, &h.vars, &ls.constants)
                .map_err(|e| format!("rules.{head}: {e}"))?;
            cs.push((c.p, w));
        }
        rules.entry(h.sym).or_default().push(Rule {
            head: h,
            choices: cs,
        });
    }
    // Conditional rules are tried before a symbol's unconditional one, so
    // `A(l)` as a fallback never shadows `A(l) : l > 1` (keys sort first).
    for rs in rules.values_mut() {
        rs.sort_by_key(|r| r.head.cond.is_none());
    }
    Ok(Compiled { axiom, rules })
}

impl LSystem {
    /// Whether any `L` can appear (so the op writes `name_leaves`).
    pub fn has_leaves(&self) -> bool {
        let leafy = |w: &str| w.split(['(', ')']).step_by(2).any(|p| p.contains('L'));
        leafy(&self.axiom) || self.rules.iter().flat_map(|r| &r.1).any(|c| leafy(&c.to))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.axiom.is_empty() {
            return Err("axiom: empty".into());
        }
        if self.iterations > 12 {
            return Err(format!(
                "iterations: {} is too many (at most 12; strings grow exponentially)",
                self.iterations
            ));
        }
        for (k, v) in [
            ("length", self.length),
            ("radius", self.radius),
            ("length_scale", self.length_scale),
            ("radius_scale", self.radius_scale),
            ("taper", self.taper),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(format!("{k}: must be > 0, got {v}"));
            }
        }
        if !(3..=32).contains(&self.sides) {
            return Err(format!("sides: 3..32, got {}", self.sides));
        }
        for (s, cs) in &self.rules {
            if cs.is_empty() || cs.iter().any(|c| !(c.p.is_finite() && c.p >= 0.0)) {
                return Err(format!("rules.{s}: needs choices with weights >= 0"));
            }
        }
        if !LEAF_SHAPES.contains(&self.leaf.shape.as_str()) {
            return Err(format!(
                "leaf.shape: unknown `{}` ({})",
                self.leaf.shape,
                LEAF_SHAPES.join(", ")
            ));
        }
        compile(self).map(|_| ())
    }
}

/// splitmix64: small, fast, deterministic per seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Uniform in [-1, 1).
    fn s(&mut self) -> f32 {
        self.f() * 2.0 - 1.0
    }
}

/// Rewrites the axiom `iterations` times.
fn expand(ls: &LSystem, c: &Compiled) -> Result<Vec<Module>, String> {
    let mut rng = Rng(ls.seed ^ 0x5EED);
    let mut s = c.axiom.clone();
    for it in 0..ls.iterations {
        let mut out = Vec::with_capacity(s.len() * 2);
        for m in &s {
            let args = &m.a[..m.n as usize];
            let rule = c.rules.get(&m.sym).and_then(|rs| {
                rs.iter().find(|r| {
                    r.head.vars.len() == args.len()
                        && r.head.cond.as_ref().is_none_or(|e| e.eval(args) != 0.0)
                })
            });
            let Some(rule) = rule else {
                out.push(*m);
                continue;
            };
            let pick = if rule.choices.len() == 1 {
                &rule.choices[0].1
            } else {
                let total: f32 = rule.choices.iter().map(|c| c.0).sum();
                let mut x = rng.f() * total;
                let mut pick = &rule.choices[rule.choices.len() - 1].1;
                for (p, w) in &rule.choices {
                    if x < *p {
                        pick = w;
                        break;
                    }
                    x -= p;
                }
                pick
            };
            let at = out.len();
            out.extend(pick.iter().map(|t| t.eval(args)));
            if let Some(bad) = out[at..]
                .iter()
                .find(|o| o.a[..o.n as usize].iter().any(|x| !x.is_finite()))
            {
                return Err(format!(
                    "a rule for `{}` made `{}` with a non-finite number at iteration {} (a division by zero, or sqrt of a negative?)",
                    m.sym,
                    bad.sym,
                    it + 1
                ));
            }
            if out.len() > MAX_SYMBOLS {
                return Err(format!(
                    "the string passes {MAX_SYMBOLS} symbols at iteration {}: lower iterations (or make rules grow slower)",
                    it + 1
                ));
            }
        }
        s = out;
    }
    Ok(s)
}

/// The expanded string, written back as text (for tests and debugging).
pub fn expand_text(ls: &LSystem) -> Result<String, String> {
    let c = compile(ls)?;
    let mut out = String::new();
    for m in expand(ls, &c)? {
        out.push(m.sym);
        if m.n > 0 {
            let a: Vec<String> = m.a[..m.n as usize].iter().map(|x| format!("{x}")).collect();
            out += &format!("({})", a.join(","));
        }
    }
    Ok(out)
}

#[derive(Clone, Copy)]
struct Turtle {
    pos: Vec3,
    rot: Quat,
    radius: f32,
    length: f32,
    chain: Option<usize>,
}

/// A branch: points along it with radii.
struct Chain {
    pts: Vec<(Vec3, f32)>,
}

pub struct Drawn {
    pub wood: Part,
    pub leaves: Option<Part>,
    pub segments: usize,
    pub leaf_count: usize,
}

/// Expands and draws: the wood part `name` and, when the system places
/// leaves, `name_leaves`.
pub fn build(name: &str, ls: &LSystem) -> Result<Drawn, String> {
    ls.validate()?;
    let compiled = compile(ls)?;
    let s = expand(ls, &compiled)?;
    let mut rng = Rng(ls.seed);
    let a = ls.angle.to_radians();
    let jit = ls.jitter.to_radians();
    let trop = Vec3::from(ls.tropism).normalize_or_zero();
    let mut t = Turtle {
        pos: Vec3::ZERO,
        rot: Quat::IDENTITY,
        radius: ls.radius,
        length: ls.length,
        chain: None,
    };
    let mut stack: Vec<Turtle> = Vec::new();
    let mut chains: Vec<Chain> = Vec::new();
    let mut leaves: Vec<(Vec3, Quat, f32)> = Vec::new();
    let mut segments = 0usize;
    // Turtle axes in its own frame: heading +y, left -x, up +z.
    let (h, l, u) = (Vec3::Y, Vec3::NEG_X, Vec3::Z);
    for m in &s {
        let c = m.sym;
        let arg = m.arg();
        // A turn's own angle (degrees) when the module carries one.
        let a = arg.map_or(a, f32::to_radians);
        let turn = |t: &mut Turtle, axis: Vec3, ang: f32, rng: &mut Rng| {
            let ang = ang + jit * rng.s();
            t.rot = (t.rot * Quat::from_axis_angle(axis, ang)).normalize();
        };
        match c {
            'F' | 'G' => {
                segments += 1;
                if segments > MAX_SEGMENTS {
                    return Err(format!(
                        "more than {MAX_SEGMENTS} segments: lower iterations"
                    ));
                }
                let len = arg.unwrap_or(t.length) * (1.0 + ls.length_jitter * rng.s()).max(0.05);
                let idx = match t.chain {
                    Some(i) => i,
                    None => {
                        chains.push(Chain {
                            pts: vec![(t.pos, t.radius)],
                        });
                        chains.len() - 1
                    }
                };
                t.chain = Some(idx);
                t.pos += t.rot * h * len;
                t.radius *= ls.taper;
                chains[idx].pts.push((t.pos, t.radius));
                if trop != Vec3::ZERO && ls.tropism_strength != 0.0 {
                    let head = t.rot * h;
                    let axis = head.cross(trop);
                    let m = axis.length();
                    if m > 1e-6 {
                        let bend = Quat::from_axis_angle(axis / m, ls.tropism_strength * m);
                        t.rot = (bend * t.rot).normalize();
                    }
                }
            }
            'f' => {
                t.pos += t.rot * h * arg.unwrap_or(t.length);
                t.chain = None;
            }
            '+' => turn(&mut t, u, a, &mut rng),
            '-' => turn(&mut t, u, -a, &mut rng),
            '&' => turn(&mut t, l, a, &mut rng),
            '^' => turn(&mut t, l, -a, &mut rng),
            '\\' => turn(&mut t, h, a, &mut rng),
            '/' => turn(&mut t, h, -a, &mut rng),
            '|' => t.rot = (t.rot * Quat::from_axis_angle(u, std::f32::consts::PI)).normalize(),
            '!' => match arg {
                Some(w) => t.radius = w.max(0.0),
                None => t.radius *= ls.radius_scale,
            },
            '[' => {
                stack.push(t);
                t.chain = None;
                t.radius *= ls.radius_scale;
                t.length *= ls.length_scale;
                if ls.branch_roll != 0.0 {
                    t.rot =
                        (t.rot * Quat::from_axis_angle(h, ls.branch_roll.to_radians())).normalize();
                }
            }
            ']' => {
                if let Some(p) = stack.pop() {
                    t = p;
                }
            }
            'L' => {
                let roll = Quat::from_axis_angle(h, rng.s() * std::f32::consts::PI);
                leaves.push((t.pos, (t.rot * roll).normalize(), arg.unwrap_or(1.0)));
            }
            _ => {}
        }
    }
    let mut wood = Part::new(name);
    for ch in &chains {
        if ch.pts.len() >= 2 {
            tube(&mut wood, &ch.pts, ls.sides as usize);
        }
    }
    if wood.indices.is_empty() {
        return Err(format!(
            "the system draws nothing: {} modules but no `F` segments (check the rules)",
            s.len()
        ));
    }
    let place = |p: &mut Part| {
        p.translation = ls.translation;
        if let Some(r) = ls.rotation {
            p.rotation = r;
        }
    };
    place(&mut wood);
    let leaf_count = leaves.len();
    let leaves = ls.has_leaves().then(|| {
        let mut lp = Part::new(format!("{name}_leaves"));
        for (pos, rot, k) in &leaves {
            blade(
                &mut lp,
                *pos,
                *rot,
                [ls.leaf_size[0] * k, ls.leaf_size[1] * k],
                &ls.leaf,
            );
        }
        place(&mut lp);
        // No `L` was reached: keep the name (materials may point at it) but
        // hide the empty part so nothing renders or exports it.
        lp.visible = leaf_count > 0;
        lp
    });
    Ok(Drawn {
        wood,
        leaves,
        segments,
        leaf_count,
    })
}

/// Any unit vector perpendicular to `v`.
fn perp(v: Vec3) -> Vec3 {
    let a = if v.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };
    v.cross(a).normalize()
}

/// A closed tube through `pts` (rings along the averaged direction, parallel-transported frame,
/// fan caps), appended to `part`. UVs are metric (around, along).
fn tube(part: &mut Part, pts: &[(Vec3, f32)], sides: usize) {
    let n = pts.len();
    let dir = |i: usize| -> Vec3 {
        let a = pts[i.saturating_sub(1)].0;
        let b = pts[(i + 1).min(n - 1)].0;
        (b - a).normalize_or(Vec3::Y)
    };
    let mut normal = perp(dir(0));
    let mut along = 0.0f32;
    let base = part.verts.len() as u32;
    let ring = sides + 1; // a seam column for the uvs
    for i in 0..n {
        let d = dir(i);
        // Parallel transport: remove the tangent part of the last normal.
        normal = (normal - d * normal.dot(d)).normalize_or(perp(d));
        let bin = d.cross(normal);
        let (p, r) = pts[i];
        if i > 0 {
            along += pts[i].0.distance(pts[i - 1].0);
        }
        for k in 0..ring {
            let th = k as f32 / sides as f32 * std::f32::consts::TAU;
            // The seam column repeats column 0 exactly (only its uv differs),
            // so welding by position always closes it.
            let a = if k == sides { 0.0 } else { th };
            let radial = normal * a.cos() + bin * a.sin();
            part.verts.push((p + radial * r).to_array());
            part.normals.push(radial.to_array());
            part.uvs.push([th * r, along]);
        }
    }
    for i in 0..n as u32 - 1 {
        for k in 0..sides as u32 {
            let a = base + i * ring as u32 + k;
            let b = a + 1;
            let c = a + ring as u32;
            let d = c + 1;
            part.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    // Caps: a centre vertex and its own ring (flat normals).
    for (end, sign) in [(0usize, -1.0f32), (n - 1, 1.0)] {
        let d = dir(end) * sign;
        let c0 = part.verts.len() as u32;
        part.verts.push(pts[end].0.to_array());
        part.normals.push(d.to_array());
        part.uvs.push([0.0, 0.0]);
        let src = base + (end * ring) as u32;
        for k in 0..sides as u32 {
            let v = part.verts[(src + k) as usize];
            part.verts.push(v);
            part.normals.push(d.to_array());
            part.uvs.push([v[0] - pts[end].0.x, v[2] - pts[end].0.z]);
        }
        for k in 0..sides as u32 {
            let a = c0 + 1 + k;
            let b = c0 + 1 + (k + 1) % sides as u32;
            if sign > 0.0 {
                part.indices.extend_from_slice(&[c0, a, b]);
            } else {
                part.indices.extend_from_slice(&[c0, b, a]);
            }
        }
    }
}

/// A leaf: a thin closed blade from `pos`, pitched `angle` off the
/// heading, with its outline from `shape`, a V `fold` along the midrib, a
/// `curl` toward its underside along the length and an optional stalk.
/// Built as rows across the blade: each interior row has a left edge, a
/// midrib on top, a right edge and a midrib underneath (edges shared, so
/// the blade is closed); base and tip are single points. Flat-shaded.
fn blade(part: &mut Part, pos: Vec3, rot: Quat, [w, len]: [f32; 2], st: &LeafStyle) {
    if w <= 0.0 || len <= 0.0 {
        return;
    }
    let rot = (rot * Quat::from_axis_angle(Vec3::NEG_X, st.angle.to_radians())).normalize();
    let (h, l, u) = (rot * Vec3::Y, rot * Vec3::NEG_X, rot * Vec3::Z);
    let base = pos + h * st.stalk;
    let th = (w * 0.03).max(0.0004);
    let fold = st.fold.to_radians() * 0.5;
    let rows = if st.shape == "needle" { 3 } else { 8 };
    let curl = st.curl.to_radians();
    // The midrib's path: a circular arc bent toward -u by `curl`.
    let rib = |t: f32| -> (Vec3, Vec3, Vec3) {
        let ang = curl * t;
        let (along, up) = if curl.abs() < 1e-4 {
            (h * len * t, Vec3::ZERO)
        } else {
            let r = len / curl;
            (h * r * ang.sin(), -u * r * (1.0 - ang.cos()))
        };
        let tangent = (h * ang.cos() - u * ang.sin()).normalize();
        let normal = tangent.cross(l).normalize();
        (base + along + up, tangent, normal)
    };
    let tip_t = 1.0;
    let mut grid: Vec<[Vec3; 4]> = Vec::new();
    for r in 1..rows {
        let t = r as f32 / rows as f32;
        let (c, _, n) = rib(t);
        let half = leaf_profile(&st.shape, t) * w;
        let lift = n * half * fold.sin();
        let left = c + l * half * fold.cos() + lift;
        let right = c - l * half * fold.cos() + lift;
        let thick = th * (1.0 - (2.0 * t - 1.0).powi(2)).max(0.2);
        grid.push([left, c + n * thick, right, c - n * thick]);
    }
    let (tip, ..) = rib(tip_t);
    let mut tri = |a: Vec3, b: Vec3, c: Vec3, out: Vec3| {
        let mut nrm = (b - a).cross(c - a);
        let (b, c) = if nrm.dot(out) < 0.0 {
            nrm = -nrm;
            (c, b)
        } else {
            (b, c)
        };
        let nrm = nrm.normalize_or(out);
        let o = part.verts.len() as u32;
        for v in [a, b, c] {
            part.verts.push(v.to_array());
            part.normals.push(nrm.to_array());
            let rel = v - pos;
            part.uvs.push([rel.dot(l), rel.dot(h)]);
        }
        part.indices.extend_from_slice(&[o, o + 1, o + 2]);
    };
    // Faces: top = (left, top rib) and (top rib, right); bottom likewise.
    let faces: [(usize, usize, f32); 4] = [(0, 1, 1.0), (1, 2, 1.0), (0, 3, -1.0), (3, 2, -1.0)];
    for (i, j, side) in faces {
        let out = |k: usize| rib((k as f32 + 1.0) / rows as f32).2 * side;
        tri(base, grid[0][i], grid[0][j], out(0));
        for k in 0..grid.len() - 1 {
            let (a, b) = (grid[k], grid[k + 1]);
            tri(a[i], b[i], b[j], out(k));
            tri(a[i], b[j], a[j], out(k));
        }
        let last = grid.len() - 1;
        tri(grid[last][i], tip, grid[last][j], out(last));
    }
    if st.stalk > 0.0 {
        // A thin square stalk from the branch to the blade.
        let r = (w * 0.03).max(0.001);
        let (a, b) = (pos, base);
        let pts = [(a, r), (b, r)];
        tube(part, &pts, 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn tree() -> LSystem {
        let rules = vec![
            (
                "X".to_string(),
                vec![
                    Choice {
                        p: 1.0,
                        to: "F[+XL]F[-XL]&[XL]".into(),
                    },
                    Choice {
                        p: 1.0,
                        to: "F[^XL][&XL]".into(),
                    },
                ],
            ),
            (
                "F".to_string(),
                vec![Choice {
                    p: 1.0,
                    to: "FF".into(),
                }],
            ),
        ];
        LSystem {
            axiom: "X".into(),
            rules,
            constants: BTreeMap::new(),
            iterations: 4,
            angle: 25.0,
            length: 0.2,
            length_scale: 0.9,
            radius: 0.05,
            radius_scale: 0.7,
            taper: 0.97,
            branch_roll: 137.5,
            jitter: 5.0,
            length_jitter: 0.1,
            tropism: [0.0, -1.0, 0.0],
            tropism_strength: 0.05,
            seed: 7,
            sides: 7,
            leaf_size: [0.06, 0.12],
            leaf: LeafStyle {
                curl: 40.0,
                fold: 30.0,
                angle: 30.0,
                stalk: 0.02,
                ..LeafStyle::default()
            },
            translation: [0.0; 3],
            rotation: None,
        }
    }

    #[test]
    fn a_tree_expands_deterministically_and_draws_closed_wood_and_leaves() {
        let ls = tree();
        let a = build("tree", &ls).unwrap_or_else(|e| panic!("{e}"));
        let b = build("tree", &ls).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.wood.verts, b.wood.verts);
        assert!(a.segments > 20, "{}", a.segments);
        assert!(a.leaf_count > 5);
        let leaves = a.leaves.unwrap_or_else(|| panic!("no leaves"));
        assert_eq!(leaves.name, "tree_leaves");
        for p in [&a.wood, &leaves] {
            assert_eq!(crate::verify::open_edge_count(p), 0, "{} is open", p.name);
            let vol: f32 = p
                .indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| {
                    let v = |i: u32| Vec3::from(p.verts[i as usize]);
                    v(t[0]).dot(v(t[1]).cross(v(t[2]))) / 6.0
                })
                .sum();
            assert!(vol > 0.0, "{} is wound inward ({vol})", p.name);
        }
        // Grows upward from the origin.
        let top = a.wood.verts.iter().map(|v| v[1]).fold(f32::MIN, f32::max);
        let low = a.wood.verts.iter().map(|v| v[1]).fold(f32::MAX, f32::min);
        assert!(top > 1.0 && low > -0.06, "{low}..{top}");
        // Another seed grows another tree.
        let c = build("tree", &LSystem { seed: 8, ..ls }).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(a.wood.verts, c.wood.verts);
    }

    #[test]
    fn runaway_growth_is_refused_with_a_hint() {
        let mut ls = tree();
        ls.iterations = 12;
        let Err(e) = build("t", &ls) else {
            panic!("a 12-iteration bush should be refused")
        };
        assert!(e.contains("lower iterations"), "{e}");
    }
}

#[cfg(test)]
mod op_tests {
    use serde_json::json;

    #[test]
    fn every_preset_grows_in_a_batch_and_follows_a_param() {
        for preset in ["tree", "pine", "bush", "fern", "weed", "roots"] {
            let mut s = crate::commands::Session::default();
            s.apply_values(
                None,
                vec![json!({"type": "lsystem", "name": "p", "preset": preset})],
            )
            .unwrap_or_else(|e| panic!("{preset}: {e}"));
            assert!(s.model.parts.iter().any(|p| p.name == "p"), "{preset}");
            let leaves = s.model.parts.iter().any(|p| p.name == "p_leaves");
            assert_eq!(leaves, preset != "roots", "{preset}");
            let tris: usize = s.model.parts.iter().map(|p| p.indices.len() / 3).sum();
            assert!(tris < 400_000, "{preset}: {tris} triangles");
        }
        let mut s = crate::commands::Session::default();
        s.apply_values(
            Some([("h".to_string(), 0.3)].into()),
            vec![json!({"type": "lsystem", "name": "t", "axiom": "F[+F]F",
                        "rules": {"F": ["F[+F]F", {"p": 2, "to": "F[-F]F"}]},
                        "iterations": 2, "length": "$h", "radius": 0.02})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let top = |s: &crate::commands::Session| {
            s.model.parts[0]
                .verts
                .iter()
                .map(|v| v[1])
                .fold(f32::MIN, f32::max)
        };
        let before = top(&s);
        s.apply_values(Some([("h".to_string(), 0.6)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(top(&s) > before * 1.8, "{before} -> {}", top(&s));
        let Err(e) = s.apply_values(
            None,
            vec![json!({"type": "lsystem", "name": "x", "axiom": "F"})],
        ) else {
            panic!("no rules should fail")
        };
        assert!(e.to_string().contains("preset"), "{e}");
    }
}

#[cfg(test)]
mod parametric_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_parametric_system_carries_numbers_and_stops_on_a_condition() {
        let mut s = crate::commands::Session::default();
        s.apply_values(
            Some([("shrink".to_string(), 0.6)].into()),
            vec![json!({"type": "lsystem", "name": "t", "axiom": "A(1, 0.08)",
                "constants": {"k": "$shrink"},
                "rules": {
                    "A(l, w) : l >= 0.2": "!(w)F(l)[&(40)A(l*k, w*0.6)L(0.5)]/(137.5)[&(40)A(l*k, w*0.6)]A(l*0.8, w*0.8)",
                    "A(l, w) : l < 0.2": "L(1)"
                },
                "iterations": 9, "leaf_shape": "maple", "leaf_size": [0.1, 0.1],
                "leaf_angle": 30, "leaf_curl": 30, "leaf_fold": 20, "leaf_stalk": 0.03})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let wood = s
            .model
            .parts
            .iter()
            .find(|p| p.name == "t")
            .unwrap_or_else(|| panic!("no t"));
        let leaves = s
            .model
            .parts
            .iter()
            .find(|p| p.name == "t_leaves")
            .unwrap_or_else(|| panic!("no leaves"));
        for p in [wood, leaves] {
            assert_eq!(crate::verify::open_edge_count(p), 0, "{} is open", p.name);
        }
        // The trunk is 1 m: the first segment's length came from A(1, ..).
        let top = wood.verts.iter().map(|v| v[1]).fold(f32::MIN, f32::max);
        assert!(top > 1.5 && top < 6.0, "{top}");
        // A smaller param shrinks the side branches.
        let span = |s: &crate::commands::Session| {
            let p = s
                .model
                .parts
                .iter()
                .find(|p| p.name == "t")
                .unwrap_or_else(|| panic!());
            p.verts.iter().map(|v| v[0].abs()).fold(0.0f32, f32::max)
        };
        let before = span(&s);
        s.apply_values(Some([("shrink".to_string(), 0.4)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(span(&s) < before, "{before} -> {}", span(&s));
    }

    #[test]
    fn a_bare_rule_is_the_fallback_after_conditional_ones() {
        let one = |to: &str| {
            vec![Choice {
                p: 1.0,
                to: to.into(),
            }]
        };
        let ls = LSystem {
            axiom: "A(2)".into(),
            // "A(n)" sorts before "A(n) : n > 0", yet the condition wins.
            rules: vec![
                ("A(n)".into(), one("X")),
                ("A(n) : n > 0".into(), one("F(1)A(n-1)")),
            ],
            iterations: 4,
            ..super::tests::tree()
        };
        assert_eq!(
            expand_text(&ls).unwrap_or_else(|e| panic!("{e}")),
            "F(1)F(1)X"
        );
        let bad = LSystem {
            axiom: "A(0)".into(),
            rules: vec![("A(n)".into(), one("F(1/n)"))],
            iterations: 1,
            ..super::tests::tree()
        };
        assert!(expand_text(&bad).is_err_and(|e| e.contains("non-finite")));
        // An L that is never reached leaves a hidden, empty leaves part.
        let bare = LSystem {
            axiom: "F".into(),
            rules: vec![("X".into(), one("L"))],
            iterations: 1,
            ..super::tests::tree()
        };
        let d = build("b", &bare).unwrap_or_else(|e| panic!("{e}"));
        assert!(d.leaves.is_some_and(|l| !l.visible && l.verts.is_empty()));
    }

    #[test]
    fn expansion_text_and_errors() {
        let ls = LSystem {
            axiom: "A(3)".into(),
            rules: vec![(
                "A(n) : n > 0".into(),
                vec![Choice {
                    p: 1.0,
                    to: "F(n)A(n-1)".into(),
                }],
            )],
            iterations: 5,
            ..super::tests::tree()
        };
        assert_eq!(
            expand_text(&ls).unwrap_or_else(|e| panic!("{e}")),
            "F(3)F(2)F(1)A(0)"
        );
        let bad = LSystem {
            rules: vec![(
                "A(n)".into(),
                vec![Choice {
                    p: 1.0,
                    to: "F(m)".into(),
                }],
            )],
            ..ls
        };
        let Err(e) = bad.validate() else {
            panic!("unknown name should fail")
        };
        assert!(e.contains("unknown name `m`"), "{e}");
    }

    #[test]
    fn every_leaf_shape_is_closed() {
        for shape in LEAF_SHAPES {
            let mut p = Part::new("l");
            let st = LeafStyle {
                shape: shape.to_string(),
                angle: 30.0,
                curl: 50.0,
                fold: 40.0,
                stalk: 0.0,
            };
            blade(&mut p, Vec3::ZERO, Quat::IDENTITY, [0.08, 0.15], &st);
            assert_eq!(crate::verify::open_edge_count(&p), 0, "{shape} is open");
            let vol: f32 = p
                .indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| {
                    let v = |i: u32| Vec3::from(p.verts[i as usize]);
                    v(t[0]).dot(v(t[1]).cross(v(t[2]))) / 6.0
                })
                .sum();
            assert!(vol > 0.0, "{shape} wound inward ({vol})");
        }
    }
}
#[cfg(test)]
mod seed_tests {
    /// Tube seams once differed by float noise and sometimes straddled the
    /// validator's weld grid: every preset and seed must come out closed.
    #[test]
    fn every_preset_is_closed_for_many_seeds() {
        for preset in ["tree", "pine", "bush", "fern", "weed", "roots"] {
            for seed in 0..6u64 {
                let mut ls = crate::spec::lsystem_preset(preset).unwrap_or_else(|e| panic!("{e}"));
                ls.seed = seed;
                let d = super::build("t", &ls).unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(
                    crate::verify::open_edge_count(&d.wood),
                    0,
                    "{preset} {seed} wood"
                );
                if let Some(l) = d.leaves {
                    assert_eq!(
                        crate::verify::open_edge_count(&l),
                        0,
                        "{preset} {seed} leaves"
                    );
                }
            }
        }
    }
}
