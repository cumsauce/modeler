//! Skeletons and animation: `skeleton` items (bones with world-aligned rest
//! frames), `bind` items (parts riding one bone rigidly, or skinned with
//! smooth weights) and `clip` items (keyframed or wave tracks per bone).
//!
//! Everything here is post-graph, like `move`: the geometry recipe stays
//! as it is, binds attach skin data to the finished parts, and one pose
//! evaluator ([`Clip::sample`], [`Skeleton::skin_matrices`],
//! [`skin::pose_part`]) serves agent views, verify, export and the window.
//!
//! Bone frames are world-aligned at rest: every bone's rest rotation is the
//! identity and its pivot is its `head`, so a clip rotation `{"x": 25}`
//! pitches the bone about world x from the rest pose, with no roll to think
//! about. Clip tracks are offsets from rest, composed down the tree.

pub mod check;
pub mod skin;

use std::collections::BTreeMap;

use bevy::math::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::{Model, ModelerError, Op, Part};

/// A point given as coordinates or as an anchor on a part's box
/// (`"leg_l:top"`, `"body:front-top"`), resolved on the finished parts so it
/// follows the geometry when params change.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum PointRef {
    Point([f32; 3]),
    Anchor(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BoneDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub head: PointRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<PointRef>,
    /// Envelope radius for smooth weights (default a fifth of the length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SkeletonDef {
    pub bones: Vec<BoneDef>,
    /// `"x"`: every bone named `*_l` gets a `*_r` twin mirrored across
    /// x = 0 (parents `*_l` map to `*_r` too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Paint {
    pub bone: String,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BindDef {
    pub skeleton: String,
    pub parts: Vec<String>,
    /// Rigid: the bone every matched part rides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// `rigid` or `smooth`.
    pub mode: String,
    /// Smooth: how wide (meters) weights blend across a joint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<f32>,
    /// Smooth: regions forced onto one bone before blending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paint: Vec<Paint>,
}

/// How values between keys are found.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    Step,
    Linear,
    /// Catmull-Rom through the keys.
    #[default]
    Smooth,
}

/// One animated channel of a bone: keys or a generated wave.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// (time, value): a quaternion for rotate, [x, y, z, 0] otherwise.
    Keys {
        keys: Vec<(f32, [f32; 4])>,
        #[serde(default)]
        interp: Interp,
    },
    /// `amount * sin(2 pi (t / period + phase))` along / about `axis`
    /// (degrees for rotate, meters for translate, a factor for scale).
    Wave {
        axis: [f32; 3],
        amount: f32,
        period: f32,
        #[serde(default)]
        phase: f32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct TrackDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<Channel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate: Option<Channel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Channel>,
}

/// A track copied from another bone, mirrored across x = 0 and shifted in
/// time by `phase` (a share of the clip): the right leg of a walk is the
/// left leg half a cycle later.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MirrorDef {
    pub from: String,
    #[serde(default)]
    pub phase: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClipDef {
    pub skeleton: String,
    pub duration: f32,
    #[serde(rename = "loop", default)]
    pub looping: bool,
    #[serde(default)]
    pub tracks: BTreeMap<String, TrackDef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mirror: BTreeMap<String, MirrorDef>,
}

/// A bone of a built skeleton (parents always come before children).
#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub head: Vec3,
    pub tail: Vec3,
    pub radius: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skeleton {
    pub name: String,
    pub bones: Vec<Bone>,
}

/// Local offsets from rest per bone: translation, rotation, scale.
pub type Pose = Vec<(Vec3, Quat, Vec3)>;

impl Skeleton {
    pub fn bone(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }

    pub fn rest(&self) -> Pose {
        vec![(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE); self.bones.len()]
    }

    /// World transform of every bone in `pose` (the rest frame of a bone is
    /// a translation to its head).
    pub fn world(&self, pose: &Pose) -> Vec<Mat4> {
        let mut out: Vec<Mat4> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            let (t, r, s) = pose
                .get(i)
                .copied()
                .unwrap_or((Vec3::ZERO, Quat::IDENTITY, Vec3::ONE));
            let rest_local = match b.parent {
                Some(p) => b.head - self.bones[p].head,
                None => b.head,
            };
            let local = Mat4::from_scale_rotation_translation(s, r, rest_local + t);
            out.push(match b.parent {
                Some(p) => out[p] * local,
                None => local,
            });
        }
        out
    }

    /// Per bone: world-in-pose times inverse-rest, the matrix that carries
    /// a rest-pose world point along with the bone.
    pub fn skin_matrices(&self, pose: &Pose) -> Vec<Mat4> {
        self.world(pose)
            .into_iter()
            .zip(&self.bones)
            .map(|(w, b)| w * Mat4::from_translation(-b.head))
            .collect()
    }

    /// Bone world head and tail in `pose`.
    pub fn posed_segments(&self, pose: &Pose) -> Vec<(Vec3, Vec3)> {
        self.skin_matrices(pose)
            .iter()
            .zip(&self.bones)
            .map(|(m, b)| (m.transform_point3(b.head), m.transform_point3(b.tail)))
            .collect()
    }
}

/// Finds a part's box for an anchor.
fn anchor(a: &str, parts: &[(String, std::sync::Arc<Part>)]) -> Result<Vec3, String> {
    let Some((target, words)) = a.split_once(':') else {
        return Err(format!(
            "`{a}`: an anchor names a part and words, like \"leg_l:top\""
        ));
    };
    let mut bx: Option<(Vec3, Vec3)> = None;
    for (n, p) in parts {
        if crate::name_matches(target, n) && !p.verts.is_empty() {
            let (lo, hi) = p.bounds();
            bx = Some(match bx {
                None => (lo, hi),
                Some((a, b)) => (a.min(lo), b.max(hi)),
            });
        }
    }
    let Some((lo, hi)) = bx else {
        return Err(format!("anchor `{a}`: no part `{target}`"));
    };
    crate::commands::anchor_point(words, lo, hi)
        .map(Vec3::from)
        .map_err(|e| format!("anchor `{a}`: {e}"))
}

fn point(p: &PointRef, parts: &[(String, std::sync::Arc<Part>)]) -> Result<Vec3, String> {
    match p {
        PointRef::Point(v) => Ok(Vec3::from(*v)),
        PointRef::Anchor(a) => anchor(a, parts),
    }
}

/// The `_l` -> `_r` twin name, if this is a left-side name.
pub fn mirror_name(n: &str) -> Option<String> {
    n.strip_suffix("_l")
        .map(|b| format!("{b}_r"))
        .or_else(|| n.strip_suffix(".l").map(|b| format!("{b}.r")))
}

/// Builds a skeleton from its item, resolving anchors on `parts`.
pub fn build(
    name: &str,
    def: &SkeletonDef,
    parts: &[(String, std::sync::Arc<Part>)],
) -> Result<Skeleton, String> {
    let ctx = format!("skeleton `{name}`");
    // name -> (parent, head, tail?, radius?)
    let mut raw: Vec<(String, Option<String>, Vec3, Option<Vec3>, Option<f32>)> = Vec::new();
    for b in &def.bones {
        let head =
            point(&b.head, parts).map_err(|e| format!("{ctx}: bone `{}`: head {e}", b.name))?;
        let tail = b
            .tail
            .as_ref()
            .map(|t| point(t, parts))
            .transpose()
            .map_err(|e| format!("{ctx}: bone `{}`: tail {e}", b.name))?;
        raw.push((b.name.clone(), b.parent.clone(), head, tail, b.radius));
    }
    match def.mirror.as_deref() {
        None => {}
        Some("x") => {
            let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
            let twins: Vec<_> = raw
                .iter()
                .filter_map(|(n, p, h, t, r)| {
                    let m = mirror_name(n)?;
                    let parent = p.as_ref().map(|p| mirror_name(p).unwrap_or(p.clone()));
                    Some((m, parent, flip(*h), t.map(flip), *r))
                })
                .collect();
            for t in twins {
                if raw.iter().any(|r| r.0 == t.0) {
                    return Err(format!(
                        "{ctx}: mirror makes `{}`, which is also listed: drop one",
                        t.0
                    ));
                }
                raw.push(t);
            }
        }
        Some(m) => return Err(format!("{ctx}: mirror `{m}`: only \"x\" is supported")),
    }
    // Parents first.
    let mut order: Vec<usize> = Vec::new();
    let mut placed = vec![false; raw.len()];
    while order.len() < raw.len() {
        let before = order.len();
        for i in 0..raw.len() {
            if placed[i] {
                continue;
            }
            let ready = match &raw[i].1 {
                None => true,
                Some(p) => {
                    let Some(j) = raw.iter().position(|r| &r.0 == p) else {
                        return Err(format!("{ctx}: bone `{}`: no parent bone `{p}`", raw[i].0));
                    };
                    placed[j]
                }
            };
            if ready {
                placed[i] = true;
                order.push(i);
            }
        }
        if order.len() == before {
            return Err(format!("{ctx}: bone parents form a loop"));
        }
    }
    let mut bones: Vec<Bone> = Vec::new();
    for &i in &order {
        let (n, p, h, _, _) = &raw[i];
        if bones.iter().any(|b| &b.name == n) {
            return Err(format!("{ctx}: bone `{n}` is listed twice"));
        }
        let parent = p
            .as_ref()
            .and_then(|p| bones.iter().position(|b| &b.name == p));
        bones.push(Bone {
            name: n.clone(),
            parent,
            head: *h,
            tail: *h,
            radius: 0.0,
        });
    }
    // Tails: given, else the mean of the children's heads, else the
    // parent direction continued (a leaf bone), else up.
    for (k, &i) in order.iter().enumerate() {
        let given = raw[i].3;
        let head = bones[k].head;
        let kids: Vec<Vec3> = bones
            .iter()
            .filter(|b| b.parent == Some(k))
            .map(|b| b.head)
            .collect();
        let tail = given.unwrap_or_else(|| {
            if !kids.is_empty() {
                kids.iter().copied().sum::<Vec3>() / kids.len() as f32
            } else if let Some(p) = bones[k].parent {
                let d = head - bones[p].head;
                if d.length() > 1e-6 {
                    head + d * 0.5
                } else {
                    head + Vec3::Y * 0.1
                }
            } else {
                head + Vec3::Y * 0.1
            }
        });
        bones[k].tail = tail;
        let len = (tail - head).length();
        bones[k].radius = raw[i].4.unwrap_or((len * 0.2).max(1e-3));
        if bones[k].radius.is_nan() || bones[k].radius <= 0.0 {
            return Err(format!(
                "{ctx}: bone `{}`: radius must be positive",
                bones[k].name
            ));
        }
    }
    if bones.is_empty() {
        return Err(format!("{ctx}: needs at least one bone"));
    }
    if bones.len() > u16::MAX as usize {
        return Err(format!("{ctx}: too many bones"));
    }
    Ok(Skeleton {
        name: name.to_string(),
        bones,
    })
}

/// A built clip: per bone, the sampled channels.
#[derive(Clone, Debug)]
pub struct Clip {
    pub name: String,
    pub skeleton: String,
    pub duration: f32,
    pub looping: bool,
    /// (bone index, track, mirrored?, time shift)
    pub tracks: Vec<(usize, TrackDef, bool, f32)>,
}

impl Clip {
    /// The pose at time `t` (wrapped when looping, clamped otherwise).
    pub fn sample(&self, skel: &Skeleton, t: f32) -> Pose {
        let mut pose = skel.rest();
        for (bone, track, mirrored, shift) in &self.tracks {
            let tt = self.wrap(t + shift);
            let d = self.duration;
            let lp = self.looping;
            let mut r = track
                .rotate
                .as_ref()
                .map(|c| rot_at(c, tt, d, lp))
                .unwrap_or(Quat::IDENTITY);
            let mut tr = track
                .translate
                .as_ref()
                .map(|c| vec_at(c, tt, d, lp, 0.0))
                .unwrap_or(Vec3::ZERO);
            let s = track
                .scale
                .as_ref()
                .map(|c| vec_at(c, tt, d, lp, 1.0))
                .unwrap_or(Vec3::ONE);
            if *mirrored {
                r = Quat::from_xyzw(r.x, -r.y, -r.z, r.w);
                tr.x = -tr.x;
            }
            if let Some(slot) = pose.get_mut(*bone) {
                *slot = (tr, r, s);
            }
        }
        pose
    }

    pub fn wrap(&self, t: f32) -> f32 {
        if self.duration <= 0.0 {
            return 0.0;
        }
        if self.looping {
            t.rem_euclid(self.duration)
        } else {
            t.clamp(0.0, self.duration)
        }
    }
}

/// Keys around `t`, with wrapped neighbours on a loop: (k0, k1, k2, k3, u).
fn around(
    keys: &[(f32, [f32; 4])],
    t: f32,
    d: f32,
    looping: bool,
) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4], f32) {
    let n = keys.len();
    if n == 1 {
        let v = keys[0].1;
        return (v, v, v, v, 0.0);
    }
    // A loop's last key at t = duration repeats the first: leave it out of
    // the neighbours so the curve passes through the seam smoothly.
    let periodic =
        looping && d > 0.0 && (keys[n - 1].0 - d).abs() < 1e-4 && (keys[0].0).abs() < 1e-4;
    let i = match keys.iter().position(|k| k.0 > t) {
        Some(0) => {
            let v = keys[0].1;
            return (v, v, v, v, 0.0);
        }
        Some(i) => i - 1,
        None => {
            let v = keys[n - 1].1;
            return (v, v, v, v, 0.0);
        }
    };
    let (a, b) = (keys[i], keys[i + 1]);
    let u = if b.0 > a.0 {
        ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let prev = if i > 0 {
        keys[i - 1].1
    } else if periodic && n > 2 {
        keys[n - 2].1
    } else {
        a.1
    };
    let next = if i + 2 < n {
        keys[i + 2].1
    } else if periodic && n > 2 {
        keys[1].1
    } else {
        b.1
    };
    (prev, a.1, b.1, next, u)
}

fn catmull(p0: f32, p1: f32, p2: f32, p3: f32, u: f32) -> f32 {
    let u2 = u * u;
    let u3 = u2 * u;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * u
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * u3)
}

fn wave(axis: [f32; 3], amount: f32, period: f32, phase: f32, t: f32) -> (Vec3, f32) {
    let a = Vec3::from(axis).normalize_or_zero();
    let s = if period > 0.0 {
        (std::f32::consts::TAU * (t / period + phase)).sin()
    } else {
        0.0
    };
    (a, amount * s)
}

fn rot_at(c: &Channel, t: f32, d: f32, looping: bool) -> Quat {
    match c {
        Channel::Wave {
            axis,
            amount,
            period,
            phase,
        } => {
            let (a, v) = wave(*axis, *amount, *period, *phase, t);
            if a == Vec3::ZERO {
                Quat::IDENTITY
            } else {
                Quat::from_axis_angle(a, v.to_radians())
            }
        }
        Channel::Keys { keys, interp } => {
            if keys.is_empty() {
                return Quat::IDENTITY;
            }
            let (k0, k1, k2, k3, u) = around(keys, t, d, looping);
            let q = |v: [f32; 4]| Quat::from_array(v).normalize();
            let q1 = q(k1);
            // Align signs so interpolation takes the short way.
            let align = |v: Quat| if v.dot(q1) < 0.0 { -v } else { v };
            let (q0, q2, q3) = (align(q(k0)), align(q(k2)), align(q(k3)));
            match interp {
                Interp::Step => q1,
                Interp::Linear => q1.slerp(q2, u),
                Interp::Smooth => {
                    let c = |i: usize| {
                        catmull(
                            q0.to_array()[i],
                            q1.to_array()[i],
                            q2.to_array()[i],
                            q3.to_array()[i],
                            u,
                        )
                    };
                    Quat::from_xyzw(c(0), c(1), c(2), c(3)).normalize()
                }
            }
        }
    }
}

fn vec_at(c: &Channel, t: f32, d: f32, looping: bool, base: f32) -> Vec3 {
    match c {
        Channel::Wave {
            axis,
            amount,
            period,
            phase,
        } => {
            let (a, v) = wave(*axis, *amount, *period, *phase, t);
            Vec3::splat(base) + a * v
        }
        Channel::Keys { keys, interp } => {
            if keys.is_empty() {
                return Vec3::splat(base);
            }
            let (k0, k1, k2, k3, u) = around(keys, t, d, looping);
            let v = |k: [f32; 4]| Vec3::new(k[0], k[1], k[2]);
            match interp {
                Interp::Step => v(k1),
                Interp::Linear => v(k1).lerp(v(k2), u),
                Interp::Smooth => Vec3::new(
                    catmull(k0[0], k1[0], k2[0], k3[0], u),
                    catmull(k0[1], k1[1], k2[1], k3[1], u),
                    catmull(k0[2], k1[2], k2[2], k3[2], u),
                ),
            }
        }
    }
}

/// Builds a clip against its skeleton.
pub fn build_clip(name: &str, def: &ClipDef, skel: &Skeleton) -> Result<Clip, String> {
    let ctx = format!("clip `{name}`");
    if def.duration.is_nan() || def.duration <= 0.0 {
        return Err(format!("{ctx}: duration must be positive"));
    }
    let mut tracks = Vec::new();
    for (bone, track) in &def.tracks {
        let Some(i) = skel.bone(bone) else {
            return Err(format!(
                "{ctx}: track `{bone}`: skeleton `{}` has no such bone (bones: {})",
                skel.name,
                bone_list(skel)
            ));
        };
        tracks.push((i, track.clone(), false, 0.0));
    }
    for (bone, m) in &def.mirror {
        let Some(i) = skel.bone(bone) else {
            return Err(format!(
                "{ctx}: mirror `{bone}`: no such bone (bones: {})",
                bone_list(skel)
            ));
        };
        let Some(src) = def.tracks.get(&m.from) else {
            return Err(format!(
                "{ctx}: mirror `{bone}`: no track `{}` to copy",
                m.from
            ));
        };
        if def.tracks.contains_key(bone) {
            return Err(format!(
                "{ctx}: `{bone}` has a track and a mirror: drop one"
            ));
        }
        tracks.push((i, src.clone(), true, m.phase * def.duration));
    }
    Ok(Clip {
        name: name.to_string(),
        skeleton: def.skeleton.clone(),
        duration: def.duration,
        looping: def.looping,
        tracks,
    })
}

fn bone_list(s: &Skeleton) -> String {
    let names: Vec<&str> = s.bones.iter().map(|b| b.name.as_str()).take(16).collect();
    let more = s.bones.len().saturating_sub(16);
    if more > 0 {
        format!("{}, +{more} more", names.join(", "))
    } else {
        names.join(", ")
    }
}

/// How a part is bound to a skeleton (derived by `bind`, never saved).
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub skeleton: String,
    pub bind: String,
    /// Rigid: the bone it rides (`joints`/`weights` empty).
    pub bone: Option<usize>,
    /// Smooth: up to four bones per vertex and their weights (sum 1).
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
}

/// Every skeleton of the model, built on its finished parts.
pub fn skeletons(model: &Model) -> Result<Vec<Skeleton>, ModelerError> {
    let named: Vec<(String, std::sync::Arc<Part>)> = model
        .parts
        .iter()
        .map(|p| (p.name.clone(), p.clone()))
        .collect();
    let mut out = Vec::new();
    for op in &model.ops {
        if let Op::Skeleton { name, def } = op {
            out.push(build(name, def, &named)?);
        }
    }
    Ok(out)
}

/// Every clip of the model with its skeleton.
pub fn clips(model: &Model) -> Result<Vec<(Clip, Skeleton)>, ModelerError> {
    let skels = skeletons(model)?;
    let mut out = Vec::new();
    for op in &model.ops {
        if let Op::Clip { name, def } = op {
            let Some(s) = skels.iter().find(|s| s.name == def.skeleton) else {
                return Err(format!("clip `{name}`: no skeleton `{}`", def.skeleton).into());
            };
            out.push((build_clip(name, def, s)?, s.clone()));
        }
    }
    Ok(out)
}

/// The named clip (or, with `None`, the only clip) and its skeleton.
pub fn clip(model: &Model, name: Option<&str>) -> Result<(Clip, Skeleton), ModelerError> {
    let all = clips(model)?;
    let names: Vec<String> = all.iter().map(|(c, _)| c.name.clone()).collect();
    match name {
        Some(n) => all.into_iter().find(|(c, _)| c.name == n).ok_or_else(|| {
            format!(
                "no clip `{n}` (clips: {})",
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            )
            .into()
        }),
        None => {
            if all.len() == 1 {
                Ok(all.into_iter().next().ok_or("no clip")?)
            } else if all.is_empty() {
                Err("the model has no clip: add a `clip` item".into())
            } else {
                Err(format!("several clips ({}): name one with `clip`", names.join(", ")).into())
            }
        }
    }
}

/// Bone world segments at `t` of a clip.
pub fn posed_segments_at(clip: &Clip, skel: &Skeleton, t: f32) -> Vec<(Vec3, Vec3)> {
    skel.posed_segments(&clip.sample(skel, t))
}

/// Every part posed at `t` of `clip`: bound parts are skinned (smooth) or
/// carried (rigid) into world space; others are returned as they are.
pub fn posed_parts(
    model: &Model,
    clip: &Clip,
    skel: &Skeleton,
    t: f32,
) -> Vec<std::sync::Arc<Part>> {
    let pose = clip.sample(skel, t);
    let mats = skel.skin_matrices(&pose);
    model
        .parts
        .iter()
        .map(|p| match &p.rig {
            Some(b) if b.skeleton == skel.name => std::sync::Arc::new(skin::pose_part(p, b, &mats)),
            _ => p.clone(),
        })
        .collect()
}

/// After the graph: builds skeletons and applies every bind to the
/// finished parts (in place on `named`).
pub fn apply_binds(
    ops: &[Op],
    named: &mut [(String, std::sync::Arc<Part>)],
) -> Result<(), ModelerError> {
    if !ops
        .iter()
        .any(|o| matches!(o, Op::Bind { .. } | Op::Skeleton { .. } | Op::Clip { .. }))
    {
        return Ok(());
    }
    let mut skels: Vec<Skeleton> = Vec::new();
    for op in ops {
        if let Op::Skeleton { name, def } = op {
            skels.push(build(name, def, named)?);
        }
    }
    // Clips are checked here so a bad track fails (and rolls back) the batch.
    for op in ops {
        if let Op::Clip { name, def } = op {
            let Some(s) = skels.iter().find(|s| s.name == def.skeleton) else {
                return Err(format!("clip `{name}`: no skeleton `{}`", def.skeleton).into());
            };
            build_clip(name, def, s)?;
        }
    }
    for op in ops {
        let Op::Bind { name, def } = op else { continue };
        let ctx = format!("bind `{name}`");
        let Some(skel) = skels.iter().find(|s| s.name == def.skeleton) else {
            return Err(format!("{ctx}: no skeleton `{}`", def.skeleton).into());
        };
        let mut hit = 0;
        for (n, part) in named.iter_mut() {
            if !def.parts.iter().any(|q| crate::name_matches(q, n)) {
                continue;
            }
            hit += 1;
            let binding = match def.mode.as_str() {
                "rigid" => {
                    let bone = def
                        .bone
                        .as_deref()
                        .ok_or_else(|| format!("{ctx}: rigid needs `bone`"))?;
                    let Some(i) = skel.bone(bone) else {
                        return Err(format!(
                            "{ctx}: no bone `{bone}` (bones: {})",
                            bone_list(skel)
                        )
                        .into());
                    };
                    Binding {
                        skeleton: skel.name.clone(),
                        bind: name.clone(),
                        bone: Some(i),
                        joints: Vec::new(),
                        weights: Vec::new(),
                    }
                }
                _ => skin::smooth_weights(part, skel, def, name)
                    .map_err(|e| format!("{ctx}: {e}"))?,
            };
            std::sync::Arc::make_mut(part).rig = Some(std::sync::Arc::new(binding));
        }
        if hit == 0 {
            return Err(format!("{ctx}: no part matches {:?}", def.parts).into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm() -> Skeleton {
        let def = SkeletonDef {
            bones: vec![
                BoneDef {
                    name: "upper".into(),
                    parent: None,
                    head: PointRef::Point([0.0, 0.0, 0.0]),
                    tail: None,
                    radius: None,
                },
                BoneDef {
                    name: "lower".into(),
                    parent: Some("upper".into()),
                    head: PointRef::Point([1.0, 0.0, 0.0]),
                    tail: Some(PointRef::Point([2.0, 0.0, 0.0])),
                    radius: None,
                },
            ],
            mirror: None,
        };
        build("arm", &def, &[]).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parent_rotation_moves_child_head() {
        let s = arm();
        assert!(
            (s.bones[0].tail - Vec3::X).length() < 1e-5,
            "tail defaults to the child's head"
        );
        let mut pose = s.rest();
        pose[0].1 = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let seg = s.posed_segments(&pose);
        assert!((seg[1].0 - Vec3::Y).length() < 1e-5, "{:?}", seg[1]);
        assert!((seg[1].1 - Vec3::Y * 2.0).length() < 1e-5);
    }

    #[test]
    fn items_bind_and_pose_through_a_session() {
        let mut s = crate::commands::Session::default();
        let items = serde_json::json!([
            {"type": "primitive", "name": "arm", "kind": "box", "size": [1.0, 0.1, 0.1], "translation": [0.5, 1.0, 0.0]},
            {"type": "primitive", "name": "hand", "kind": "box", "size": [0.1, 0.1, 0.1], "translation": [1.05, 1.0, 0.0]},
            {"type": "skeleton", "name": "rig", "bones": {
                "shoulder": {"head": "arm:left", "tail": "arm:right"}}},
            {"type": "bind", "name": "b_arm", "skeleton": "rig", "parts": ["arm"], "mode": "smooth"},
            {"type": "bind", "name": "b_hand", "skeleton": "rig", "parts": ["hand"], "bone": "shoulder"},
            {"type": "clip", "name": "raise", "skeleton": "rig", "duration": 1.0, "loop": false,
             "tracks": {"shoulder": {"rotate": [[0, {"z": 0}], [1, {"z": 90}]]}}}
        ]);
        let serde_json::Value::Array(items) = items else {
            panic!()
        };
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        let arm = s
            .model
            .parts
            .iter()
            .find(|p| p.name == "arm")
            .unwrap_or_else(|| panic!("no arm"));
        assert!(
            arm.rig
                .as_ref()
                .is_some_and(|b| b.weights.len() == arm.verts.len())
        );
        let (walk, skel) = clip(&s.model, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(skel.bones[0].head, Vec3::new(0.0, 1.0, 0.0));
        let posed = posed_parts(&s.model, &walk, &skel, 1.0);
        let hand = posed
            .iter()
            .find(|p| p.name == "hand")
            .unwrap_or_else(|| panic!("no hand"));
        let (lo, hi) = hand.bounds();
        let c = (lo + hi) * 0.5;
        // The hand swung from x 1.05 to straight up over the shoulder.
        assert!((c - Vec3::new(0.0, 2.05, 0.0)).length() < 1e-3, "{c:?}");
        // A bad track names the bones.
        let bad = serde_json::json!([{"type": "clip", "name": "raise", "skeleton": "rig", "duration": 1.0,
            "tracks": {"elbow": {"rotate": [[0, {"z": 0}]]}}}]);
        let serde_json::Value::Array(bad) = bad else {
            panic!()
        };
        let Err(err) = s.apply_values(None, bad) else {
            panic!("bad clip applied")
        };
        let err = err.to_string();
        assert!(
            err.contains("no such bone") && err.contains("shoulder"),
            "{err}"
        );
        assert!(
            clip(&s.model, Some("raise")).is_ok(),
            "rolled back to the good clip"
        );
    }

    #[test]
    fn mirror_makes_twins() {
        let def = SkeletonDef {
            bones: vec![
                BoneDef {
                    name: "hip".into(),
                    parent: None,
                    head: PointRef::Point([0.0, 1.0, 0.0]),
                    tail: None,
                    radius: None,
                },
                BoneDef {
                    name: "leg_l".into(),
                    parent: Some("hip".into()),
                    head: PointRef::Point([0.2, 1.0, 0.0]),
                    tail: Some(PointRef::Point([0.2, 0.0, 0.0])),
                    radius: None,
                },
            ],
            mirror: Some("x".into()),
        };
        let s = build("r", &def, &[]).unwrap_or_else(|e| panic!("{e}"));
        let r = s.bone("leg_r").unwrap_or_else(|| panic!("no leg_r"));
        assert!((s.bones[r].head.x + 0.2).abs() < 1e-6);
        assert_eq!(s.bones[r].parent, s.bone("hip"));
    }

    #[test]
    fn clip_keys_and_mirror_phase() {
        let s = arm();
        let q = |deg: f32| Quat::from_rotation_x(deg.to_radians()).to_array();
        let mut tracks = BTreeMap::new();
        tracks.insert(
            "upper".to_string(),
            TrackDef {
                rotate: Some(Channel::Keys {
                    keys: vec![(0.0, q(0.0)), (0.5, q(40.0)), (1.0, q(0.0))],
                    interp: Interp::Linear,
                }),
                ..Default::default()
            },
        );
        let mut mirror = BTreeMap::new();
        mirror.insert(
            "lower".to_string(),
            MirrorDef {
                from: "upper".into(),
                phase: 0.5,
            },
        );
        let def = ClipDef {
            skeleton: "arm".into(),
            duration: 1.0,
            looping: true,
            tracks,
            mirror,
        };
        let c = build_clip("swing", &def, &s).unwrap_or_else(|e| panic!("{e}"));
        let p = c.sample(&s, 0.25);
        assert!(
            (p[0]
                .1
                .angle_between(Quat::from_rotation_x(20f32.to_radians())))
                < 1e-3
        );
        // lower = upper at t + 0.5 = 0.75 -> 20 deg, mirrored (x-axis rotation keeps its sign).
        assert!(
            (p[1]
                .1
                .angle_between(Quat::from_rotation_x(20f32.to_radians())))
                < 1e-3
        );
        let p0 = c.sample(&s, 0.0);
        let p1 = c.sample(&s, 1.0);
        assert!(p0[0].1.angle_between(p1[0].1) < 1e-4, "loop seam");
        // Smooth interpolation passes through the keys.
        let mut def2 = def.clone();
        if let Some(Channel::Keys { interp, .. }) =
            def2.tracks.get_mut("upper").and_then(|t| t.rotate.as_mut())
        {
            *interp = Interp::Smooth;
        }
        let c2 = build_clip("s", &def2, &s).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            c2.sample(&s, 0.5)[0]
                .1
                .angle_between(Quat::from_rotation_x(40f32.to_radians()))
                < 1e-3
        );
    }
}
