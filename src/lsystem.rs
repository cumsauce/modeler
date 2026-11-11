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
//! Every unbroken run of segments becomes ONE tube with mitred rings, so
//! bends are smooth and there are no gaps at joints; branches start inside
//! their parent. The wood is one closed solid per branch, merged into the
//! part `name`; leaves (thin closed blades) go to `name_leaves`.

use std::collections::BTreeMap;

use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::Part;

/// Most symbols an expansion may reach (the string, before drawing).
pub const MAX_SYMBOLS: usize = 400_000;
/// Most drawn segments (each is a ring of the tube).
pub const MAX_SEGMENTS: usize = 40_000;

/// One rewrite choice: `to` replaces the symbol with probability weight `p`.
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
    /// Symbol -> choices (one choice = a deterministic rule).
    pub rules: BTreeMap<char, Vec<Choice>>,
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
    pub translation: [f32; 3],
    pub rotation: Option<[f32; 4]>,
}

impl LSystem {
    /// Whether any `L` can appear (so the op writes `name_leaves`).
    pub fn has_leaves(&self) -> bool {
        self.axiom.contains('L') || self.rules.values().flatten().any(|c| c.to.contains('L'))
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
        Ok(())
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
pub fn expand(ls: &LSystem) -> Result<String, String> {
    let mut rng = Rng(ls.seed ^ 0x5EED);
    let mut s = ls.axiom.clone();
    for it in 0..ls.iterations {
        let mut out = String::with_capacity(s.len() * 2);
        for c in s.chars() {
            match ls.rules.get(&c) {
                Some(cs) if cs.len() == 1 => out.push_str(&cs[0].to),
                Some(cs) => {
                    let total: f32 = cs.iter().map(|c| c.p).sum();
                    let mut x = rng.f() * total;
                    let mut pick = &cs[cs.len() - 1];
                    for ch in cs {
                        if x < ch.p {
                            pick = ch;
                            break;
                        }
                        x -= ch.p;
                    }
                    out.push_str(&pick.to);
                }
                None => out.push(c),
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
    let s = expand(ls)?;
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
    let mut leaves: Vec<(Vec3, Quat)> = Vec::new();
    let mut segments = 0usize;
    // Turtle axes in its own frame: heading +y, left -x, up +z.
    let (h, l, u) = (Vec3::Y, Vec3::NEG_X, Vec3::Z);
    for c in s.chars() {
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
                let len = t.length * (1.0 + ls.length_jitter * rng.s()).max(0.05);
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
                t.pos += t.rot * h * t.length;
                t.chain = None;
            }
            '+' => turn(&mut t, u, a, &mut rng),
            '-' => turn(&mut t, u, -a, &mut rng),
            '&' => turn(&mut t, l, a, &mut rng),
            '^' => turn(&mut t, l, -a, &mut rng),
            '\\' => turn(&mut t, h, a, &mut rng),
            '/' => turn(&mut t, h, -a, &mut rng),
            '|' => t.rot = (t.rot * Quat::from_axis_angle(u, std::f32::consts::PI)).normalize(),
            '!' => t.radius *= ls.radius_scale,
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
                leaves.push((t.pos, (t.rot * roll).normalize()));
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
            "the system draws nothing: {} symbols but no `F` segments (check the rules)",
            s.chars().count()
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
        for (pos, rot) in &leaves {
            blade(&mut lp, *pos, *rot, ls.leaf_size);
        }
        place(&mut lp);
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
            let radial = normal * th.cos() + bin * th.sin();
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

/// A leaf: a thin closed blade (a flat bipyramid) from `pos` along the
/// turtle's heading, width across its left axis, flat faces.
fn blade(part: &mut Part, pos: Vec3, rot: Quat, [w, len]: [f32; 2]) {
    let h = rot * Vec3::Y;
    let l = rot * Vec3::NEG_X;
    let u = rot * Vec3::Z;
    let th = (w * 0.04).max(0.0005);
    let base = pos;
    let tip = pos + h * len;
    let left = pos + h * len * 0.4 + l * w * 0.5;
    let right = pos + h * len * 0.4 - l * w * 0.5;
    let top = pos + h * len * 0.4 + u * th;
    let bot = pos + h * len * 0.4 - u * th;
    let rim = [base, left, tip, right];
    for (apex, up) in [(top, true), (bot, false)] {
        for i in 0..4 {
            let (a, b) = (rim[i], rim[(i + 1) % 4]);
            // Outward winding: counter-clockwise seen from outside.
            let tri = if up { [a, b, apex] } else { [b, a, apex] };
            let mut nrm = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or(u);
            if (nrm.dot(if up { u } else { -u })) < 0.0 {
                nrm = -nrm;
            }
            let o = part.verts.len() as u32;
            for v in tri {
                part.verts.push(v.to_array());
                part.normals.push(nrm.to_array());
                let rel = v - pos;
                part.uvs.push([rel.dot(l), rel.dot(h)]);
            }
            let fix = (tri[1] - tri[0]).cross(tri[2] - tri[0]).dot(nrm) < 0.0;
            if fix {
                part.indices.extend_from_slice(&[o, o + 2, o + 1]);
            } else {
                part.indices.extend_from_slice(&[o, o + 1, o + 2]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn tree() -> LSystem {
        let mut rules = BTreeMap::new();
        rules.insert(
            'X',
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
        );
        rules.insert(
            'F',
            vec![Choice {
                p: 1.0,
                to: "FF".into(),
            }],
        );
        LSystem {
            axiom: "X".into(),
            rules,
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
