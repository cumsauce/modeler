//! Skeletons and animation: `skeleton` items (bones with world-aligned rest
//! frames), `bind` items (parts riding one bone rigidly, or skinned with
//! smooth weights) and `clip` items (keyframed or wave tracks per bone).
//!
//! Everything here is post-graph, like `move`: the geometry recipe stays
//! as it is, binds attach skin data to the finished parts, and one pose
//! evaluator ([`Clip::sample`], [`Skeleton::skin_matrices`],
//! [`skin::pose_part`]) serves agent views, verify, export and the window.
//!
//! Bone frames are world-aligned at rest by default: a bone's rest rotation
//! is the identity and its pivot is its `head`, so a clip rotation
//! `{"x": 25}` pitches the bone about world x from the rest pose, with no
//! roll to think about. A bone may set a rest rotation (`rest`, or `roll`
//! about its head -> tail axis with the frame's +y along the bone); its
//! clip channels are then in that rest frame (local axes). Clip tracks are
//! offsets from rest, composed down the tree.

pub mod auto;
pub mod check;
#[cfg(test)]
mod clip_tests;
pub mod ik;
pub mod skin;
pub mod sway;

pub use sway::SwayDef;

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
    /// Rest rotation of the bone's frame in world space (a quat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<[f32; 4]>,
    /// Degrees about head -> tail, with the frame's +y along the bone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roll: Option<f32>,
    /// Joint limits (degrees, in the bone's rest frame).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
    /// Aim the bone at a point, anchor or bone after IK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_at: Option<LookAtDef>,
}

/// Joint limits of a bone: degree ranges for the Euler angles of its clip
/// rotation (the offset from rest, in the bone's rest frame), decomposed
/// in the order rotations `{x, y, z}` are written (x, then y, then z).
/// An axis left out is free.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<[f32; 2]>,
}

impl Limits {
    /// The limits of the x-mirrored twin: x stays, y and z flip.
    pub fn mirrored(&self) -> Limits {
        let f = |r: Option<[f32; 2]>| r.map(|[a, b]| [-b, -a]);
        Limits {
            x: self.x,
            y: f(self.y),
            z: f(self.z),
        }
    }
}

/// `look_at` on a bone: a point, a part anchor (`"ball:center"`) or a bone
/// name (its posed head).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LookAtDef {
    pub target: PointRef,
    /// The bone's aiming axis in its rest frame (default head -> tail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
}

/// A built `look_at`.
#[derive(Clone, Debug, PartialEq)]
pub struct LookAt {
    pub target: LookTarget,
    /// Unit axis in the bone's rest frame.
    pub axis: Vec3,
    pub weight: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LookTarget {
    Point(Vec3),
    Bone(usize),
}

/// Where an IK entry's end goes: a point, a part anchor, or keyed points
/// (a translate-like channel, world meters).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum IkTarget {
    Point([f32; 3]),
    Anchor(String),
    Keys(Channel),
}

/// One `ik` entry of a clip, keyed by the end bone: that bone's HEAD is
/// driven to `target` by bending the `chain` bones above it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IkDef {
    pub target: IkTarget,
    #[serde(default = "two")]
    pub chain: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pole: Option<PointRef>,
    /// 0..1, FK to IK (x of a scale-like channel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<Channel>,
    /// The end bone's world rotation as an offset from its rest frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<Channel>,
}

fn two() -> u32 {
    2
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SkeletonDef {
    pub bones: Vec<BoneDef>,
    /// `"x"`: every bone named `*_l` gets a `*_r` twin mirrored across
    /// x = 0 (parents `*_l` map to `*_r` too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
    /// Auto-rig from this op (an lsystem or a skin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone_length: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bones: Option<u32>,
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
    /// Smooth: take each vertex's weights from the closest point of this
    /// (bound) part's surface: clothing over a body (`skin::transfer_weights`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights_from: Option<String>,
    /// Smooth: pose-driven corrective shapes (`skin::correctives`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correctives: Option<skin::CorrectivesDef>,
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
    /// Rotate only: (time, [x, y, z, 0]) Euler degrees, interpolated as
    /// plain numbers and composed x, then y, then z about the rest frame's
    /// axes (as the `{x, y, z}` rotation form), so keys may run past 180
    /// degrees: a full spin is `[[0, {y: 0}], [1, {y: 360}]]`.
    Euler {
        keys: Vec<(f32, [f32; 4])>,
        #[serde(default)]
        interp: Interp,
    },
}

/// Euler degrees `[x, y, z]` composed x, then y, then z.
pub fn euler_quat(e: Vec3) -> Quat {
    (Quat::from_rotation_z(e.z.to_radians())
        * Quat::from_rotation_y(e.y.to_radians())
        * Quat::from_rotation_x(e.x.to_radians()))
    .normalize()
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct ClipDef {
    pub skeleton: String,
    pub duration: f32,
    #[serde(rename = "loop", default)]
    pub looping: bool,
    #[serde(default)]
    pub tracks: BTreeMap<String, TrackDef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mirror: BTreeMap<String, MirrorDef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ik: BTreeMap<String, IkDef>,
    /// Whole-pose keys `(time, pose name)` (`"rest"` is built in).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poses: Vec<(f32, String)>,
    /// Interpolation of the pose keys (the clip's `interp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interp: Option<Interp>,
    /// A keyed clip of the same skeleton this one layers over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
    /// With `over`: tracks are offsets on top of the base's pose
    /// (rotations compose, translations add, scales multiply) instead of
    /// replacing it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub additive: bool,
    /// With `over`: how much of this layer applies (x of a scale-like
    /// channel, 0..1; default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<Channel>,
    /// With `over`: the bones the layer touches (`*` globs; default all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bones: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_motion: Option<RootMotionDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<ClipEvent>,
    /// Morph target weights: part -> shape -> channel (the weight in x;
    /// keys, a constant or a wave).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub morphs: BTreeMap<String, BTreeMap<String, Channel>>,
    /// A `mocap` item: the tracks come from a BVH file, retargeted onto
    /// the skeleton when the clip builds (`crate::mocap::lower`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mocap: Option<Box<crate::mocap::MocapDef>>,
}

/// `root_motion` of a clip: the bone whose translation on `axes` (and,
/// with `yaw`, whose turn about world y) is the character's travel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RootMotionDef {
    pub bone: String,
    pub axes: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub yaw: bool,
}

/// A named moment of a clip (a footstep), with optional game data.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClipEvent {
    pub time: f32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// One bone of a `pose`: offsets from rest (as clip track values).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct PoseBone {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 3]>,
}

/// A named static pose of a skeleton, keyable by clips (`poses`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PoseDef {
    pub skeleton: String,
    #[serde(default)]
    pub bones: BTreeMap<String, PoseBone>,
    /// Fill `_r` bones from their `_l` twins (and back) where not given.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror: bool,
    /// IK solved once when the pose is built (targets are points or
    /// anchors).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ik: BTreeMap<String, IkDef>,
    /// Morph target weights: part -> shape -> weight (an expression).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub morphs: BTreeMap<String, BTreeMap<String, f32>>,
}

/// A built pose: per touched bone, the channels it sets.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltPose {
    pub name: String,
    pub skeleton: String,
    pub bones: Vec<(usize, Option<Quat>, Option<Vec3>, Option<Vec3>)>,
    /// (part, shape, shape index, weight).
    pub morphs: Vec<(String, String, usize, f32)>,
}

/// One animated morph target weight of a built clip.
#[derive(Clone, Debug, PartialEq)]
pub struct MorphTrack {
    pub part: String,
    pub shape: String,
    /// The shape's index in the part's `morphs`.
    pub index: usize,
    /// The weight in x.
    pub channel: Channel,
}

/// A built root motion: bone index, world axes, yaw.
#[derive(Clone, Debug, PartialEq)]
pub struct RootMotion {
    pub bone: usize,
    pub axes: [bool; 3],
    pub yaw: bool,
}

/// A built layer's blend (`over` on a keyed clip).
#[derive(Clone, Debug, PartialEq)]
pub struct Layering {
    pub additive: bool,
    pub weight: Option<Channel>,
}

/// A bone of a built skeleton (parents always come before children).
#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub head: Vec3,
    pub tail: Vec3,
    pub radius: f32,
    /// Rest rotation of the bone's frame in world space (identity =
    /// world-aligned, the default).
    pub rest: Quat,
    pub limits: Option<Limits>,
    pub look_at: Option<LookAt>,
}

impl Bone {
    /// The inverse of the bone's rest world matrix.
    pub fn inverse_bind(&self) -> Mat4 {
        if self.rest == Quat::IDENTITY {
            Mat4::from_translation(-self.head)
        } else {
            Mat4::from_quat(self.rest.inverse()) * Mat4::from_translation(-self.head)
        }
    }

    /// The rest frame relative to the parent's rest frame: translation and
    /// rotation (the identity rotation when neither has a rest rotation).
    pub fn rest_local(&self, bones: &[Bone]) -> (Vec3, Quat) {
        match self.parent {
            Some(p) => {
                let pb = &bones[p];
                let d = self.head - pb.head;
                if pb.rest == Quat::IDENTITY {
                    (d, self.rest)
                } else {
                    let inv = pb.rest.inverse();
                    (inv * d, (inv * self.rest).normalize())
                }
            }
            None => (self.head, self.rest),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skeleton {
    pub name: String,
    pub bones: Vec<Bone>,
    /// Exact weights when the skeleton was grown `from` an lsystem.
    pub auto: Option<std::sync::Arc<auto::AutoMap>>,
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
            let (rest_local, rl) = b.rest_local(&self.bones);
            let local = if rl == Quat::IDENTITY {
                Mat4::from_scale_rotation_translation(s, r, rest_local + t)
            } else {
                Mat4::from_scale_rotation_translation(s, (rl * r).normalize(), rest_local + rl * t)
            };
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
            .map(|(w, b)| w * b.inverse_bind())
            .collect()
    }

    /// Whether any bone has limits or a look_at.
    pub fn constrained(&self) -> bool {
        self.bones
            .iter()
            .any(|b| b.limits.is_some() || b.look_at.is_some())
    }

    /// Whether any bone has a rest rotation.
    pub fn has_rest(&self) -> bool {
        self.bones.iter().any(|b| b.rest != Quat::IDENTITY)
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
    if let Some(j) = parts
        .iter()
        .find(|(n, _)| n == target)
        .and_then(|(_, p)| p.joint(words))
    {
        return Ok(j);
    }
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

/// A bone item's rest rotation: `rest` (world), then `roll` degrees about
/// head -> tail with +y along the bone.
fn rest_of(b: &BoneDef, head: Vec3, tail: Vec3) -> Result<Quat, String> {
    let mut q = match b.rest {
        Some(r) => {
            let q = Quat::from_array(r);
            if !q.is_finite() || q.length() < 1e-6 {
                return Err("rest: not a rotation".into());
            }
            q.normalize()
        }
        None => Quat::IDENTITY,
    };
    if let Some(roll) = b.roll {
        let dir = q.inverse() * (tail - head);
        let Some(dir) = dir.try_normalize() else {
            return Err("roll needs a bone with length (head and tail apart)".into());
        };
        q = (q * Quat::from_rotation_arc(Vec3::Y, dir) * Quat::from_rotation_y(roll.to_radians()))
            .normalize();
    }
    Ok(q)
}

/// The `_l` -> `_r` twin name, if this is a left-side name.
pub fn mirror_name(n: &str) -> Option<String> {
    n.strip_suffix("_l")
        .map(|b| format!("{b}_r"))
        .or_else(|| n.strip_suffix(".l").map(|b| format!("{b}.r")))
}

/// The other side's name of a `_l` / `_r` (`.l` / `.r`) bone.
fn twin_name(n: &str) -> Option<String> {
    mirror_name(n).or_else(|| {
        n.strip_suffix("_r")
            .map(|b| format!("{b}_l"))
            .or_else(|| n.strip_suffix(".r").map(|b| format!("{b}.l")))
    })
}

/// Builds a skeleton from its item, resolving anchors on `parts` (and a
/// `from` auto-rig on `parts` / `ops`).
pub fn build(
    name: &str,
    def: &SkeletonDef,
    parts: &[(String, std::sync::Arc<Part>)],
    ops: &[Op],
) -> Result<Skeleton, String> {
    let ctx = format!("skeleton `{name}`");
    // name -> (parent, head, tail?, radius?)
    let mut raw: Vec<auto::RawBone> = Vec::new();
    let mut mirror = def.mirror.clone();
    // lsystem rigs: per chain point, (generated bone index, weight) pairs.
    let mut lsys: Option<(String, Vec<Vec<[(usize, f32); 2]>>)> = None;
    if let Some(from) = &def.from {
        let skin = ops.iter().find_map(|o| match o {
            Op::Skin { name: n, def } if n == from => Some(def),
            _ => None,
        });
        let body = ops.iter().find_map(|o| match o {
            Op::Body { name: n, def } if n == from => Some(def),
            _ => None,
        });
        if let Some(bd) = body {
            let bones =
                crate::body::rig_bones(bd).map_err(|e| format!("{ctx}: from `{from}`: {e}"))?;
            // Follow the body part's own transform (a move, a transform).
            let part = parts
                .iter()
                .find(|(n, _)| n == from)
                .map(|(_, p)| p.clone());
            let world = |v: Vec3| match &part {
                Some(p) => {
                    crate::quat_of(p.rotation) * (v * Vec3::from(p.scale))
                        + Vec3::from(p.translation)
                }
                None => v,
            };
            raw.extend(
                bones
                    .into_iter()
                    .map(|(n, p, h, t, r)| (n, p, world(h), t.map(world), r)),
            );
        } else if let Some(sd) = skin {
            let (bones, mirrored) =
                auto::from_skin(from, sd).map_err(|e| format!("{ctx}: from {e}"))?;
            raw.extend(bones);
            if mirrored {
                mirror = Some("x".into());
            }
        } else {
            let part = parts.iter().find(|(n, _)| n == from).map(|(_, p)| p);
            let Some(br) = part.and_then(|p| p.branches.as_ref().map(|b| (p, b))) else {
                return Err(format!(
                    "{ctx}: from `{from}`: not an lsystem's wood part or a skin op (auto-rigs grow from those)"
                ));
            };
            let (bones, point) = auto::from_lsystem(from, def, br.0, br.1)
                .map_err(|e| format!("{ctx}: from {e}"))?;
            raw.extend(bones);
            lsys = Some((from.clone(), point));
        }
    }
    let generated: Vec<String> = raw.iter().map(|r| r.0.clone()).collect();
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
    match mirror.as_deref() {
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
            rest: Quat::IDENTITY,
            limits: None,
            look_at: None,
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
    // Rest rotations: listed bones first, then mirror twins take the
    // mirrored frame of their source (M R M, M = flip x).
    for b in &def.bones {
        let Some(k) = bones.iter().position(|x| x.name == b.name) else {
            continue;
        };
        bones[k].rest = rest_of(b, bones[k].head, bones[k].tail)
            .map_err(|e| format!("{ctx}: bone `{}`: {e}", b.name))?;
    }
    // A human body brings anatomical frames and joint limits for its
    // arms and fingers (legs and spine stay world-aligned).
    if let Some(from) = &def.from
        && let Some(bd) = ops.iter().find_map(|o| match o {
            Op::Body { name: n, def } if n == from && def.preset == "human" => Some(def),
            _ => None,
        })
    {
        let turn = parts
            .iter()
            .find(|(n, _)| n == from)
            .map_or(Quat::IDENTITY, |(_, p)| crate::quat_of(p.rotation));
        let frames =
            crate::human::bone_frames(bd).map_err(|e| format!("{ctx}: from `{from}`: {e}"))?;
        for (name, q, limits) in frames {
            if def.bones.iter().any(|b| b.name == name) {
                continue;
            }
            if let Some(k) = bones.iter().position(|x| x.name == name) {
                bones[k].rest = turn * q;
                bones[k].limits = limits;
            }
        }
    }
    if mirror.as_deref() == Some("x") {
        for b in &def.bones {
            let (Some(twin), Some(src)) = (
                mirror_name(&b.name),
                bones.iter().position(|x| x.name == b.name),
            ) else {
                continue;
            };
            let q = bones[src].rest;
            if let Some(k) = bones.iter().position(|x| x.name == twin)
                && q != Quat::IDENTITY
            {
                bones[k].rest = Quat::from_xyzw(q.x, -q.y, -q.z, q.w);
            }
        }
    }
    // Constraints: listed bones, then their mirror twins.
    for b in &def.bones {
        let Some(k) = bones.iter().position(|x| x.name == b.name) else {
            continue;
        };
        let bctx = format!("{ctx}: bone `{}`", b.name);
        if let Some(l) = &b.limits {
            for r in [l.x, l.y, l.z].into_iter().flatten() {
                if !r[0].is_finite() || !r[1].is_finite() || r[0] > r[1] {
                    return Err(format!("{bctx}: limits: each range is [min, max] degrees"));
                }
            }
        }
        let look = match &b.look_at {
            None => None,
            Some(la) => {
                let target = match &la.target {
                    PointRef::Point(p) => LookTarget::Point(Vec3::from(*p)),
                    PointRef::Anchor(a) if a.contains(':') => LookTarget::Point(
                        anchor(a, parts).map_err(|e| format!("{bctx}: look_at {e}"))?,
                    ),
                    PointRef::Anchor(a) => match bones.iter().position(|x| &x.name == a) {
                        Some(i) if i != k => LookTarget::Bone(i),
                        _ => {
                            return Err(format!(
                                "{bctx}: look_at `{a}`: not a bone of this skeleton (or the bone itself); anchors are \"part:words\""
                            ));
                        }
                    },
                };
                let rest = bones[k].rest;
                let axis = match la.axis {
                    Some(a) => Vec3::from(a).try_normalize(),
                    None => (rest.inverse() * (bones[k].tail - bones[k].head)).try_normalize(),
                }
                .ok_or(format!("{bctx}: look_at: the aiming axis is zero"))?;
                let weight = la.weight.unwrap_or(1.0);
                if !(0.0..=1.0).contains(&weight) {
                    return Err(format!("{bctx}: look_at weight must be 0..1"));
                }
                Some(LookAt {
                    target,
                    axis,
                    weight,
                })
            }
        };
        bones[k].limits = b.limits.clone();
        bones[k].look_at = look.clone();
        if mirror.as_deref() == Some("x")
            && let Some(twin) = mirror_name(&b.name)
            && let Some(t) = bones.iter().position(|x| x.name == twin)
        {
            bones[t].limits = b.limits.as_ref().map(Limits::mirrored);
            bones[t].look_at = look.map(|l| LookAt {
                target: match l.target {
                    LookTarget::Point(p) => LookTarget::Point(Vec3::new(-p.x, p.y, p.z)),
                    LookTarget::Bone(i) => LookTarget::Bone(
                        mirror_name(&bones[i].name)
                            .and_then(|n| bones.iter().position(|x| x.name == n))
                            .unwrap_or(i),
                    ),
                },
                axis: Vec3::new(-l.axis.x, l.axis.y, l.axis.z),
                weight: l.weight,
            });
        }
    }
    if bones.is_empty() {
        return Err(format!("{ctx}: needs at least one bone"));
    }
    if bones.len() > u16::MAX as usize {
        return Err(format!("{ctx}: too many bones"));
    }
    let auto = match lsys {
        None => None,
        Some((from, point)) => {
            let idx: Vec<u16> = generated
                .iter()
                .map(|n| bones.iter().position(|b| &b.name == n).unwrap_or(0) as u16)
                .collect();
            let map = |b: usize| idx.get(b).copied().unwrap_or(0);
            Some(std::sync::Arc::new(auto::AutoMap {
                from,
                point: point
                    .into_iter()
                    .map(|c| {
                        c.into_iter()
                            .map(|[(a, wa), (b, wb)]| [(map(a), wa), (map(b), wb)])
                            .collect()
                    })
                    .collect(),
            }))
        }
    };
    Ok(Skeleton {
        name: name.to_string(),
        bones,
        auto,
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
    /// The clip this one layers over (a sway's `over`, or a keyed clip's
    /// `over` with `layering` set).
    pub base: Option<Box<Clip>>,
    /// Dynamic motion on top (a `sway`).
    pub layer: Option<std::sync::Arc<sway::Layer>>,
    /// Export samples per second (default 30).
    pub fps: Option<f32>,
    /// IK entries, parents' end bones first.
    pub ik: Vec<ik::Ik>,
    /// How a keyed clip with `over` combines with its base.
    pub layering: Option<Layering>,
    pub root_motion: Option<RootMotion>,
    pub events: Vec<ClipEvent>,
    /// A `pose` item seen as a one-second static clip (never exported or
    /// listed; `agent_views` / `inspect` / `verify` reach it by name).
    pub pose: bool,
    /// Morph target weights this level keys.
    pub morphs: Vec<MorphTrack>,
    /// Ground velocity `[vx, vz]` (m/s) of the motion the clip was made
    /// from (a `mocap` clip played in place): the default `speed` of
    /// `verify animation` foot_slide / gait.
    pub speed: Option<[f32; 2]>,
    /// What building it found (a `mocap` clip's retarget report).
    pub report: Option<String>,
}

impl Clip {
    /// A plain clip with no tracks.
    pub fn empty(name: &str, skeleton: &str, duration: f32, looping: bool) -> Clip {
        Clip {
            name: name.to_string(),
            skeleton: skeleton.to_string(),
            duration,
            looping,
            tracks: Vec::new(),
            base: None,
            layer: None,
            fps: None,
            ik: Vec::new(),
            layering: None,
            root_motion: None,
            events: Vec::new(),
            pose: false,
            morphs: Vec::new(),
            speed: None,
            report: None,
        }
    }

    /// Whether this clip or a base it layers over keys morph weights.
    pub fn has_morphs(&self) -> bool {
        !self.morphs.is_empty() || self.base.as_ref().is_some_and(|b| b.has_morphs())
    }

    /// Morph weights at `t`: (part, shape index, weight) for every shape
    /// any level keys. A keyed layer replaces its base's weight (blended
    /// by its `weight`) or, `additive`, adds `weight` x its own.
    pub fn morph_weights(&self, t: f32) -> Vec<(String, usize, f32)> {
        let mut out = match &self.base {
            Some(b) => b.morph_weights(t),
            None => Vec::new(),
        };
        if self.morphs.is_empty() {
            return out;
        }
        let layered = self.keyed_layer();
        let additive = self.layering.as_ref().is_some_and(|l| l.additive);
        let w = if layered { self.weight_at(t) } else { 1.0 };
        let tt = self.wrap(t);
        for m in &self.morphs {
            let v = vec_at(&m.channel, tt, self.duration, self.looping, 0.0).x;
            let i = match out.iter().position(|o| o.0 == m.part && o.1 == m.index) {
                Some(i) => i,
                None => {
                    out.push((m.part.clone(), m.index, 0.0));
                    out.len() - 1
                }
            };
            let slot = &mut out[i].2;
            if !layered {
                *slot = v;
            } else if additive {
                *slot += v * w;
            } else {
                let wc = w.min(1.0);
                *slot += (v - *slot) * wc;
            }
        }
        out
    }

    /// Every (part, shape) any level keys, base first, with its key times
    /// (empty for a wave) and whether it is a wave.
    pub fn morph_lanes(&self) -> Vec<(String, String, Vec<f32>, bool)> {
        let mut out = match &self.base {
            Some(b) => {
                let reps = (self.duration / b.duration.max(1e-6)).round().max(1.0) as usize;
                b.morph_lanes()
                    .into_iter()
                    .map(|(p, s, ks, w)| {
                        let ks: Vec<f32> = ks
                            .iter()
                            .flat_map(|t| (0..reps).map(move |r| t + r as f32 * b.duration))
                            .collect();
                        (p, s, ks, w)
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        for m in &self.morphs {
            let ks = channel_times(&m.channel);
            let wave = matches!(m.channel, Channel::Wave { .. });
            match out.iter_mut().find(|o| o.0 == m.part && o.1 == m.shape) {
                Some(o) => {
                    o.2.extend(ks);
                    o.3 |= wave;
                }
                None => out.push((m.part.clone(), m.shape.clone(), ks, wave)),
            }
        }
        out
    }

    /// Whether this is a keyed layer over a keyed base.
    fn keyed_layer(&self) -> bool {
        self.base.is_some() && self.layer.is_none()
    }

    /// The pose at time `t` (wrapped when looping, clamped otherwise).
    ///
    /// Order: the base's tracks (a keyed `over`, recursively), this clip's
    /// tracks layered on them, then once on the result: limits, every
    /// level's IK (base first) and look_at; a sway's dynamics last.
    pub fn sample(&self, skel: &Skeleton, t: f32) -> Pose {
        if let Some(l) = &self.layer {
            let mut pose = match &self.base {
                Some(b) => b.sample(skel, t),
                None => skel.rest(),
            };
            if self.base.is_none() && (!self.ik.is_empty() || skel.constrained()) {
                ik::post(self, skel, t, &mut pose);
            }
            sway::apply(l, skel, self.duration, self.wrap(t), &mut pose);
            return pose;
        }
        let mut pose = self.fk(skel, t);
        let mut levels: Vec<&Clip> = vec![self];
        while let Some(b) = levels.last().and_then(|c| c.base.as_deref()) {
            levels.push(b);
        }
        levels.reverse();
        if levels.iter().any(|c| !c.ik.is_empty()) || skel.constrained() {
            ik::post_levels(&levels, skel, t, &mut pose);
        }
        pose
    }

    /// The layer weight at `t` (1 without one).
    fn weight_at(&self, t: f32) -> f32 {
        match self.layering.as_ref().and_then(|l| l.weight.as_ref()) {
            Some(c) => vec_at(c, self.wrap(t), self.duration, self.looping, 1.0)
                .x
                .max(0.0),
            None => 1.0,
        }
    }

    /// Forward kinematics only: the tracks (layered over a keyed base).
    fn fk(&self, skel: &Skeleton, t: f32) -> Pose {
        let layered = self.keyed_layer();
        let mut pose = match &self.base {
            Some(b) if layered => b.fk(skel, t),
            _ => skel.rest(),
        };
        let additive = self.layering.as_ref().is_some_and(|l| l.additive);
        let w = if layered { self.weight_at(t) } else { 1.0 };
        if layered && w == 0.0 {
            return pose;
        }
        for (bone, track, mirrored, shift) in &self.tracks {
            let tt = self.wrap(t + shift);
            let d = self.duration;
            let lp = self.looping;
            let r = track.rotate.as_ref().map(|c| rot_at(c, tt, d, lp));
            let tr = track.translate.as_ref().map(|c| vec_at(c, tt, d, lp, 0.0));
            let s = track.scale.as_ref().map(|c| vec_at(c, tt, d, lp, 1.0));
            let mr = |r: Quat| {
                if *mirrored {
                    Quat::from_xyzw(r.x, -r.y, -r.z, r.w)
                } else {
                    r
                }
            };
            let mt = |mut v: Vec3| {
                if *mirrored {
                    v.x = -v.x;
                }
                v
            };
            let Some(slot) = pose.get_mut(*bone) else {
                continue;
            };
            if !layered {
                *slot = (
                    mt(tr.unwrap_or(Vec3::ZERO)),
                    mr(r.unwrap_or(Quat::IDENTITY)),
                    s.unwrap_or(Vec3::ONE),
                );
                continue;
            }
            let (r, tr) = (r.map(mr), tr.map(mt));
            if additive {
                if let Some(v) = tr {
                    slot.0 += v * w;
                }
                if let Some(q) = r {
                    let q = if w == 1.0 {
                        q
                    } else {
                        Quat::IDENTITY.slerp(q, w)
                    };
                    slot.1 = (slot.1 * q).normalize();
                }
                if let Some(v) = s {
                    slot.2 *= if w == 1.0 { v } else { Vec3::ONE.lerp(v, w) };
                }
            } else {
                let wc = w.min(1.0);
                if let Some(v) = tr {
                    slot.0 = if wc == 1.0 { v } else { slot.0.lerp(v, wc) };
                }
                if let Some(q) = r {
                    slot.1 = if wc == 1.0 {
                        q
                    } else {
                        slot.1.slerp(q, wc).normalize()
                    };
                }
                if let Some(v) = s {
                    slot.2 = if wc == 1.0 { v } else { slot.2.lerp(v, wc) };
                }
            }
        }
        pose
    }

    /// World travel of the root-motion bone at `t` from t = 0 (on its
    /// axes) and its yaw from t = 0 (degrees, 0 without `yaw`).
    pub fn travel(&self, skel: &Skeleton, t: f32) -> (Vec3, f32) {
        let Some(rm) = &self.root_motion else {
            return (Vec3::ZERO, 0.0);
        };
        let at = |t: f32| -> (Vec3, f32) {
            let p = self.sample(skel, t);
            let b = &skel.bones[rm.bone];
            let (tr, r, _) =
                p.get(rm.bone)
                    .copied()
                    .unwrap_or((Vec3::ZERO, Quat::IDENTITY, Vec3::ONE));
            let w = b.rest * tr;
            let q = (b.rest * r).normalize();
            (w, 2.0 * q.y.atan2(q.w).to_degrees())
        };
        let mask = |v: Vec3| {
            Vec3::new(
                if rm.axes[0] { v.x } else { 0.0 },
                if rm.axes[1] { v.y } else { 0.0 },
                if rm.axes[2] { v.z } else { 0.0 },
            )
        };
        let (w0, y0) = at(0.0);
        let one = |t: f32| {
            let (w, y) = at(t);
            let dy = if rm.yaw {
                (y - y0 + 180.0).rem_euclid(360.0) - 180.0
            } else {
                0.0
            };
            (mask(w - w0), dy)
        };
        if self.looping && t >= self.duration - 1e-6 {
            // t = duration wraps to 0: extrapolate the end from below.
            let e = (self.duration * 1e-3).max(1e-5);
            let (a, ay) = one(self.duration - e);
            let (b, by) = one(self.duration - 2.0 * e);
            return (a + (a - b), ay + (ay - by));
        }
        one(t)
    }

    /// The pose at `t` with the root motion taken out (what a game plays
    /// while it moves the entity itself); [`Clip::sample`] without root
    /// motion.
    pub fn sample_in_place(&self, skel: &Skeleton, t: f32) -> Pose {
        let mut pose = self.sample(skel, t);
        let Some(rm) = &self.root_motion else {
            return pose;
        };
        let (trav, yaw) = self.travel(skel, t);
        let b = &skel.bones[rm.bone];
        if let Some(slot) = pose.get_mut(rm.bone) {
            slot.0 -= b.rest.inverse() * trav;
            if rm.yaw && yaw != 0.0 {
                let w = Quat::from_rotation_y(-yaw.to_radians()) * (b.rest * slot.1);
                slot.1 = (b.rest.inverse() * w).normalize();
            }
        }
        pose
    }

    /// Root motion over the whole clip: (delta, yaw degrees).
    pub fn root_delta(&self, skel: &Skeleton) -> (Vec3, f32) {
        self.travel(skel, self.duration)
    }

    /// Which channels of which bones move: (bone, translate, rotate, scale).
    pub fn channels(&self, skel: &Skeleton) -> Vec<(usize, bool, bool, bool)> {
        let mut out: Vec<(usize, bool, bool, bool)> = match &self.base {
            Some(b) => b.channels(skel),
            None => Vec::new(),
        };
        let mut add =
            |bone: usize, t: bool, r: bool, s: bool| match out.iter_mut().find(|c| c.0 == bone) {
                Some(c) => {
                    c.1 |= t;
                    c.2 |= r;
                    c.3 |= s;
                }
                None => out.push((bone, t, r, s)),
            };
        for (bone, track, _, _) in &self.tracks {
            add(
                *bone,
                track.translate.is_some(),
                track.rotate.is_some(),
                track.scale.is_some(),
            );
        }
        if self.base.is_none() || self.layer.is_none() {
            for k in &self.ik {
                for &b in &k.chain {
                    add(b, false, true, false);
                }
                if k.rotate.is_some() {
                    add(k.bone, false, true, false);
                }
            }
            for (i, b) in skel.bones.iter().enumerate() {
                let out_of_range = b
                    .limits
                    .as_ref()
                    .is_some_and(|l| ik::clamp(Quat::IDENTITY, l) != Quat::IDENTITY);
                if b.look_at.is_some() || out_of_range {
                    add(i, false, true, false);
                }
            }
        }
        match self.layer.as_deref() {
            Some(sway::Layer::Wind(rig)) => {
                for (i, b) in rig.bones.iter().enumerate() {
                    if b.weight > 0.0 {
                        add(i, false, true, false);
                    }
                }
            }
            Some(sway::Layer::Spring(_)) => {
                for i in 0..skel.bones.len() {
                    add(i, false, true, false);
                }
            }
            None => {}
        }
        out.sort_by_key(|c| c.0);
        out
    }

    /// Key times of keyed channels (base included), wrapped into the clip.
    pub fn key_times(&self) -> Vec<f32> {
        let mut out = Vec::new();
        if let Some(b) = &self.base {
            let reps = (self.duration / b.duration).round().max(1.0) as usize;
            for t in b.key_times() {
                for k in 0..reps {
                    out.push(t + k as f32 * b.duration);
                }
            }
        }
        for (_, track, _, shift) in &self.tracks {
            for ch in [&track.rotate, &track.translate, &track.scale]
                .into_iter()
                .flatten()
            {
                if let Channel::Keys { keys, .. } | Channel::Euler { keys, .. } = ch {
                    for (t, _) in keys {
                        out.push(self.wrap(t - shift));
                    }
                }
            }
        }
        for m in &self.morphs {
            if let Channel::Keys { keys, .. } = &m.channel {
                for (t, _) in keys {
                    out.push(self.wrap(*t));
                }
            }
        }
        for k in &self.ik {
            for ch in [Some(&k.target), k.weight.as_ref(), k.rotate.as_ref()]
                .into_iter()
                .flatten()
            {
                if let Channel::Keys { keys, .. } = ch
                    && keys.len() > 1
                {
                    for (t, _) in keys {
                        out.push(self.wrap(t - k.shift));
                    }
                }
            }
        }
        out
    }

    /// Bones IK entries move (chain bones and ends), with whether the
    /// entry is a mirror copy.
    pub fn ik_bones(&self) -> Vec<(usize, bool)> {
        let mut out: Vec<(usize, bool)> = Vec::new();
        for k in &self.ik {
            for &b in k.chain.iter().chain(std::iter::once(&k.bone)) {
                if !out.iter().any(|o| o.0 == b) {
                    out.push((b, k.mirrored));
                }
            }
        }
        out
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
///
/// `cumulative` (Euler angles): across a loop's seam the wrapped
/// neighbours are offset by the loop's total change (last - first), so a
/// spin keeps turning the same way through the seam.
fn around(
    keys: &[(f32, [f32; 4])],
    t: f32,
    d: f32,
    looping: bool,
    cumulative: bool,
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
    let span = |k: usize| -> f32 {
        if cumulative {
            keys[n - 1].1[k] - keys[0].1[k]
        } else {
            0.0
        }
    };
    let shifted = |v: [f32; 4], sign: f32| -> [f32; 4] {
        if !cumulative {
            return v;
        }
        [
            v[0] + sign * span(0),
            v[1] + sign * span(1),
            v[2] + sign * span(2),
            v[3] + sign * span(3),
        ]
    };
    let prev = if i > 0 {
        keys[i - 1].1
    } else if periodic && (n > 2 || cumulative) {
        shifted(keys[n - 2].1, -1.0)
    } else {
        a.1
    };
    let next = if i + 2 < n {
        keys[i + 2].1
    } else if periodic && (n > 2 || cumulative) {
        shifted(keys[1].1, 1.0)
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
        Channel::Euler { keys, interp } => {
            if keys.is_empty() {
                return Quat::IDENTITY;
            }
            euler_quat(lerp_keys(keys, *interp, t, d, looping, true))
        }
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
            let (k0, k1, k2, k3, u) = around(keys, t, d, looping, false);
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
        Channel::Keys { keys, interp } | Channel::Euler { keys, interp } => {
            if keys.is_empty() {
                return Vec3::splat(base);
            }
            lerp_keys(keys, *interp, t, d, looping, false)
        }
    }
}

/// Keys of 3-vectors interpolated component-wise.
fn lerp_keys(
    keys: &[(f32, [f32; 4])],
    interp: Interp,
    t: f32,
    d: f32,
    looping: bool,
    cumulative: bool,
) -> Vec3 {
    let (k0, k1, k2, k3, u) = around(keys, t, d, looping, cumulative);
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

/// Builds a clip against its skeleton (no poses, no `over`).
pub fn build_clip(
    name: &str,
    def: &ClipDef,
    skel: &Skeleton,
    parts: &[(String, std::sync::Arc<Part>)],
) -> Result<Clip, String> {
    build_clip_with(name, def, skel, parts, &[], None)
}

/// Every key time of a channel.
fn channel_times(c: &Channel) -> Vec<f32> {
    match c {
        Channel::Keys { keys, .. } | Channel::Euler { keys, .. } => {
            keys.iter().map(|k| k.0).collect()
        }
        Channel::Wave { .. } => Vec::new(),
    }
}

/// The tracks `poses` keys make: per bone every pose touches, keys at
/// each pose key (the pose's value, or rest where that pose leaves the
/// bone alone).
type MorphChannels = BTreeMap<String, BTreeMap<String, Channel>>;

fn pose_tracks(
    ctx: &str,
    def: &ClipDef,
    skel: &Skeleton,
    poses: &[BuiltPose],
) -> Result<(BTreeMap<String, TrackDef>, MorphChannels), String> {
    let mut keyed: Vec<(f32, Option<&BuiltPose>)> = Vec::new();
    for (t, n) in &def.poses {
        if n == "rest" {
            keyed.push((*t, None));
            continue;
        }
        let Some(p) = poses.iter().find(|p| &p.name == n) else {
            let names: Vec<&str> = poses
                .iter()
                .filter(|p| p.skeleton == skel.name)
                .map(|p| p.name.as_str())
                .collect();
            return Err(format!(
                "{ctx}: poses: no pose `{n}` (poses of `{}`: rest{}{})",
                skel.name,
                if names.is_empty() { "" } else { ", " },
                names.join(", ")
            ));
        };
        if p.skeleton != skel.name {
            return Err(format!(
                "{ctx}: poses: pose `{n}` is for skeleton `{}`, not `{}`",
                p.skeleton, skel.name
            ));
        }
        keyed.push((*t, Some(p)));
    }
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    if keyed.windows(2).any(|w| (w[1].0 - w[0].0).abs() < 1e-6) {
        return Err(format!("{ctx}: poses: two pose keys at the same time"));
    }
    // bone -> (rotate?, translate?, scale?)
    let mut used: BTreeMap<usize, (bool, bool, bool)> = BTreeMap::new();
    for (_, p) in &keyed {
        for (b, r, t, s) in p.map(|p| p.bones.as_slice()).unwrap_or(&[]) {
            let e = used.entry(*b).or_default();
            e.0 |= r.is_some();
            e.1 |= t.is_some();
            e.2 |= s.is_some();
        }
    }
    let interp = def.interp.unwrap_or_default();
    let mut out = BTreeMap::new();
    for (b, (ur, ut, us)) in used {
        let get = |p: Option<&BuiltPose>| {
            p.and_then(|p| p.bones.iter().find(|x| x.0 == b))
                .map(|x| (x.1, x.2, x.3))
                .unwrap_or((None, None, None))
        };
        let keys = |f: &dyn Fn(Option<&BuiltPose>) -> [f32; 4]| -> Channel {
            Channel::Keys {
                keys: keyed.iter().map(|(t, p)| (*t, f(*p))).collect(),
                interp,
            }
        };
        let mut tr = TrackDef::default();
        if ur {
            tr.rotate = Some(keys(&|p| get(p).0.unwrap_or(Quat::IDENTITY).to_array()));
        }
        if ut {
            tr.translate = Some(keys(&|p| {
                let v = get(p).1.unwrap_or(Vec3::ZERO);
                [v.x, v.y, v.z, 0.0]
            }));
        }
        if us {
            tr.scale = Some(keys(&|p| {
                let v = get(p).2.unwrap_or(Vec3::ONE);
                [v.x, v.y, v.z, 0.0]
            }));
        }
        out.insert(skel.bones[b].name.clone(), tr);
    }
    // Morph weights any pose keys: the pose's weight, or 0 where it
    // leaves that shape alone.
    let mut morphs: MorphChannels = BTreeMap::new();
    let mut shapes: Vec<(&str, &str)> = Vec::new();
    for (_, p) in &keyed {
        for (part, shape, ..) in p.map(|p| p.morphs.as_slice()).unwrap_or(&[]) {
            if !shapes.contains(&(part.as_str(), shape.as_str())) {
                shapes.push((part, shape));
            }
        }
    }
    for (part, shape) in shapes {
        let keys = keyed
            .iter()
            .map(|(t, p)| {
                let w = p
                    .and_then(|p| p.morphs.iter().find(|m| m.0 == part && m.1 == shape))
                    .map_or(0.0, |m| m.3);
                (*t, [w, w, w, 0.0])
            })
            .collect();
        morphs
            .entry(part.to_string())
            .or_default()
            .insert(shape.to_string(), Channel::Keys { keys, interp });
    }
    Ok((out, morphs))
}

/// The closest of `names` to `n` (a typo), if any is near.
fn closest<'a>(n: &str, names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    names
        .map(|c| (crate::spec::edit_distance(n, c), c))
        .min()
        .filter(|(d, c)| *d <= 2.max(c.len() / 3))
        .map(|x| x.1)
}

/// Resolves morph channels against the parts: each part must exist and
/// carry the shape (errors name the closest match).
fn resolve_morphs(
    ctx: &str,
    morphs: &MorphChannels,
    parts: &[(String, std::sync::Arc<Part>)],
) -> Result<Vec<MorphTrack>, String> {
    let mut out = Vec::new();
    for (part, shapes) in morphs {
        let Some((_, p)) = parts.iter().find(|(n, _)| n == part) else {
            let with: Vec<&str> = parts
                .iter()
                .filter(|(_, p)| p.morphs.as_ref().is_some_and(|m| !m.is_empty()))
                .map(|(n, _)| n.as_str())
                .collect();
            let near = closest(part, parts.iter().map(|(n, _)| n.as_str()));
            return Err(format!(
                "{ctx}: morphs.{part}: no such part{} (parts with morph targets: {})",
                near.map(|n| format!("; did you mean `{n}`?"))
                    .unwrap_or_default(),
                if with.is_empty() {
                    "none".into()
                } else {
                    with.join(", ")
                }
            ));
        };
        let names: Vec<&str> = p
            .morphs
            .as_ref()
            .map(|m| m.iter().map(|x| x.0.as_str()).collect())
            .unwrap_or_default();
        for (shape, ch) in shapes {
            let Some(index) = names.iter().position(|n| n == shape) else {
                if names.is_empty() {
                    return Err(format!(
                        "{ctx}: morphs.{part}.{shape}: `{part}` has no morph targets (a human body's `shapes` makes them)"
                    ));
                }
                let near = closest(shape, names.iter().copied());
                return Err(format!(
                    "{ctx}: morphs.{part}.{shape}: no shape `{shape}`{} (shapes of `{part}`: {})",
                    near.map(|n| format!("; did you mean `{n}`?"))
                        .unwrap_or_default(),
                    names.join(", ")
                ));
            };
            out.push(MorphTrack {
                part: part.clone(),
                shape: shape.clone(),
                index,
                channel: ch.clone(),
            });
        }
    }
    Ok(out)
}

/// Builds a clip against its skeleton, with the model's built `poses`
/// and its `over` base (already built) if it has one.
pub fn build_clip_with(
    name: &str,
    def: &ClipDef,
    skel: &Skeleton,
    parts: &[(String, std::sync::Arc<Part>)],
    poses: &[BuiltPose],
    base: Option<Clip>,
) -> Result<Clip, String> {
    let ctx = format!("clip `{name}`");
    if def.over.is_some() != base.is_some() {
        return Err(format!("{ctx}: over: the base clip wasn't built"));
    }
    if base.is_none() {
        if def.additive {
            return Err(format!(
                "{ctx}: additive layers over a base clip: give `over`"
            ));
        }
        if def.weight.is_some() || !def.bones.is_empty() {
            return Err(format!(
                "{ctx}: weight and bones are for a layer: give `over` (the base clip)"
            ));
        }
    }
    let duration = match &base {
        Some(b) if def.duration <= 0.0 => b.duration,
        _ => def.duration,
    };
    if duration.is_nan() || duration <= 0.0 {
        return Err(format!("{ctx}: duration must be positive"));
    }
    if let Some(b) = &base
        && b.looping
        && def.looping
    {
        let cycles = duration / b.duration;
        if (cycles - cycles.round()).abs() > 1e-3 || cycles.round() < 1.0 {
            return Err(format!(
                "{ctx}: duration {duration} s is not a whole number of `{}` cycles ({} s each)",
                b.name, b.duration
            ));
        }
    }
    // Key times against the resolved duration (parsing checked them when
    // the duration was given).
    {
        let mut ts: Vec<f32> = def.poses.iter().map(|p| p.0).collect();
        ts.extend(def.events.iter().map(|e| e.time));
        for tr in def.tracks.values() {
            for c in [&tr.rotate, &tr.translate, &tr.scale].into_iter().flatten() {
                ts.extend(channel_times(c));
            }
        }
        if let Some(w) = &def.weight {
            ts.extend(channel_times(w));
        }
        for c in def.morphs.values().flat_map(|m| m.values()) {
            ts.extend(channel_times(c));
        }
        if let Some(t) = ts
            .iter()
            .find(|t| t.is_nan() || **t < -1e-6 || **t > duration + 1e-4)
        {
            return Err(format!("{ctx}: time {t} is outside 0..{duration}"));
        }
    }
    let in_layer =
        |bone: &str| def.bones.is_empty() || def.bones.iter().any(|q| crate::name_matches(q, bone));
    if !def.bones.is_empty() && !skel.bones.iter().any(|b| in_layer(&b.name)) {
        return Err(format!(
            "{ctx}: bones {:?} match no bone of `{}` (bones: {})",
            def.bones,
            skel.name,
            bone_list(skel)
        ));
    }
    // Pose keys first, explicit tracks override them channel by channel.
    let (mut merged, mut morph_map) = pose_tracks(&ctx, def, skel, poses)?;
    for (part, shapes) in &def.morphs {
        let e = morph_map.entry(part.clone()).or_default();
        for (shape, ch) in shapes {
            e.insert(shape.clone(), ch.clone());
        }
    }
    let morphs = resolve_morphs(&ctx, &morph_map, parts)?;
    for (bone, track) in &def.tracks {
        let e = merged.entry(bone.clone()).or_default();
        if track.rotate.is_some() {
            e.rotate = track.rotate.clone();
        }
        if track.translate.is_some() {
            e.translate = track.translate.clone();
        }
        if track.scale.is_some() {
            e.scale = track.scale.clone();
        }
    }
    let mut tracks = Vec::new();
    for (bone, track) in &merged {
        let Some(i) = skel.bone(bone) else {
            return Err(format!(
                "{ctx}: track `{bone}`: skeleton `{}` has no such bone (bones: {})",
                skel.name,
                bone_list(skel)
            ));
        };
        // A mirror onto this bone replaces what the poses keyed.
        let mirrored_here = def
            .mirror
            .get(bone)
            .is_some_and(|m| def.tracks.contains_key(&m.from))
            && !def.tracks.contains_key(bone);
        if mirrored_here || !in_layer(bone) {
            continue;
        }
        tracks.push((i, track.clone(), false, 0.0));
    }
    let mut iks = Vec::new();
    for (bone, d) in &def.ik {
        if in_layer(bone) {
            iks.push(build_ik(&ctx, bone, d, skel, parts, false, 0.0)?);
        }
    }
    for (bone, m) in &def.mirror {
        let Some(i) = skel.bone(bone) else {
            return Err(format!(
                "{ctx}: mirror `{bone}`: no such bone (bones: {})",
                bone_list(skel)
            ));
        };
        let src = def.tracks.get(&m.from);
        let src_ik = def.ik.get(&m.from);
        if src.is_none() && src_ik.is_none() {
            return Err(format!(
                "{ctx}: mirror `{bone}`: no track or ik entry `{}` to copy",
                m.from
            ));
        }
        if !in_layer(bone) {
            continue;
        }
        if let Some(src) = src {
            if def.tracks.contains_key(bone) {
                return Err(format!(
                    "{ctx}: `{bone}` has a track and a mirror: drop one"
                ));
            }
            tracks.push((i, src.clone(), true, m.phase * duration));
        }
        if let Some(d) = src_ik {
            if def.ik.contains_key(bone) {
                return Err(format!(
                    "{ctx}: `{bone}` has an ik entry and a mirror of one: drop one"
                ));
            }
            iks.push(build_ik(
                &ctx,
                bone,
                d,
                skel,
                parts,
                true,
                m.phase * duration,
            )?);
        }
    }
    iks.sort_by_key(|k| k.bone);
    let root_motion = match &def.root_motion {
        Some(rm) => Some(root_motion_of(&ctx, rm, skel)?),
        None => base.as_ref().and_then(|b| b.root_motion.clone()),
    };
    let mut events = def.events.clone();
    events.sort_by(|a, b| a.time.total_cmp(&b.time));
    let layering = base.as_ref().map(|_| Layering {
        additive: def.additive,
        weight: def.weight.clone(),
    });
    Ok(Clip {
        tracks,
        base: base.map(Box::new),
        ik: iks,
        layering,
        root_motion,
        events,
        morphs,
        ..Clip::empty(name, &def.skeleton, duration, def.looping)
    })
}

/// Resolves a clip's `root_motion` against its skeleton.
fn root_motion_of(ctx: &str, rm: &RootMotionDef, skel: &Skeleton) -> Result<RootMotion, String> {
    let found = if rm.bone.is_empty() {
        skel.bone("hip")
            .filter(|&i| skel.bones[i].parent.is_none())
            .or_else(|| skel.bones.iter().position(|b| b.parent.is_none()))
    } else {
        skel.bone(&rm.bone)
    };
    let Some(b) = found else {
        return Err(format!(
            "{ctx}: root_motion: no bone `{}` (bones: {})",
            rm.bone,
            bone_list(skel)
        ));
    };
    if skel.bones[b].parent.is_some() {
        let roots: Vec<&str> = skel
            .bones
            .iter()
            .filter(|b| b.parent.is_none())
            .map(|b| b.name.as_str())
            .collect();
        return Err(format!(
            "{ctx}: root_motion: `{}` has a parent: root motion rides a root bone ({})",
            rm.bone,
            roots.join(", ")
        ));
    }
    let mut axes = [false; 3];
    for c in rm.axes.chars() {
        match c {
            'x' => axes[0] = true,
            'y' => axes[1] = true,
            'z' => axes[2] = true,
            _ => {
                return Err(format!(
                    "{ctx}: root_motion axes `{}`: letters x, y, z (\"xz\")",
                    rm.axes
                ));
            }
        }
    }
    Ok(RootMotion {
        bone: b,
        axes,
        yaw: rm.yaw,
    })
}

/// Builds a `pose` item: its bones (mirror filled in) as offsets from
/// rest, then its IK entries solved once.
pub fn build_pose(
    name: &str,
    def: &PoseDef,
    skel: &Skeleton,
    parts: &[(String, std::sync::Arc<Part>)],
) -> Result<BuiltPose, String> {
    let ctx = format!("pose `{name}`");
    if name == "rest" {
        return Err(format!(
            "{ctx}: `rest` is the built-in rest pose: pick another name"
        ));
    }
    let mut given: BTreeMap<String, (PoseBone, bool)> = BTreeMap::new();
    for (bone, v) in &def.bones {
        given.insert(bone.clone(), (v.clone(), false));
    }
    if def.mirror {
        for (bone, v) in &def.bones {
            let Some(twin) = twin_name(bone) else {
                continue;
            };
            if def.bones.contains_key(&twin) {
                continue;
            }
            given.insert(twin, (v.clone(), true));
        }
    }
    let mut pose = skel.rest();
    let mut touched: Vec<(usize, bool, bool, bool)> = Vec::new();
    for (bone, (v, mirrored)) in &given {
        let Some(i) = skel.bone(bone) else {
            return Err(format!(
                "{ctx}: bones.{bone}: skeleton `{}` has no such bone (bones: {})",
                skel.name,
                bone_list(skel)
            ));
        };
        let m = *mirrored;
        if let Some(q) = v.rotate {
            let q = Quat::from_array(q).normalize();
            pose[i].1 = if m {
                Quat::from_xyzw(q.x, -q.y, -q.z, q.w)
            } else {
                q
            };
        }
        if let Some(t) = v.translate {
            pose[i].0 = Vec3::new(if m { -t[0] } else { t[0] }, t[1], t[2]);
        }
        if let Some(s) = v.scale {
            pose[i].2 = Vec3::from(s);
        }
        touched.push((
            i,
            v.rotate.is_some(),
            v.translate.is_some(),
            v.scale.is_some(),
        ));
    }
    let mut iks = Vec::new();
    for (bone, d) in &def.ik {
        if matches!(d.target, IkTarget::Keys(_)) {
            return Err(format!(
                "{ctx}: ik `{bone}`: a pose's target is a point or an anchor, not keys"
            ));
        }
        iks.push(build_ik(&ctx, bone, d, skel, parts, false, 0.0)?);
        if def.mirror
            && let Some(twin) = twin_name(bone)
            && !def.ik.contains_key(&twin)
            && skel.bone(&twin).is_some()
        {
            iks.push(build_ik(&ctx, &twin, d, skel, parts, true, 0.0)?);
        }
    }
    iks.sort_by_key(|k| k.bone);
    for k in &iks {
        let target = vec_at(&k.target, 0.0, 1.0, false, 0.0);
        let weight = k
            .weight
            .as_ref()
            .map(|c| vec_at(c, 0.0, 1.0, false, 1.0).x.clamp(0.0, 1.0))
            .unwrap_or(1.0);
        let rotate = k.rotate.as_ref().map(|c| rot_at(c, 0.0, 1.0, false));
        ik::solve(k, skel, &mut pose, target, weight, rotate);
        for &b in &k.chain {
            touched.push((b, true, false, false));
        }
        if k.rotate.is_some() {
            touched.push((k.bone, true, false, false));
        }
    }
    let mut bones: Vec<(usize, Option<Quat>, Option<Vec3>, Option<Vec3>)> = Vec::new();
    for (i, r, t, s) in touched {
        let e = match bones.iter_mut().find(|b| b.0 == i) {
            Some(e) => e,
            None => {
                bones.push((i, None, None, None));
                let n = bones.len() - 1;
                &mut bones[n]
            }
        };
        if r {
            e.1 = Some(pose[i].1);
        }
        if t {
            e.2 = Some(pose[i].0);
        }
        if s {
            e.3 = Some(pose[i].2);
        }
    }
    bones.sort_by_key(|b| b.0);
    let mut channels: MorphChannels = BTreeMap::new();
    for (part, shapes) in &def.morphs {
        for (shape, w) in shapes {
            channels
                .entry(part.clone())
                .or_default()
                .insert(shape.clone(), point_channel(Vec3::splat(*w)));
        }
    }
    let morphs = resolve_morphs(&ctx, &channels, parts)?
        .into_iter()
        .map(|m| {
            let w = vec_at(&m.channel, 0.0, 1.0, false, 0.0).x;
            (m.part, m.shape, m.index, w)
        })
        .collect();
    Ok(BuiltPose {
        name: name.to_string(),
        skeleton: def.skeleton.clone(),
        bones,
        morphs,
    })
}

impl BuiltPose {
    /// The pose as a one-second static clip (for views, inspect, verify).
    pub fn as_clip(&self, skel: &Skeleton) -> Clip {
        let one = |v: [f32; 4]| Channel::Keys {
            keys: vec![(0.0, v)],
            interp: Interp::Linear,
        };
        let tracks = self
            .bones
            .iter()
            .map(|(b, r, t, s)| {
                (
                    *b,
                    TrackDef {
                        rotate: r.map(|q| one(q.to_array())),
                        translate: t.map(|v| one([v.x, v.y, v.z, 0.0])),
                        scale: s.map(|v| one([v.x, v.y, v.z, 0.0])),
                    },
                    false,
                    0.0,
                )
            })
            .collect();
        let morphs = self
            .morphs
            .iter()
            .map(|(part, shape, index, w)| MorphTrack {
                part: part.clone(),
                shape: shape.clone(),
                index: *index,
                channel: point_channel(Vec3::splat(*w)),
            })
            .collect();
        Clip {
            tracks,
            pose: true,
            morphs,
            ..Clip::empty(&self.name, &skel.name, 1.0, false)
        }
    }
}

/// A rotate channel mirrored across x = 0.
fn mirror_rot(c: &Channel) -> Channel {
    match c {
        Channel::Euler { keys, interp } => Channel::Euler {
            keys: keys
                .iter()
                .map(|(t, e)| (*t, [e[0], -e[1], -e[2], 0.0]))
                .collect(),
            interp: *interp,
        },
        Channel::Keys { keys, interp } => Channel::Keys {
            keys: keys
                .iter()
                .map(|(t, q)| (*t, [q[0], -q[1], -q[2], q[3]]))
                .collect(),
            interp: *interp,
        },
        Channel::Wave {
            axis,
            amount,
            period,
            phase,
        } => Channel::Wave {
            axis: [axis[0], -axis[1], -axis[2]],
            amount: *amount,
            period: *period,
            phase: *phase,
        },
    }
}

/// A constant point as a one-key channel.
fn point_channel(v: Vec3) -> Channel {
    Channel::Keys {
        keys: vec![(0.0, [v.x, v.y, v.z, 0.0])],
        interp: Interp::Linear,
    }
}

/// Builds one IK entry (`mirrored`: the x-mirrored copy onto `bone`).
fn build_ik(
    ctx: &str,
    bone: &str,
    d: &IkDef,
    skel: &Skeleton,
    parts: &[(String, std::sync::Arc<Part>)],
    mirrored: bool,
    shift: f32,
) -> Result<ik::Ik, String> {
    let ictx = format!("{ctx}: ik `{bone}`");
    let Some(e) = skel.bone(bone) else {
        return Err(format!(
            "{ictx}: skeleton `{}` has no such bone (bones: {})",
            skel.name,
            bone_list(skel)
        ));
    };
    if d.chain == 0 {
        return Err(format!("{ictx}: chain must be at least 1"));
    }
    let mut chain = Vec::new();
    let mut cur = e;
    for _ in 0..d.chain {
        let Some(p) = skel.bones[cur].parent else {
            return Err(format!(
                "{ictx}: chain {}: `{bone}` has only {} bone(s) above it",
                d.chain,
                chain.len()
            ));
        };
        chain.push(p);
        cur = p;
    }
    chain.reverse();
    let flip = |v: Vec3| {
        if mirrored {
            Vec3::new(-v.x, v.y, v.z)
        } else {
            v
        }
    };
    let target = match &d.target {
        IkTarget::Point(p) => point_channel(flip(Vec3::from(*p))),
        IkTarget::Anchor(a) => point_channel(flip(
            anchor(a, parts).map_err(|e| format!("{ictx}: target {e}"))?,
        )),
        IkTarget::Keys(Channel::Keys { keys, interp }) => Channel::Keys {
            keys: keys
                .iter()
                .map(|(t, v)| {
                    let p = flip(Vec3::new(v[0], v[1], v[2]));
                    (*t, [p.x, p.y, p.z, 0.0])
                })
                .collect(),
            interp: *interp,
        },
        IkTarget::Keys(Channel::Wave { .. } | Channel::Euler { .. }) => {
            return Err(format!(
                "{ictx}: target: a point, an anchor or keys [[t, [x, y, z]], ...], not a wave"
            ));
        }
    };
    let pole = d
        .pole
        .as_ref()
        .map(|p| point(p, parts))
        .transpose()
        .map_err(|e| format!("{ictx}: pole {e}"))?
        .map(flip);
    let rotate = match &d.rotate {
        Some(c) if mirrored => Some(mirror_rot(c)),
        r => r.clone(),
    };
    Ok(ik::Ik {
        bone: e,
        chain,
        target,
        pole,
        weight: d.weight.clone(),
        rotate,
        mirrored,
        shift,
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

/// Built skeletons and clips, by [`rig_key`]: repeated lookups on an
/// unchanged model (the window plays a clip every frame) skip the rebuild,
/// which includes a spring sway's bake.
type RigEntry = (
    u64,
    std::sync::Arc<Vec<Skeleton>>,
    Option<std::sync::Arc<Vec<(Clip, Skeleton)>>>,
);
static RIG_CACHE: std::sync::Mutex<Vec<RigEntry>> = std::sync::Mutex::new(Vec::new());
const RIG_CACHE_LEN: usize = 16;

/// A content hash of everything skeletons and clips are built from: the
/// rig ops (skeleton, clip, sway) and the ops auto-rigs read (body, skin,
/// lsystem), serialized; and, for the parts they resolve against, the
/// part's transform plus (for anchor targets) its box and joints, or (for
/// `from` parts) its vertex count.
fn rig_key(ops: &[Op], parts: &[(String, std::sync::Arc<Part>)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut targets: Vec<&str> = Vec::new();
    let mut froms: Vec<&str> = Vec::new();
    for o in ops {
        let tag = match o {
            Op::Skeleton { def, .. } => {
                for b in &def.bones {
                    for p in std::iter::once(&b.head).chain(b.tail.as_ref()) {
                        if let PointRef::Anchor(a) = p {
                            targets.push(a.split_once(':').map_or(a.as_str(), |x| x.0));
                        }
                    }
                }
                for b in &def.bones {
                    if let Some(LookAtDef {
                        target: PointRef::Anchor(a),
                        ..
                    }) = &b.look_at
                        && let Some((part, _)) = a.split_once(':')
                    {
                        targets.push(part);
                    }
                }
                froms.extend(def.from.as_deref());
                0u8
            }
            Op::Clip { def, .. } => {
                if let Some(m) = &def.mocap {
                    crate::mocap::live_key(m).hash(&mut h);
                }
                for k in def.ik.values() {
                    let t = match &k.target {
                        IkTarget::Anchor(a) => Some(a),
                        _ => None,
                    };
                    let p = match &k.pole {
                        Some(PointRef::Anchor(a)) => Some(a),
                        _ => None,
                    };
                    for a in t.into_iter().chain(p) {
                        targets.push(a.split_once(':').map_or(a.as_str(), |x| x.0));
                    }
                }
                1
            }
            Op::Pose { def, .. } => {
                for k in def.ik.values() {
                    let t = match &k.target {
                        IkTarget::Anchor(a) => Some(a),
                        _ => None,
                    };
                    let p = match &k.pole {
                        Some(PointRef::Anchor(a)) => Some(a),
                        _ => None,
                    };
                    for a in t.into_iter().chain(p) {
                        targets.push(a.split_once(':').map_or(a.as_str(), |x| x.0));
                    }
                }
                6
            }
            Op::Sway { .. } => 2,
            Op::Body { name, .. } => {
                froms.push(name);
                3
            }
            Op::Skin { .. } => 4,
            Op::LSystem { .. } => 5,
            _ => continue,
        };
        tag.hash(&mut h);
        serde_json::to_string(o).unwrap_or_default().hash(&mut h);
    }
    let bits = |h: &mut std::collections::hash_map::DefaultHasher, v: &[f32]| {
        for x in v {
            x.to_bits().hash(h);
        }
    };
    // Morph target names (clips and poses resolve shapes against them).
    for (n, p) in parts {
        if let Some(m) = &p.morphs {
            n.hash(&mut h);
            for (s, d) in m.iter() {
                s.hash(&mut h);
                d.len().hash(&mut h);
            }
        }
    }
    for (n, p) in parts {
        let anchored = targets.iter().any(|t| crate::name_matches(t, n));
        let from = froms.contains(&n.as_str());
        if !anchored && !from {
            continue;
        }
        n.hash(&mut h);
        bits(&mut h, &p.translation);
        bits(&mut h, &p.rotation);
        bits(&mut h, &p.scale);
        p.verts.len().hash(&mut h);
        if anchored {
            if !p.verts.is_empty() {
                let (lo, hi) = p.bounds();
                bits(&mut h, &lo.to_array());
                bits(&mut h, &hi.to_array());
            }
            if let Some(j) = &p.joints {
                for (jn, v) in j.iter() {
                    jn.hash(&mut h);
                    bits(&mut h, v);
                }
            }
        }
        if from && let Some(b) = &p.branches {
            b.vert_point.len().hash(&mut h);
            b.chains.len().hash(&mut h);
        }
    }
    h.finish()
}

/// Drops every cached skeleton and clip (timing tests measure a cold
/// build with it).
#[doc(hidden)]
pub fn clear_rig_cache() {
    if let Ok(mut c) = RIG_CACHE.lock() {
        c.clear();
    }
}

fn cache_get(key: u64) -> Option<RigEntry> {
    let c = RIG_CACHE.lock().ok()?;
    c.iter().find(|e| e.0 == key).cloned()
}

fn cache_put(entry: RigEntry) {
    let Ok(mut c) = RIG_CACHE.lock() else { return };
    c.retain(|e| e.0 != entry.0);
    c.push(entry);
    if c.len() > RIG_CACHE_LEN {
        c.remove(0);
    }
}

/// A `prefab` instance of the ops: its name, the prefab model (resolved
/// as the graph resolves it: an open document first, else the catalog
/// file) and the instance transform (translation, rotation, scale).
struct Instance {
    name: String,
    model: std::sync::Arc<Model>,
    t: Vec3,
    r: Quat,
    s: Vec3,
}

thread_local! {
    static RIG_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Runs `f` one prefab level deeper, or not at all past 8 levels (a
/// prefab that instances itself; the graph refuses those already).
fn deeper<T>(f: impl FnOnce() -> T) -> Option<T> {
    let d = RIG_DEPTH.with(|c| c.get());
    if d >= 8 {
        return None;
    }
    RIG_DEPTH.with(|c| c.set(d + 1));
    let out = f();
    RIG_DEPTH.with(|c| c.set(d));
    Some(out)
}

/// Every prefab instance of the ops whose prefab loads (one that doesn't
/// already failed the graph).
fn instances(ops: &[Op]) -> Vec<Instance> {
    ops.iter()
        .filter_map(|o| match o {
            Op::Prefab {
                name,
                prefab,
                translation,
                rotation,
                scale,
                ..
            } => {
                let model = crate::graph::load_prefab_guarded(prefab).ok()?;
                Some(Instance {
                    name: name.clone(),
                    model,
                    t: Vec3::from(*translation),
                    r: crate::quat_of(rotation.unwrap_or([0.0, 0.0, 0.0, 1.0])),
                    s: Vec3::from(scale.unwrap_or([1.0; 3])),
                })
            }
            _ => None,
        })
        .collect()
}

/// The instance names of the ops' `prefab` items.
fn instance_names(ops: &[Op]) -> Vec<&str> {
    ops.iter()
        .filter_map(|o| match o {
            Op::Prefab { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

/// Whether part `n` is a prefab instance's part still riding that
/// instance's own rig (`inst.<skeleton>`): host binds leave it alone.
pub(crate) fn instance_bound(ops: &[Op], n: &str, part: &Part) -> bool {
    let Some(b) = &part.rig else { return false };
    instance_names(ops).iter().any(|i| {
        n.strip_prefix(i).is_some_and(|r| r.starts_with('.'))
            && b.skeleton
                .strip_prefix(i)
                .is_some_and(|r| r.starts_with('.'))
    })
}

/// The rig key of a whole model: its own rig ops and parts, plus each
/// instance's name, transform and (recursively) its prefab's rig key.
fn full_key(ops: &[Op], named: &[(String, std::sync::Arc<Part>)], insts: &[Instance]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rig_key(ops, named).hash(&mut h);
    for i in insts {
        i.name.hash(&mut h);
        for x in
            i.t.to_array()
                .iter()
                .chain(&i.r.to_array())
                .chain(&i.s.to_array())
        {
            x.to_bits().hash(&mut h);
        }
        let sub = deeper(|| {
            let named = named_parts(&i.model);
            full_key(&i.model.ops, &named, &instances(&i.model.ops))
        });
        sub.hash(&mut h);
    }
    h.finish()
}

/// The instance transform's uniform share (meters along a bone, radii,
/// clip translations): the mean of the scale's magnitudes.
fn uniform(s: Vec3) -> f32 {
    (s.x.abs() + s.y.abs() + s.z.abs()) / 3.0
}

/// An instance's copy of a prefab skeleton: renamed `inst.<skeleton>`,
/// heads and tails carried by the instance transform, rest frames turned
/// by its rotation, radii scaled.
fn instance_skeleton(i: &Instance, s: &Skeleton) -> Skeleton {
    let m = |v: Vec3| i.t + i.r * (i.s * v);
    let u = uniform(i.s);
    Skeleton {
        name: format!("{}.{}", i.name, s.name),
        bones: s
            .bones
            .iter()
            .map(|b| Bone {
                name: b.name.clone(),
                parent: b.parent,
                head: m(b.head),
                tail: m(b.tail),
                radius: b.radius * u,
                rest: if i.r == Quat::IDENTITY {
                    b.rest
                } else {
                    (i.r * b.rest).normalize()
                },
                limits: b.limits.clone(),
                look_at: b.look_at.as_ref().map(|l| LookAt {
                    target: match l.target {
                        LookTarget::Point(p) => LookTarget::Point(m(p)),
                        LookTarget::Bone(k) => LookTarget::Bone(k),
                    },
                    axis: l.axis,
                    weight: l.weight,
                }),
            })
            .collect(),
        auto: s.auto.clone(),
    }
}

/// An instance's copy of a prefab clip: renamed `inst.<clip>` on
/// `inst.<skeleton>`, translations scaled with the instance, a wind
/// sway's rig carried into the instance's place and a spring's baked
/// world-aligned rotations turned with it.
fn instance_clip(i: &Instance, c: &Clip) -> Clip {
    let u = uniform(i.s);
    let scale_channel = |ch: &Channel| match ch {
        Channel::Keys { keys, interp } => Channel::Keys {
            keys: keys
                .iter()
                .map(|(t, v)| (*t, [v[0] * u, v[1] * u, v[2] * u, v[3]]))
                .collect(),
            interp: *interp,
        },
        Channel::Wave {
            axis,
            amount,
            period,
            phase,
        } => Channel::Wave {
            axis: *axis,
            amount: amount * u,
            period: *period,
            phase: *phase,
        },
        e @ Channel::Euler { .. } => e.clone(),
    };
    let layer = c.layer.as_ref().map(|l| {
        std::sync::Arc::new(match l.as_ref() {
            sway::Layer::Wind(rig) => {
                let mut rig = rig.clone();
                let m = |v: [f32; 3]| (i.t + i.r * (i.s * Vec3::from(v))).to_array();
                for b in &mut rig.bones {
                    b.head = m(b.head);
                    b.tail = m(b.tail);
                    b.radius *= u;
                }
                rig.clip = rig.clip.map(|n| format!("{}.{n}", i.name));
                sway::Layer::Wind(rig)
            }
            sway::Layer::Spring(frames) => sway::Layer::Spring(
                frames
                    .iter()
                    .map(|f| {
                        f.iter()
                            .map(|q| (i.r * *q * i.r.inverse()).normalize())
                            .collect()
                    })
                    .collect(),
            ),
        })
    });
    Clip {
        name: format!("{}.{}", i.name, c.name),
        skeleton: format!("{}.{}", i.name, c.skeleton),
        duration: c.duration,
        looping: c.looping,
        tracks: c
            .tracks
            .iter()
            .map(|(b, tr, m, sh)| {
                let mut tr = tr.clone();
                tr.translate = tr.translate.as_ref().map(scale_channel);
                (*b, tr, *m, *sh)
            })
            .collect(),
        base: c.base.as_ref().map(|b| Box::new(instance_clip(i, b))),
        layer,
        fps: c.fps,
        ik: c
            .ik
            .iter()
            .map(|k| {
                let m = |v: Vec3| i.t + i.r * (i.s * v);
                let mut k = k.clone();
                if let Channel::Keys { keys, .. } = &mut k.target {
                    for (_, v) in keys.iter_mut() {
                        let p = m(Vec3::new(v[0], v[1], v[2]));
                        *v = [p.x, p.y, p.z, 0.0];
                    }
                }
                k.pole = k.pole.map(m);
                if i.r != Quat::IDENTITY {
                    k.rotate = k.rotate.map(|c| match c {
                        Channel::Keys { keys, interp } => Channel::Keys {
                            keys: keys
                                .into_iter()
                                .map(|(t, q)| {
                                    let q = (i.r * Quat::from_array(q) * i.r.inverse()).normalize();
                                    (t, q.to_array())
                                })
                                .collect(),
                            interp,
                        },
                        Channel::Wave {
                            axis,
                            amount,
                            period,
                            phase,
                        } => Channel::Wave {
                            axis: (i.r * Vec3::from(axis)).to_array(),
                            amount,
                            period,
                            phase,
                        },
                        // Euler angles about world axes don't turn with
                        // the instance (a known limit).
                        e @ Channel::Euler { .. } => e,
                    });
                }
                k
            })
            .collect(),
        layering: c.layering.clone(),
        root_motion: c.root_motion.clone(),
        events: c.events.clone(),
        pose: c.pose,
        morphs: c
            .morphs
            .iter()
            .map(|m| MorphTrack {
                part: format!("{}.{}", i.name, m.part),
                ..m.clone()
            })
            .collect(),
        speed: c.speed.map(|v| {
            let w = i.r * Vec3::new(v[0], 0.0, v[1]) * i.s;
            [w.x, w.z]
        }),
        report: c.report.clone(),
    }
}

/// The skeletons and clips every instance brings, built from its prefab's
/// own ops on its own parts (so bone indices match the copied bindings),
/// then carried into the instance.
fn instance_rigs(
    insts: &[Instance],
    with_clips: bool,
) -> Result<(Vec<Skeleton>, Vec<(Clip, Skeleton)>), ModelerError> {
    let (mut skels, mut clips) = (Vec::new(), Vec::new());
    for i in insts {
        let ctx = |e: ModelerError| -> ModelerError {
            format!("prefab instance `{}`: {e}", i.name).into()
        };
        let Some(built) = deeper(|| -> Result<_, ModelerError> {
            let named = named_parts(&i.model);
            let (_, s) = skeletons_on(&i.model.ops, &named)?;
            let c = if with_clips {
                Some(clips_on(&i.model.ops, &named)?)
            } else {
                None
            };
            Ok((s, c))
        }) else {
            continue;
        };
        let (s, c) = built.map_err(ctx)?;
        let mine: Vec<Skeleton> = s.iter().map(|k| instance_skeleton(i, k)).collect();
        if let Some(c) = c {
            for (clip, _) in c.iter() {
                let ic = instance_clip(i, clip);
                if let Some(k) = mine.iter().find(|k| k.name == ic.skeleton) {
                    clips.push((ic, k.clone()));
                }
            }
        }
        skels.extend(mine);
    }
    Ok((skels, clips))
}

/// Skeletons built on `named` (cached by content), the ops' own first,
/// then those prefab instances bring (`inst.<skeleton>`).
fn skeletons_on(
    ops: &[Op],
    named: &[(String, std::sync::Arc<Part>)],
) -> Result<(u64, std::sync::Arc<Vec<Skeleton>>), ModelerError> {
    let insts = instances(ops);
    let key = full_key(ops, named, &insts);
    if let Some(e) = cache_get(key) {
        return Ok((key, e.1));
    }
    let mut out = Vec::new();
    for (name, def) in skeleton_defs(ops) {
        out.push(build(&name, &def, named, ops)?);
    }
    out.extend(instance_rigs(&insts, false)?.0);
    let out = std::sync::Arc::new(out);
    cache_put((key, out.clone(), None));
    Ok((key, out))
}

/// Skeletons and every clip built on `named` (cached by content): the
/// ops' own clips, then those prefab instances bring (`inst.<clip>`).
fn clips_on(
    ops: &[Op],
    named: &[(String, std::sync::Arc<Part>)],
) -> Result<std::sync::Arc<Vec<(Clip, Skeleton)>>, ModelerError> {
    let (key, skels) = skeletons_on(ops, named)?;
    if let Some((_, _, Some(c))) = cache_get(key) {
        return Ok(c);
    }
    let mut all = build_all(ops, &skels, named)?;
    let insts = instances(ops);
    if !insts.is_empty() {
        let theirs = instance_rigs(&insts, true)?.1;
        for (c, k) in theirs {
            if all.iter().any(|(a, _)| a.name == c.name) {
                return Err(format!(
                    "clip `{}`: a prefab instance brings a clip of that name: rename one",
                    c.name
                )
                .into());
            }
            all.push((c, k));
        }
    }
    let all = std::sync::Arc::new(all);
    cache_put((key, skels, Some(all.clone())));
    Ok(all)
}

/// Every clip name of the model, its own and its prefab instances'
/// (`inst.<clip>`), in [`clips`] order; empty when the rig doesn't build.
pub fn clip_names(model: &Model) -> Vec<String> {
    clips(model)
        .map(|v| v.into_iter().map(|(c, _)| c.name).collect())
        .unwrap_or_default()
}

/// Whether the model has any rig: skeletons of its own or parts bound to
/// a prefab instance's rig.
pub fn has_rig(model: &Model) -> bool {
    has_skeleton(&model.ops) || model.parts.iter().any(|p| p.rig.is_some())
}

fn named_parts(model: &Model) -> Vec<(String, std::sync::Arc<Part>)> {
    model
        .parts
        .iter()
        .map(|p| (p.name.clone(), p.clone()))
        .collect()
}

/// Every skeleton of the model, built on its finished parts.
pub fn skeletons(model: &Model) -> Result<Vec<Skeleton>, ModelerError> {
    Ok(skeletons_on(&model.ops, &named_parts(model))?
        .1
        .as_ref()
        .clone())
}

/// Every skeleton the ops declare: `skeleton` items, plus `<body>_rig`
/// (from the body) for each `body` item unless it says `rig: false` or a
/// skeleton item already has that name.
pub fn skeleton_defs(ops: &[Op]) -> Vec<(String, SkeletonDef)> {
    let mut out: Vec<(String, SkeletonDef)> = ops
        .iter()
        .filter_map(|o| match o {
            Op::Skeleton { name, def } => Some((name.clone(), def.clone())),
            _ => None,
        })
        .collect();
    for o in ops {
        if let Op::Body { name, def } = o
            && def.rig != Some(false)
        {
            let rig = format!("{name}_rig");
            if !out.iter().any(|(n, _)| *n == rig) {
                out.push((
                    rig,
                    SkeletonDef {
                        from: Some(name.clone()),
                        ..SkeletonDef::default()
                    },
                ));
            }
        }
    }
    out
}

/// Every bind the ops declare: first each body's automatic smooth bind
/// (`<body>_bind` onto `<body>_rig`, unless `bind: false`, `rig: false` or
/// a `bind` item already targets that rig), then the `bind` items in order
/// (so an item wins over the automatic one).
pub fn bind_defs(ops: &[Op]) -> Vec<(String, BindDef)> {
    let mut out = Vec::new();
    for o in ops {
        if let Op::Body { name, def } = o
            && def.rig != Some(false)
            && def.bind != Some(false)
        {
            let rig = format!("{name}_rig");
            if !ops
                .iter()
                .any(|o| matches!(o, Op::Bind { def, .. } if def.skeleton == rig && def.weights_from.is_none()))
            {
                out.push((
                    format!("{name}_bind"),
                    BindDef {
                        skeleton: rig,
                        parts: crate::body::outputs(name, def),
                        bone: None,
                        mode: "smooth".into(),
                        blend: crate::body::bind_blend(def),
                        paint: Vec::new(),
                        weights_from: None,
                        correctives: None,
                    },
                ));
            }
        }
    }
    for o in ops {
        if let Op::Bind { name, def } = o {
            out.push((name.clone(), def.clone()));
        }
    }
    out
}

/// Whether the ops make any skeleton (items or a body's rig).
pub fn has_skeleton(ops: &[Op]) -> bool {
    ops.iter().any(|o| {
        matches!(o, Op::Skeleton { .. })
            || matches!(o, Op::Body { def, .. } if def.rig != Some(false))
    })
}

/// Every clip of the model with its skeleton.
/// Poses (static clips) are left out: [`clip`] reaches them by name.
pub fn clips(model: &Model) -> Result<Vec<(Clip, Skeleton)>, ModelerError> {
    Ok(clips_on(&model.ops, &named_parts(model))?
        .iter()
        .filter(|(c, _)| !c.pose)
        .cloned()
        .collect())
}

/// Poses first (as static clips marked `pose`), then keyed clips (each
/// after the clip it layers `over`), then sways (which may layer over a
/// keyed clip), in op order within each.
fn build_all(
    ops: &[Op],
    skels: &[Skeleton],
    parts: &[(String, std::sync::Arc<Part>)],
) -> Result<Vec<(Clip, Skeleton)>, String> {
    let mut poses: Vec<BuiltPose> = Vec::new();
    let mut pose_clips: Vec<(Clip, Skeleton)> = Vec::new();
    for op in ops {
        if let Op::Pose { name, def } = op {
            let Some(s) = skels.iter().find(|s| s.name == def.skeleton) else {
                return Err(format!("pose `{name}`: no skeleton `{}`", def.skeleton));
            };
            let p = build_pose(name, def, s, parts)?;
            pose_clips.push((p.as_clip(s), s.clone()));
            poses.push(p);
        }
    }
    let defs: Vec<(&String, &ClipDef)> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Clip { name, def } => Some((name, def)),
            _ => None,
        })
        .collect();
    let mut built: Vec<Option<(Clip, Skeleton)>> = vec![None; defs.len()];
    // Layers after their bases: passes until nothing more builds.
    loop {
        let mut progress = false;
        for (k, (name, def)) in defs.iter().enumerate() {
            if built[k].is_some() {
                continue;
            }
            let Some(s) = skels.iter().find(|s| s.name == def.skeleton) else {
                return Err(format!("clip `{name}`: no skeleton `{}`", def.skeleton));
            };
            let base = match &def.over {
                None => None,
                Some(o) => {
                    let Some(j) = defs.iter().position(|(n, _)| *n == o) else {
                        return Err(format!(
                            "clip `{name}`: over `{o}`: no such clip (a layer goes over a keyed clip)"
                        ));
                    };
                    match &built[j] {
                        None => continue,
                        Some((c, _)) => {
                            if c.skeleton != s.name {
                                return Err(format!(
                                    "clip `{name}`: over `{o}` animates skeleton `{}`, not `{}`",
                                    c.skeleton, s.name
                                ));
                            }
                            Some(c.clone())
                        }
                    }
                }
            };
            let clip = if def.mocap.is_some() {
                let (lowered, low) = crate::mocap::lower(name, def, s)?;
                let mut c = build_clip_with(name, &lowered, s, parts, &poses, base)?;
                crate::mocap::finish(&mut c, s, &low);
                c
            } else {
                build_clip_with(name, def, s, parts, &poses, base)?
            };
            built[k] = Some((clip, s.clone()));
            progress = true;
        }
        if !progress {
            break;
        }
    }
    if let Some(k) = built.iter().position(Option::is_none) {
        return Err(format!("clip `{}`: `over` clips form a cycle", defs[k].0));
    }
    let mut out: Vec<(Clip, Skeleton)> = built.into_iter().flatten().collect();
    for (c, _) in &pose_clips {
        if out.iter().any(|(o, _)| o.name == c.name) {
            return Err(format!(
                "pose `{}`: a clip is already named `{}`: rename one",
                c.name, c.name
            ));
        }
    }
    let mut sways = Vec::new();
    for op in ops {
        if let Op::Sway { name, def } = op {
            let Some(s) = skels.iter().find(|s| s.name == def.skeleton) else {
                return Err(format!("sway `{name}`: no skeleton `{}`", def.skeleton));
            };
            if out.iter().chain(&pose_clips).any(|(c, _)| &c.name == name) {
                return Err(format!(
                    "sway `{name}`: a clip is already named `{name}`: rename one"
                ));
            }
            let base = match &def.over {
                None => None,
                Some(o) => {
                    let Some((c, _)) = out.iter().find(|(c, _)| &c.name == o) else {
                        return Err(format!("sway `{name}`: over `{o}`: no such clip"));
                    };
                    if c.skeleton != s.name {
                        return Err(format!(
                            "sway `{name}`: over `{o}` animates skeleton `{}`, not `{}`",
                            c.skeleton, s.name
                        ));
                    }
                    Some(c.clone())
                }
            };
            sways.push((sway::build(name, def, s, base)?, s.clone()));
        }
    }
    out.extend(sways);
    out.extend(pose_clips);
    Ok(out)
}

/// The named clip (or, with `None`, the only clip) and its skeleton.
pub fn clip(model: &Model, name: Option<&str>) -> Result<(Clip, Skeleton), ModelerError> {
    let arc = clips_on(&model.ops, &named_parts(model))?;
    let names: Vec<String> = arc
        .iter()
        .map(|(c, _)| {
            if c.pose {
                format!("pose {}", c.name)
            } else {
                c.name.clone()
            }
        })
        .collect();
    let all: Vec<(Clip, Skeleton)> = match name {
        // Clone only the clip asked for (a pose too, as a static clip).
        Some(n) => arc.iter().filter(|(c, _)| c.name == n).cloned().collect(),
        None => arc.iter().filter(|(c, _)| !c.pose).cloned().collect(),
    };
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
    let weights = clip.morph_weights(t);
    model
        .parts
        .iter()
        .map(|p| {
            let w = part_weights(&weights, &p.name);
            let morphed = skin::morphed(p, &w).map(|(verts, normals)| Part {
                verts,
                normals,
                morphs: None,
                ..(**p).clone()
            });
            let src: &Part = morphed.as_ref().unwrap_or(p);
            match &p.rig {
                Some(b) if b.skeleton == skel.name => {
                    std::sync::Arc::new(skin::pose_part(src, b, &mats))
                }
                _ => morphed.map_or_else(|| p.clone(), std::sync::Arc::new),
            }
        })
        .collect()
}

/// The non-zero weights of `part` among a clip's [`Clip::morph_weights`]:
/// (shape index, weight).
pub fn part_weights(weights: &[(String, usize, f32)], part: &str) -> Vec<(usize, f32)> {
    weights
        .iter()
        .filter(|w| w.0 == part && w.2 != 0.0)
        .map(|w| (w.1, w.2))
        .collect()
}

/// After the graph: builds skeletons and applies every bind to the
/// finished parts (in place on `named`).
pub fn apply_binds(
    ops: &[Op],
    named: &mut [(String, std::sync::Arc<Part>)],
) -> Result<(), ModelerError> {
    if !ops.iter().any(|o| {
        matches!(
            o,
            Op::Bind { .. }
                | Op::Skeleton { .. }
                | Op::Clip { .. }
                | Op::Pose { .. }
                | Op::Sway { .. }
        )
    }) && !has_skeleton(ops)
    {
        return Ok(());
    }
    // Clips are checked here so a bad track fails (and rolls back) the batch.
    clips_on(ops, named)?;
    let (_, skels) = skeletons_on(ops, named)?;
    let mut defs = bind_defs(ops);
    defs.sort_by_key(|d| d.1.weights_from.is_some()); // sources bind first
    for (name, def) in &defs {
        let ctx = format!("bind `{name}`");
        let src = def
            .weights_from
            .as_ref()
            .map(|f| {
                named
                    .iter()
                    .find(|p| &p.0 == f)
                    .map(|p| p.1.clone())
                    .ok_or_else(|| format!("{ctx}: weights_from: no part `{f}`"))
            })
            .transpose()?;
        let Some(skel) = skels.iter().find(|s| s.name == def.skeleton) else {
            return Err(format!("{ctx}: no skeleton `{}`", def.skeleton).into());
        };
        let mut hit = 0;
        for (n, part) in named.iter_mut() {
            if !def.parts.iter().any(|q| crate::name_matches(q, n)) {
                continue;
            }
            // A prefab instance's part keeps its own rig.
            if instance_bound(ops, n, part) {
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
                "auto" if exact_fits(skel, part, n) => exact(skel, part, name),
                _ if def.paint.is_empty() && carried(skel, part).is_some() => {
                    let (joints, weights) = carried(skel, part).unwrap_or_default();
                    Binding {
                        skeleton: skel.name.clone(),
                        bind: name.clone(),
                        bone: None,
                        joints,
                        weights,
                    }
                }
                _ if src.is_some() => src
                    .as_deref()
                    .map_or(Err(String::new()), |s| {
                        skin::transfer_weights(part, s, skel, def, name)
                    })
                    .map_err(|e| format!("{ctx}: {e}"))?,
                _ => skin::smooth_weights(part, skel, def, name)
                    .map_err(|e| format!("{ctx}: {e}"))?,
            };
            let fix = match &src {
                Some(s) => skin::transfer_correctives(part, s),
                None => skin::correctives(part, skel, &binding, def)
                    .map_err(|e| format!("{ctx}: {e}"))?,
            };
            let p = std::sync::Arc::make_mut(part);
            p.rig = Some(std::sync::Arc::new(binding));
            p.correctives = fix;
        }
        // A body's automatic bind quietly skips a renamed or deleted body.
        let item = ops
            .iter()
            .any(|o| matches!(o, Op::Bind { name: n, .. } if n == name));
        if hit == 0 && item {
            return Err(format!("{ctx}: no part matches {:?}", def.parts).into());
        }
    }
    Ok(())
}

/// The skin weights a part came with (the human template's), mapped onto
/// `skel`'s bones: only when every bone they name is in it and the vertex
/// count still matches (an op that rebuilt the mesh drops them).
fn carried(skel: &Skeleton, part: &Part) -> Option<(Vec<[u16; 4]>, Vec<[f32; 4]>)> {
    let s = part.skin.as_ref()?;
    if s.joints.len() != part.verts.len() {
        return None;
    }
    let map: Vec<u16> = s
        .bones
        .iter()
        .map(|b| skel.bone(b).map(|i| i as u16))
        .collect::<Option<Vec<_>>>()?;
    Some((
        s.joints
            .iter()
            .map(|j| j.map(|b| map.get(b as usize).copied().unwrap_or(0)))
            .collect(),
        s.weights.clone(),
    ))
}

/// Whether `part` (named `n`) is the wood or leaves of the lsystem `skel`
/// was grown from, with its branch record intact.
fn exact_fits(skel: &Skeleton, part: &Part, n: &str) -> bool {
    let (Some(map), Some(br)) = (&skel.auto, &part.branches) else {
        return false;
    };
    (n == map.from || n.strip_suffix("_leaves") == Some(map.from.as_str()))
        && br.vert_point.len() == part.verts.len()
        && br.chains.len() == map.point.len()
}

/// Exact weights from the branch record: each vertex rides the bones of
/// the chain point it hangs on.
fn exact(skel: &Skeleton, part: &Part, bind: &str) -> Binding {
    let mut joints = Vec::with_capacity(part.verts.len());
    let mut weights = Vec::with_capacity(part.verts.len());
    let map = skel.auto.as_deref();
    for &(c, p) in part
        .branches
        .as_ref()
        .map_or(&[][..], |b| &b.vert_point[..])
    {
        let w = map
            .and_then(|m| m.point.get(c as usize)?.get(p as usize))
            .copied()
            .unwrap_or([(0, 1.0), (0, 0.0)]);
        if w[1].1 > 0.0 && w[1].0 != w[0].0 {
            joints.push([w[0].0, w[1].0, 0, 0]);
            weights.push([w[0].1, w[1].1, 0.0, 0.0]);
        } else {
            joints.push([w[0].0, 0, 0, 0]);
            weights.push([1.0, 0.0, 0.0, 0.0]);
        }
    }
    Binding {
        skeleton: skel.name.clone(),
        bind: bind.to_string(),
        bone: None,
        joints,
        weights,
    }
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
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
                },
                BoneDef {
                    name: "lower".into(),
                    parent: Some("upper".into()),
                    head: PointRef::Point([1.0, 0.0, 0.0]),
                    tail: Some(PointRef::Point([2.0, 0.0, 0.0])),
                    radius: None,
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
                },
            ],
            ..Default::default()
        };
        build("arm", &def, &[], &[]).unwrap_or_else(|e| panic!("{e}"))
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
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
                },
                BoneDef {
                    name: "leg_l".into(),
                    parent: Some("hip".into()),
                    head: PointRef::Point([0.2, 1.0, 0.0]),
                    tail: Some(PointRef::Point([0.2, 0.0, 0.0])),
                    radius: None,
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
                },
            ],
            mirror: Some("x".into()),
            ..Default::default()
        };
        let s = build("r", &def, &[], &[]).unwrap_or_else(|e| panic!("{e}"));
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
            ..Default::default()
        };
        let c = build_clip("swing", &def, &s, &[]).unwrap_or_else(|e| panic!("{e}"));
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
        let c2 = build_clip("s", &def2, &s, &[]).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            c2.sample(&s, 0.5)[0]
                .1
                .angle_between(Quat::from_rotation_x(40f32.to_radians()))
                < 1e-3
        );
    }

    fn session(items: serde_json::Value) -> crate::commands::Session {
        let mut s = crate::commands::Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!()
        };
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn lsystem_auto_rig_and_wind() {
        let s = session(serde_json::json!([
            {"type": "lsystem", "name": "oak", "preset": "tree", "seed": 2},
            {"type": "skeleton", "name": "oak_rig", "from": "oak"},
            {"type": "bind", "name": "oak_bind", "skeleton": "oak_rig", "parts": ["oak", "oak_leaves"], "mode": "auto"},
            {"type": "sway", "name": "wind", "skeleton": "oak_rig", "strength": 1.0, "duration": 4}
        ]));
        let (clip, skel) = clip(&s.model, Some("wind")).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            skel.bones.len() > 4 && skel.bones.len() <= 128,
            "{}",
            skel.bones.len()
        );
        assert!(skel.auto.is_some());
        for name in ["oak", "oak_leaves"] {
            let p = s
                .model
                .parts
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("no {name}"));
            let b = p.rig.as_ref().unwrap_or_else(|| panic!("{name} unbound"));
            assert_eq!(b.weights.len(), p.verts.len(), "{name}");
            for w in &b.weights {
                assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{w:?}");
            }
        }
        // Loops, and moves the crown more than the base.
        let a = clip.sample(&skel, 0.0);
        let b = clip.sample(&skel, 4.0 - 1e-4);
        for (x, y) in a.iter().zip(&b) {
            assert!(x.1.angle_between(y.1) < 2e-3);
        }
        let rest = posed_parts(&s.model, &clip, &skel, 0.0);
        let later = posed_parts(&s.model, &clip, &skel, 1.3);
        let wood = |v: &[std::sync::Arc<Part>]| {
            v.iter()
                .find(|p| p.name == "oak")
                .cloned()
                .unwrap_or_else(|| panic!("no oak"))
        };
        let (r, l) = (wood(&rest), wood(&later));
        let mut top = 0.0f32;
        let mut bottom = 0.0f32;
        for (p, q) in r.verts.iter().zip(&l.verts) {
            let d = Vec3::from(*p).distance(Vec3::from(*q));
            if p[1] > 3.0 {
                top = top.max(d);
            } else if p[1] < 0.5 {
                bottom = bottom.max(d);
            }
        }
        assert!(
            top > 0.01 && top > bottom * 3.0,
            "top {top} bottom {bottom}"
        );
    }

    #[test]
    fn spring_tail_lags_a_walk_and_loops() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "body", "kind": "box", "size": [0.3, 0.2, 0.5], "translation": [0, 0.5, 0]},
            {"type": "skeleton", "name": "rig", "bones": {
                "body": {"head": [0, 0.5, 0], "tail": [0, 0.5, -0.25]},
                "tail_0": {"parent": "body", "head": [0, 0.5, -0.25], "tail": [0, 0.5, -0.45]},
                "tail_1": {"parent": "tail_0", "head": [0, 0.5, -0.45], "tail": [0, 0.5, -0.65]}}},
            {"type": "bind", "name": "b", "skeleton": "rig", "parts": ["body"], "bone": "body"},
            {"type": "clip", "name": "trot", "skeleton": "rig", "duration": 1.0,
             "tracks": {"body": {"translate": {"wave": {"axis": "x", "amount": 0.1}}}}},
            {"type": "sway", "name": "tail_spring", "skeleton": "rig", "kind": "spring", "over": "trot",
             "bones": ["tail*"], "stiffness": 60, "damping": 4}
        ]));
        let (clip, skel) = clip(&s.model, Some("tail_spring")).unwrap_or_else(|e| panic!("{e}"));
        let t1 = skel.bone("tail_1").unwrap_or(0);
        let swing = (0..20)
            .map(|k| {
                clip.sample(&skel, k as f32 * 0.05)[t1]
                    .1
                    .angle_between(Quat::IDENTITY)
            })
            .fold(0.0f32, f32::max);
        assert!(swing > 0.02, "the tail swings ({swing})");
        // The body keeps its trot.
        assert!((clip.sample(&skel, 0.25)[0].0.x - 0.1).abs() < 1e-3);
        let a = clip.sample(&skel, 0.0);
        let b = clip.sample(&skel, 1.0 - 1e-4);
        assert!(a[t1].1.angle_between(b[t1].1) < 5e-3, "seam");
        // A duration that isn't whole cycles of the driver fails.
        let mut s = s;
        let bad = serde_json::json!([{"type": "sway", "name": "tail_spring", "skeleton": "rig",
            "kind": "spring", "over": "trot", "duration": 1.5}]);
        let serde_json::Value::Array(bad) = bad else {
            panic!()
        };
        let Err(e) = s.apply_values(None, bad) else {
            panic!("applied")
        };
        assert!(e.to_string().contains("whole number"), "{e}");
    }

    #[test]
    fn skin_auto_rig_mirrors() {
        let s = session(serde_json::json!([
            {"type": "skin", "name": "critter", "mirror": "x",
             "joints": {"body": [0, 0.3, 0, 0.12]},
             "chains": [["body", [0.15, 0.15, 0.05, 0.04], [0.2, 0.02, 0.08, 0.03]], ["body", [0, 0.5, 0.1, 0.08]]]},
            {"type": "skeleton", "name": "rig", "from": "critter"},
            {"type": "bind", "name": "b", "skeleton": "rig", "parts": ["critter"], "mode": "auto"}
        ]));
        let skel = skeletons(&s.model).unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<&str> = skel[0].bones.iter().map(|b| b.name.as_str()).collect();
        for n in ["body", "c0_1_l", "c0_1_r", "c0_2_r", "c1_1"] {
            assert!(names.contains(&n), "{n} in {names:?}");
        }
        let r = skel[0].bone("c0_1_r").unwrap_or(0);
        assert_eq!(skel[0].bones[r].parent, skel[0].bone("body"));
        assert!(skel[0].bones[r].head.x < -0.1);
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn no_rest_rotation_poses_as_before() {
        let s = session(serde_json::json!([
            {"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
                "hip": {"head": [0, 1, 0], "tail": [0, 1.2, 0]},
                "leg_l": {"parent": "hip", "head": [0.1, 1, 0], "tail": [0.1, 0.1, 0.05]}}},
            {"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1.0,
             "tracks": {"leg_l": {"rotate": [[0, {"x": 25}], [0.5, {"x": -25}], [1, {"x": 25}]],
                                  "scale": [[0, [1, 1, 1]], [0.5, [1.2, 0.9, 1]], [1, [1, 1, 1]]]},
                        "hip": {"translate": {"wave": {"axis": "y", "amount": 0.03, "period": 0.5}}}},
             "mirror": {"leg_r": {"from": "leg_l", "phase": 0.5}}}
        ]));
        let (c, sk) = clip(&s.model, Some("walk")).unwrap_or_else(|e| panic!("{e}"));
        assert!(!sk.has_rest());
        for t in [0.0, 0.13, 0.5, 0.77] {
            let pose = c.sample(&sk, t);
            // The pre-rest-rotation formulas, bit for bit.
            let mut old: Vec<Mat4> = Vec::new();
            for (i, b) in sk.bones.iter().enumerate() {
                let (tr, r, sc) = pose[i];
                let rl = match b.parent {
                    Some(p) => b.head - sk.bones[p].head,
                    None => b.head,
                };
                let local = Mat4::from_scale_rotation_translation(sc, r, rl + tr);
                old.push(match b.parent {
                    Some(p) => old[p] * local,
                    None => local,
                });
            }
            assert_eq!(sk.world(&pose), old);
            let skin: Vec<Mat4> = old
                .iter()
                .zip(&sk.bones)
                .map(|(w, b)| *w * Mat4::from_translation(-b.head))
                .collect();
            assert_eq!(sk.skin_matrices(&pose), skin);
        }
        // Saved items carry no rest fields.
        let json = serde_json::to_string(&s.model.ops).unwrap_or_default();
        assert!(
            !json.contains("\"rest\"") && !json.contains("\"roll\""),
            "{json}"
        );
    }

    #[test]
    fn rest_rotation_turns_about_local_axes() {
        let s = session(serde_json::json!([
            {"type": "skeleton", "name": "rig", "bones": {
                "a": {"head": [0, 1, 0], "tail": [1, 1, 0], "roll": 0},
                "b": {"head": [0, 2, 0], "tail": [1, 2, 0], "rest": {"y": 90}},
                "c": {"parent": "a", "head": [1, 1, 0], "tail": [1, 1, 1], "roll": 30}}},
            {"type": "clip", "name": "bend", "skeleton": "rig", "duration": 1.0,
             "tracks": {"a": {"rotate": [[0, {"x": 30}], [1, {"x": 30}]]},
                        "b": {"rotate": [[0, {"x": 30}], [1, {"x": 30}]]}}}
        ]));
        let (c, sk) = clip(&s.model, Some("bend")).unwrap_or_else(|e| panic!("{e}"));
        let (a, b, cb) = (&sk.bones[0], &sk.bones[1], &sk.bones[2]);
        // roll: the frame's +y runs along the bone.
        assert!(close(a.rest * Vec3::Y, Vec3::X), "{:?}", a.rest * Vec3::Y);
        assert!(close(cb.rest * Vec3::Y, Vec3::Z));
        assert!(close(b.rest * Vec3::X, -Vec3::Z));
        // At rest nothing moves and the inverse binds undo the rest frames.
        let rest = sk.rest();
        for (m, w) in sk.skin_matrices(&rest).iter().zip(sk.world(&rest)) {
            assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-5), "{m:?}");
            let _ = w;
        }
        for (w, bn) in sk.world(&rest).iter().zip(&sk.bones) {
            assert!((*w * bn.inverse_bind()).abs_diff_eq(Mat4::IDENTITY, 1e-5));
        }
        let pose = c.sample(&sk, 0.5);
        let segs = sk.posed_segments(&pose);
        for (i, bn) in [(0usize, a), (1, b)] {
            let axis = bn.rest * Vec3::X;
            let want =
                bn.head + Quat::from_axis_angle(axis, 30f32.to_radians()) * (bn.tail - bn.head);
            assert!(
                close(segs[i].1, want),
                "{}: {:?} vs {want:?}",
                bn.name,
                segs[i].1
            );
        }
        // The child rides its parent's local turn.
        let axis = a.rest * Vec3::X;
        let q = Quat::from_axis_angle(axis, 30f32.to_radians());
        let want = a.head + q * (cb.tail - a.head);
        assert!(close(segs[2].1, want), "{:?} vs {want:?}", segs[2].1);
        // inspect pose reports the turn from rest, not the rest frame.
        let (_, j) = check::pose_report(&s.model, Some("bend"), 0.5, Some(&["c".into()]))
            .unwrap_or_else(|e| panic!("{e}"));
        let r = &j["bones"][0]["rotation"];
        let got = Quat::from_xyzw(
            r[0].as_f64().unwrap_or(0.0) as f32,
            r[1].as_f64().unwrap_or(0.0) as f32,
            r[2].as_f64().unwrap_or(0.0) as f32,
            r[3].as_f64().unwrap_or(1.0) as f32,
        );
        assert!(got.angle_between(q) < 1e-3, "{got:?} vs {q:?}");
    }

    #[test]
    fn mirror_twins_get_the_mirrored_rest_frame() {
        let s = session(serde_json::json!([
            {"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
                "hip": {"head": [0, 1, 0], "tail": [0, 1.2, 0]},
                "arm_l": {"parent": "hip", "head": [0.2, 1.1, 0], "tail": [0.6, 1.3, 0.1], "roll": 20}}},
            {"type": "clip", "name": "flap", "skeleton": "rig", "duration": 1.0,
             "tracks": {"arm_l": {"rotate": [[0, {"x": 10}], [0.5, {"x": 40, "z": 15}], [1, {"x": 10}]],
                                  "translate": [[0, [0, 0, 0]], [0.5, [0.02, 0.03, -0.01]], [1, [0, 0, 0]]]}},
             "mirror": {"arm_r": {"from": "arm_l", "phase": 0.0}}}
        ]));
        let (c, sk) = clip(&s.model, Some("flap")).unwrap_or_else(|e| panic!("{e}"));
        let l = &sk.bones[sk.bone("arm_l").unwrap_or(0)];
        let r = &sk.bones[sk.bone("arm_r").unwrap_or(0)];
        let q = l.rest;
        assert_eq!(r.rest, Quat::from_xyzw(q.x, -q.y, -q.z, q.w));
        assert!(close(r.rest * Vec3::Y, (r.tail - r.head).normalize()));
        let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
        for t in [0.0, 0.3, 0.5] {
            let segs = sk.posed_segments(&c.sample(&sk, t));
            let (il, ir) = (sk.bone("arm_l").unwrap_or(0), sk.bone("arm_r").unwrap_or(0));
            assert!(close(segs[ir].0, flip(segs[il].0)), "t {t}");
            assert!(
                close(segs[ir].1, flip(segs[il].1)),
                "t {t}: {:?} vs {:?}",
                segs[ir].1,
                flip(segs[il].1)
            );
        }
    }

    #[test]
    fn cached_rig_matches_a_fresh_build() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "arm", "kind": "box", "size": [1.0, 0.1, 0.1], "translation": [0.5, 1.0, 0.0]},
            {"type": "skeleton", "name": "rig", "bones": {
                "shoulder": {"head": "arm:left", "tail": "arm:right"},
                "tip": {"parent": "shoulder", "head": "arm:right"}}},
            {"type": "clip", "name": "raise", "skeleton": "rig", "duration": 1.0,
             "tracks": {"shoulder": {"rotate": [[0, {"z": 0}], [1, {"z": 90}]]}}},
            {"type": "sway", "name": "spring", "skeleton": "rig", "kind": "spring", "over": "raise", "duration": 1.0}
        ]));
        let named = named_parts(&s.model);
        let mut fresh = Vec::new();
        for (n, d) in skeleton_defs(&s.model.ops) {
            fresh.push(build(&n, &d, &named, &s.model.ops).unwrap_or_else(|e| panic!("{e}")));
        }
        let built = build_all(&s.model.ops, &fresh, &named).unwrap_or_else(|e| panic!("{e}"));
        for _ in 0..2 {
            let all = clips(&s.model).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(all.len(), built.len());
            for ((c, sk), (c2, sk2)) in all.iter().zip(&built) {
                assert_eq!(sk, sk2);
                for t in [0.0, 0.25, 0.6] {
                    assert_eq!(c.sample(sk, t), c2.sample(sk2, t));
                }
            }
        }
        // Moving the anchor part changes the key: the rig follows.
        let mut m = s.model.clone();
        if let Some(p) = m.parts.iter_mut().find(|p| p.name == "arm") {
            std::sync::Arc::make_mut(p).translation[1] = 2.0;
        }
        let (_, sk) = clip(&m, Some("raise")).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            (sk.bones[0].head.y - 2.0).abs() < 1e-5,
            "{:?}",
            sk.bones[0].head
        );
    }
}
