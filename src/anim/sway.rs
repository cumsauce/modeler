//! `sway` items: wind and springs (the `modeler_anim` simulation) as clips.
//!
//! A wind clip is evaluated analytically at every sample: periodic in its
//! duration, so it loops seamlessly. A spring clip runs the simulation over
//! three loops of its driver (the `over` clip, or rest) and keeps the last,
//! with the small mismatch at the seam spread over the loop.

use std::sync::Arc;

use bevy::math::{Quat, Vec3};
use modeler_anim::{SPRING_DT, SpringSim, SwayBone, SwayKind, SwayRig};
use serde::{Deserialize, Serialize};

use super::{Clip, Skeleton};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SwayDef {
    pub skeleton: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bones: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f32>,
    pub motion: SwayKind,
}

/// The dynamic part of a clip, on top of its base.
#[derive(Clone, Debug)]
pub enum Layer {
    Wind(SwayRig),
    /// Local rotations per bone at `SPRING_DT` steps over one loop.
    Spring(Vec<Vec<Quat>>),
}

/// The simulation's view of a skeleton: which bones sway.
pub fn rig_of(skel: &Skeleton, def: &SwayDef, period: Option<f32>, clip: &str) -> SwayRig {
    SwayRig {
        bones: skel
            .bones
            .iter()
            .map(|b| SwayBone {
                name: b.name.clone(),
                parent: b.parent.map(|p| p as u32),
                head: b.head.to_array(),
                tail: b.tail.to_array(),
                radius: b.radius,
                weight: if def.bones.is_empty()
                    || def.bones.iter().any(|q| crate::name_matches(q, &b.name))
                {
                    1.0
                } else {
                    0.0
                },
            })
            .collect(),
        kind: def.motion.clone(),
        period,
        clip: Some(clip.to_string()),
    }
}

/// Builds the clip a `sway` item makes, over its base clip if any.
pub fn build(
    name: &str,
    def: &SwayDef,
    skel: &Skeleton,
    base: Option<Clip>,
) -> Result<Clip, String> {
    let ctx = format!("sway `{name}`");
    if let Some(b) = &base
        && !b.looping
    {
        return Err(format!(
            "{ctx}: over `{}` doesn't loop: a sway bakes a loop, so its driver must loop",
            b.name
        ));
    }
    let duration = def
        .duration
        .or(base.as_ref().map(|b| b.duration))
        .unwrap_or(4.0);
    if let Some(b) = &base {
        let cycles = duration / b.duration;
        if (cycles - cycles.round()).abs() > 1e-3 || cycles.round() < 1.0 {
            return Err(format!(
                "{ctx}: duration {duration} s is not a whole number of `{}` cycles ({} s each)",
                b.name, b.duration
            ));
        }
    }
    if !def.bones.is_empty()
        && !skel
            .bones
            .iter()
            .any(|b| def.bones.iter().any(|q| crate::name_matches(q, &b.name)))
    {
        return Err(format!(
            "{ctx}: bones {:?} match no bone of `{}`",
            def.bones, skel.name
        ));
    }
    let rig = rig_of(skel, def, Some(duration), name);
    let layer = match &def.motion {
        SwayKind::Wind(_) => Layer::Wind(rig),
        SwayKind::Spring(s) => {
            let steps = ((duration / SPRING_DT).round() as usize).max(1);
            let h = duration / steps as f32;
            let mut sim = SpringSim::new();
            let mut out = Vec::new();
            let mut frames: Vec<Vec<Quat>> = Vec::with_capacity(steps + 1);
            let driver = |t: f32| -> Vec<(Vec3, Quat)> {
                match &base {
                    // The simulation works in world-aligned frames.
                    Some(b) => b
                        .sample(skel, t)
                        .iter()
                        .zip(&skel.bones)
                        .map(|((t, r, _), bone)| (bone.rest * *t, to_world(bone.rest, *r)))
                        .collect(),
                    None => vec![(Vec3::ZERO, Quat::IDENTITY); skel.bones.len()],
                }
            };
            for lap in 0..3 {
                for k in 0..=steps {
                    if k == 0 && lap > 0 {
                        continue; // the previous lap's last step was this time
                    }
                    let t = k as f32 * h;
                    let anim = driver(t);
                    sim.step(&rig, s, &anim, lap as f32 * duration + t, h, &mut out);
                    if lap == 2 {
                        frames.push(out.clone());
                    }
                }
            }
            // The last lap still drifts a little: spread the seam's
            // mismatch over the loop so the end meets the start.
            if let (Some(first), Some(last)) = (frames.first().cloned(), frames.last().cloned()) {
                let n = frames.len().saturating_sub(1).max(1) as f32;
                for (i, f) in frames.iter_mut().enumerate() {
                    let u = i as f32 / n;
                    for (b, q) in f.iter_mut().enumerate() {
                        let fix = first[b] * last[b].inverse();
                        *q = (Quat::IDENTITY.slerp(fix, u) * *q).normalize();
                    }
                }
            }
            Layer::Spring(frames)
        }
    };
    let root_motion = base.as_ref().and_then(|b| b.root_motion.clone());
    Ok(Clip {
        base: base.map(Box::new),
        layer: Some(Arc::new(layer)),
        fps: def.fps,
        root_motion,
        ..Clip::empty(name, &def.skeleton, duration, true)
    })
}

/// Applies the layer at `t` (already wrapped into the clip) to `pose`.
/// A rotation in a bone's rest frame, as a world-aligned one.
fn to_world(rest: Quat, r: Quat) -> Quat {
    if rest == Quat::IDENTITY {
        r
    } else {
        (rest * r * rest.inverse()).normalize()
    }
}

/// A world-aligned rotation, in a bone's rest frame.
fn to_local(rest: Quat, r: Quat) -> Quat {
    if rest == Quat::IDENTITY {
        r
    } else {
        (rest.inverse() * r * rest).normalize()
    }
}

pub fn apply(layer: &Layer, skel: &Skeleton, duration: f32, t: f32, pose: &mut super::Pose) {
    let rest = |b: usize| skel.bones.get(b).map_or(Quat::IDENTITY, |x| x.rest);
    match layer {
        Layer::Wind(rig) => {
            let SwayKind::Wind(w) = &rig.kind else { return };
            let mut q = Vec::new();
            rig.wind_rotations(w, t, 0.0, &mut q);
            for (b, (slot, w)) in pose.iter_mut().zip(q).enumerate() {
                slot.1 = (to_local(rest(b), w) * slot.1).normalize();
            }
        }
        Layer::Spring(frames) => {
            if frames.is_empty() {
                return;
            }
            let n = frames.len() - 1;
            let x = if duration > 0.0 {
                (t / duration).clamp(0.0, 1.0) * n as f32
            } else {
                0.0
            };
            let i = (x.floor() as usize).min(n);
            let j = (i + 1).min(n);
            let u = x - i as f32;
            for (b, slot) in pose.iter_mut().enumerate() {
                if let (Some(a), Some(c)) = (frames[i].get(b), frames[j].get(b)) {
                    slot.1 = to_local(rest(b), a.slerp(*c, u).normalize());
                }
            }
        }
    }
}
