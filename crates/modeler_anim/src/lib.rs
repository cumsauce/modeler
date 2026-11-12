//! The dynamic-animation contract shared by the modeler and its Bevy plugin.
//!
//! The modeler bakes a `sway` item into a clip with these functions. It also
//! writes the same [`SwayRig`] into the exported glTF (`extras.sway` on the
//! skeleton's root node), and `modeler_bevy` runs the same code live in the
//! game. The baked clip and the live motion therefore match.
//!
//! Bone frames follow the modeler: at rest every bone is world-aligned (identity
//! rotation) and pivots at its `head`, and bones are listed parents first. A
//! pose is one local (translation offset, rotation) per bone, relative to rest.
//!
//! Two kinds:
//! - **Wind**: a pure function of time, [`SwayRig::wind_rotations`]. Thick bones
//!   bend slowly and a little, thin ones faster and further, and the tips
//!   flutter. With a `period` every frequency snaps to whole cycles, so a baked
//!   clip loops seamlessly.
//! - **Spring**: secondary motion ([`SpringSim`]). Each bone's tip lags behind
//!   where the driving animation puts it, on a damped spring, which suits tails,
//!   ears, antennae and hanging signs. Stepped at a fixed dt, so it is
//!   deterministic.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// One bone as the simulation sees it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SwayBone {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    pub head: [f32; 3],
    pub tail: [f32; 3],
    pub radius: f32,
    /// 0 = this bone doesn't sway (it still follows its parent), 1 = full.
    #[serde(default = "one")]
    pub weight: f32,
}

fn one() -> f32 {
    1.0
}

/// Wind settings. Every field has a default, so `{}` is a light breeze along +x.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Wind {
    /// Where the wind blows to (only the horizontal part counts).
    pub direction: [f32; 3],
    /// 0 still, 0.5 a breeze, 1 a strong wind, 2 a gale.
    pub strength: f32,
    /// How much the strength rises and falls in gusts (0..1).
    pub gust: f32,
    /// Fast jitter of the thinnest bones (twigs, leaf clusters), 0..1.
    pub flutter: f32,
    /// Speeds every motion up (> 1) or down.
    pub frequency: f32,
    pub seed: u32,
}

impl Default for Wind {
    fn default() -> Self {
        Self {
            direction: [1.0, 0.0, 0.0],
            strength: 0.5,
            gust: 0.3,
            flutter: 0.5,
            frequency: 1.0,
            seed: 0,
        }
    }
}

/// Spring settings for secondary motion.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Spring {
    /// Pull toward the animated pose, per second squared: higher is stiffer
    /// and faster.
    pub stiffness: f32,
    /// Velocity damping per second: about 2 * sqrt(stiffness) is critical
    /// (no bounce), lower bounces.
    pub damping: f32,
    /// Share of Earth's gravity pulling the tips down (0 = none).
    pub gravity: f32,
    /// Wind pushing on the tips, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind: Option<Wind>,
}

impl Default for Spring {
    fn default() -> Self {
        Self {
            stiffness: 80.0,
            damping: 8.0,
            gravity: 0.0,
            wind: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwayKind {
    Wind(Wind),
    Spring(Spring),
}

/// What the exporter writes and the runtime reads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SwayRig {
    pub bones: Vec<SwayBone>,
    #[serde(flatten)]
    pub kind: SwayKind,
    /// Seconds after which the motion repeats exactly (the baked clip's
    /// duration). The runtime leaves it `None` and the motion never repeats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<f32>,
    /// The baked clip of this motion in the same file. A game plays either
    /// that clip or the live simulation, not both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
}

/// A frequency snapped to a whole number of cycles per `period`.
fn snap(f: f32, period: Option<f32>) -> f32 {
    match period {
        Some(p) if p > 0.0 => (f * p).round().max(1.0) / p,
        _ => f,
    }
}

fn hash(a: u32, b: u32) -> f32 {
    let mut x = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

const TAU: f32 = std::f32::consts::TAU;

impl Wind {
    fn axes(&self) -> (Vec3, Vec3) {
        let mut d = Vec3::from(self.direction);
        d.y = 0.0;
        let d = d.normalize_or(Vec3::X);
        // Rotating +y about (y x d) by a positive angle leans it toward d.
        (Vec3::Y.cross(d).normalize_or(Vec3::NEG_Z), d)
    }

    /// Strength right now, between (1 - gust) and 1 times `strength`:
    /// three slow waves, periodic in `period`.
    pub fn gust_at(&self, t: f32, period: Option<f32>) -> f32 {
        let s = self.seed;
        let mut g = 0.0;
        let mut total = 0.0;
        for (k, (f, a)) in [(0.11f32, 1.0f32), (0.27, 0.6), (0.61, 0.35)]
            .into_iter()
            .enumerate()
        {
            let f = snap(f * self.frequency, period);
            let ph = hash(s, 101 + k as u32) * TAU;
            g += a * (TAU * f * t + ph).sin();
            total += a;
        }
        let g = 0.5 + 0.5 * g / total; // 0..1
        self.strength * (1.0 - self.gust.clamp(0.0, 1.0) * (1.0 - g))
    }
}

impl SwayRig {
    /// Wind rotations at time `t` (seconds), one local rotation per bone, to
    /// multiply in front of the animated local rotation
    /// (`r = wind[i] * anim[i]`). `phase` offsets the time per instance, so
    /// a forest doesn't sway in lockstep.
    pub fn wind_rotations(&self, w: &Wind, t: f32, phase: f32, out: &mut Vec<Quat>) {
        out.clear();
        let t = t + phase;
        let (bend_axis, along) = w.axes();
        let rmax = self
            .bones
            .iter()
            .filter(|b| b.weight > 0.0)
            .map(|b| b.radius)
            .fold(1e-4f32, f32::max);
        let strength = w.gust_at(t, self.period);
        let has_child: Vec<bool> = {
            let mut v = vec![false; self.bones.len()];
            for b in &self.bones {
                if let Some(p) = b.parent
                    && let Some(slot) = v.get_mut(p as usize)
                {
                    *slot = true;
                }
            }
            v
        };
        for (i, b) in self.bones.iter().enumerate() {
            if b.weight <= 0.0 {
                out.push(Quat::IDENTITY);
                continue;
            }
            let head = Vec3::from(b.head);
            let tail = Vec3::from(b.tail);
            let len = head.distance(tail);
            let thin = (rmax / b.radius.max(rmax * 0.02)).sqrt();
            // Degrees of bend at strength 1: about 2 degrees per meter for
            // the trunk, more for thin wood.
            let flex = 2.0 * len * thin * b.weight;
            let f = snap(0.35 * thin.powf(0.7) * w.frequency, self.period);
            let f2 = snap(f * 1.37, self.period);
            // A phase that varies smoothly in space: the sway travels
            // downwind, and neighbouring bones move together.
            let ph =
                head.dot(along) * 0.7 + 0.8 * (head.x * 1.3 + head.z * 1.7 + w.seed as f32).sin();
            let lean = strength * flex * (0.6 + 0.4 * (TAU * f * t + ph).sin());
            let cross = strength * flex * 0.25 * (TAU * f2 * t + ph * 1.9).sin();
            let mut q = Quat::from_axis_angle(bend_axis, lean.to_radians())
                * Quat::from_axis_angle(along, cross.to_radians());
            // Flutter: the tips and the thinnest wood.
            let tip = !has_child.get(i).copied().unwrap_or(false);
            if w.flutter > 0.0 && (tip || b.radius < rmax * 0.25) {
                let ff = snap(
                    (3.0 + 3.0 * hash(w.seed, i as u32)) * w.frequency,
                    self.period,
                );
                let ff2 = snap(ff * 1.3, self.period);
                let fp = hash(i as u32, w.seed ^ 0x5bd1) * TAU;
                let amt = w.flutter * 6.0 * strength.min(1.5) * b.weight;
                let axis = (tail - head).normalize_or(Vec3::Y);
                q = q
                    * Quat::from_axis_angle(axis, (amt * (TAU * ff * t + fp).sin()).to_radians())
                    * Quat::from_axis_angle(
                        bend_axis,
                        (amt * 0.5 * (TAU * ff2 * t + fp * 2.0).sin()).to_radians(),
                    );
            }
            out.push(q.normalize());
        }
    }

    /// World (position, rotation) of every bone for local (translation
    /// offset, rotation) pairs. Scale is ignored.
    pub fn world(&self, locals: &[(Vec3, Quat)]) -> Vec<(Vec3, Quat)> {
        let mut out: Vec<(Vec3, Quat)> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            let (t, r) = locals
                .get(i)
                .copied()
                .unwrap_or((Vec3::ZERO, Quat::IDENTITY));
            let head = Vec3::from(b.head);
            let w = match b.parent.and_then(|p| out.get(p as usize).copied()) {
                Some((pp, pr)) => {
                    let ph = Vec3::from(self.bones[b.parent.unwrap_or(0) as usize].head);
                    (pp + pr * (head - ph + t), pr * r)
                }
                None => (head + t, r),
            };
            out.push(w);
        }
        out
    }
}

/// The state of a spring simulation: one tip point per bone.
#[derive(Clone, Debug, Default)]
pub struct SpringSim {
    tip: Vec<Vec3>,
    vel: Vec<Vec3>,
    started: bool,
}

/// The fixed step the springs advance by (seconds).
pub const SPRING_DT: f32 = 1.0 / 240.0;

impl SpringSim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the motion (the next step starts at rest on the pose).
    pub fn reset(&mut self) {
        self.started = false;
    }

    /// Advances `dt` seconds (in fixed sub-steps) against the driving
    /// animation's local pose `anim` (translation offset, rotation per
    /// bone) at time `t`, and writes the corrected local rotations to `out`.
    pub fn step(
        &mut self,
        rig: &SwayRig,
        s: &Spring,
        anim: &[(Vec3, Quat)],
        t: f32,
        dt: f32,
        out: &mut Vec<Quat>,
    ) {
        let n = rig.bones.len();
        let steps = ((dt / SPRING_DT).round() as usize).max(1);
        let h = dt / steps as f32;
        for k in 0..steps {
            self.sub_step(rig, s, anim, t - dt + h * (k + 1) as f32, h, out);
        }
        out.truncate(n);
    }

    fn sub_step(
        &mut self,
        rig: &SwayRig,
        s: &Spring,
        anim: &[(Vec3, Quat)],
        t: f32,
        h: f32,
        out: &mut Vec<Quat>,
    ) {
        let n = rig.bones.len();
        if !self.started || self.tip.len() != n {
            self.tip = vec![Vec3::ZERO; n];
            self.vel = vec![Vec3::ZERO; n];
        }
        let wind_force = s.wind.as_ref().map(|w| {
            let (_, along) = w.axes();
            along * w.gust_at(t, rig.period) * 6.0
        });
        let gravity = Vec3::new(0.0, -9.81 * s.gravity, 0.0);
        out.clear();
        let mut world: Vec<(Vec3, Quat)> = Vec::with_capacity(n);
        for (i, b) in rig.bones.iter().enumerate() {
            let (lt, lr) = anim.get(i).copied().unwrap_or((Vec3::ZERO, Quat::IDENTITY));
            let head = Vec3::from(b.head);
            let (parent_pos, parent_rot, ph) =
                match b.parent.and_then(|p| world.get(p as usize).copied()) {
                    Some((pp, pr)) => (
                        pp,
                        pr,
                        Vec3::from(rig.bones[b.parent.unwrap_or(0) as usize].head),
                    ),
                    None => (Vec3::ZERO, Quat::IDENTITY, Vec3::ZERO),
                };
            let pos = parent_pos + parent_rot * (head - ph + lt);
            let rot = parent_rot * lr;
            let rest_dir = Vec3::from(b.tail) - head;
            let len = rest_dir.length();
            if b.weight <= 0.0 || len < 1e-5 {
                world.push((pos, rot));
                out.push(lr);
                continue;
            }
            let target = pos + rot * rest_dir;
            if !self.started {
                self.tip[i] = target;
                self.vel[i] = Vec3::ZERO;
            }
            let mut acc = (target - self.tip[i]) * s.stiffness - self.vel[i] * s.damping + gravity;
            if let Some(f) = wind_force {
                acc += f;
            }
            self.vel[i] += acc * h;
            self.tip[i] += self.vel[i] * h;
            // Keep the bone's length: the tip lives on a sphere round the head.
            let dir = (self.tip[i] - pos).normalize_or((target - pos) / len);
            self.tip[i] = pos + dir * len;
            let radial = self.vel[i].dot(dir);
            self.vel[i] -= dir * radial;
            let anim_dir = (target - pos) / len;
            let fix = Quat::from_rotation_arc(anim_dir, dir);
            let fix = Quat::IDENTITY.slerp(fix, b.weight.clamp(0.0, 1.0));
            let world_rot = (fix * rot).normalize();
            world.push((pos, world_rot));
            out.push((parent_rot.inverse() * world_rot).normalize());
        }
        self.started = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(n: usize) -> Vec<SwayBone> {
        (0..n)
            .map(|i| SwayBone {
                name: format!("b{i}"),
                parent: (i > 0).then(|| i as u32 - 1),
                head: [0.0, i as f32 * 0.5, 0.0],
                tail: [0.0, (i + 1) as f32 * 0.5, 0.0],
                radius: 0.2 / (i + 1) as f32,
                weight: 1.0,
            })
            .collect()
    }

    #[test]
    fn wind_loops_and_leans_downwind() {
        let w = Wind {
            strength: 1.0,
            ..Default::default()
        };
        let rig = SwayRig {
            bones: chain(5),
            kind: SwayKind::Wind(w.clone()),
            period: Some(4.0),
            clip: None,
        };
        let (mut a, mut b) = (Vec::new(), Vec::new());
        rig.wind_rotations(&w, 0.0, 0.0, &mut a);
        rig.wind_rotations(&w, 4.0, 0.0, &mut b);
        for (x, y) in a.iter().zip(&b) {
            assert!(x.angle_between(*y) < 1e-3, "seam {x:?} {y:?}");
        }
        // Averaged over the loop, the tip leans toward +x.
        let mut lean = 0.0;
        for k in 0..40 {
            rig.wind_rotations(&w, k as f32 * 0.1, 0.0, &mut a);
            let locals: Vec<(Vec3, Quat)> = a.iter().map(|q| (Vec3::ZERO, *q)).collect();
            let wp = rig.world(&locals);
            let (p, r) = wp[4];
            lean += (p + r * Vec3::new(0.0, 0.5, 0.0)).x;
        }
        assert!(lean / 40.0 > 0.02, "{}", lean / 40.0);
    }

    #[test]
    fn springs_settle_and_lag() {
        let s = Spring::default();
        let rig = SwayRig {
            bones: chain(3),
            kind: SwayKind::Spring(s.clone()),
            period: None,
            clip: None,
        };
        let mut sim = SpringSim::new();
        let mut out = Vec::new();
        let rest = vec![(Vec3::ZERO, Quat::IDENTITY); 3];
        // Jerk the root sideways: the tip lags behind, then settles back.
        let moved = vec![
            (Vec3::new(0.3, 0.0, 0.0), Quat::IDENTITY),
            (Vec3::ZERO, Quat::IDENTITY),
            (Vec3::ZERO, Quat::IDENTITY),
        ];
        sim.step(&rig, &s, &rest, 0.0, 0.1, &mut out);
        sim.step(&rig, &s, &moved, 0.1, 1.0 / 30.0, &mut out);
        assert!(out[1].angle_between(Quat::IDENTITY) > 0.01, "lags");
        for k in 0..240 {
            sim.step(
                &rig,
                &s,
                &moved,
                0.2 + k as f32 / 60.0,
                1.0 / 60.0,
                &mut out,
            );
        }
        for q in &out {
            assert!(q.angle_between(Quat::IDENTITY) < 1e-3, "settled {q:?}");
        }
    }

    #[test]
    fn rig_round_trips_as_json() {
        let rig = SwayRig {
            bones: chain(2),
            kind: SwayKind::Wind(Wind::default()),
            period: None,
            clip: Some("wind".into()),
        };
        let s = serde_json::to_string(&rig).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.contains("\"kind\":\"wind\""), "{s}");
        let back: SwayRig = serde_json::from_str(&s).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, rig);
    }
}
