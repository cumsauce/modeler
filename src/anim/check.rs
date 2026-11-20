//! Rig summaries, lints and `verify` checks for skeletons and clips.

use bevy::math::Vec3;

use super::{Clip, Skeleton};
use crate::{Model, Op};

/// The `rig:` lines of `scene_summary` (none without rig items).
pub fn summary(model: &Model) -> Option<String> {
    let mut lines = Vec::new();
    let skels = super::skeletons(model).ok()?;
    for s in &skels {
        let all = super::bind_defs(&model.ops);
        let binds: Vec<String> = all
            .iter()
            .filter_map(|(name, def)| match () {
                () if def.skeleton == s.name => {
                    let n = model
                        .parts
                        .iter()
                        .filter(|p| p.rig.as_ref().is_some_and(|b| &b.bind == name))
                        .count();
                    Some(format!("`{name}` {} ({n} part(s))", def.mode))
                }
                _ => None,
            })
            .collect();
        // A prefab instance's rig: its parts' bindings and its clips.
        let mut binds = binds;
        let mut inst_clips = Vec::new();
        if !model
            .ops
            .iter()
            .any(|o| matches!(o, Op::Skeleton { name, .. } if *name == s.name))
            && super::instance_names(&model.ops)
                .iter()
                .any(|i| s.name.strip_prefix(i).is_some_and(|r| r.starts_with('.')))
        {
            let mut names: Vec<&str> = model
                .parts
                .iter()
                .filter_map(|p| p.rig.as_ref().filter(|b| b.skeleton == s.name))
                .map(|b| b.bind.as_str())
                .collect();
            names.sort();
            names.dedup();
            for n in names {
                let c = model
                    .parts
                    .iter()
                    .filter(|p| p.rig.as_ref().is_some_and(|b| b.bind == n))
                    .count();
                binds.push(format!("`{n}` from the prefab ({c} part(s))"));
            }
            if let Ok(all) = super::clips(model) {
                inst_clips = all
                    .iter()
                    .filter(|(c, _)| c.skeleton == s.name)
                    .map(|(c, _)| {
                        format!(
                            "`{}` {}s{}",
                            c.name,
                            c.duration,
                            if c.looping { " loop" } else { "" }
                        )
                    })
                    .collect();
            }
        }
        let mut clips: Vec<String> = model
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::Clip { name, def } if def.skeleton == s.name => Some(format!(
                    "`{name}` {}{} ({} track(s){}){}{}{}",
                    if def.duration > 0.0 {
                        format!("{}s", def.duration)
                    } else {
                        "base-length".into()
                    },
                    if def.looping { " loop" } else { "" },
                    def.tracks.len() + def.mirror.len(),
                    if def.poses.is_empty() {
                        String::new()
                    } else {
                        format!(", {} pose key(s)", def.poses.len())
                    },
                    def.over
                        .as_ref()
                        .map(|o| format!(
                            " {} over `{o}`",
                            if def.additive { "additive" } else { "layer" }
                        ))
                        .unwrap_or_default(),
                    if def.root_motion.is_some() {
                        " root motion"
                    } else {
                        ""
                    },
                    if def.events.is_empty() {
                        String::new()
                    } else {
                        format!(" {} event(s)", def.events.len())
                    }
                )),
                Op::Pose { name, def } if def.skeleton == s.name => Some(format!(
                    "pose `{name}` ({} bone(s){}{})",
                    def.bones.len(),
                    if def.mirror { ", mirrored" } else { "" },
                    if def.ik.is_empty() {
                        String::new()
                    } else {
                        format!(", {} ik", def.ik.len())
                    }
                )),
                Op::Sway { name, def } if def.skeleton == s.name => Some(format!(
                    "`{name}` {} sway{}",
                    match def.motion {
                        modeler_anim::SwayKind::Wind(_) => "wind",
                        modeler_anim::SwayKind::Spring(_) => "spring",
                    },
                    def.over
                        .as_ref()
                        .map(|o| format!(" over `{o}`"))
                        .unwrap_or_default()
                )),
                _ => None,
            })
            .collect();
        clips.extend(inst_clips);
        lines.push(format!(
            "rig: skeleton `{}` ({} bones: {}); binds: {}; clips: {}",
            s.name,
            s.bones.len(),
            crate::compact_names(&s.bones.iter().map(|b| b.name.clone()).collect::<Vec<_>>())
                .into_iter()
                .take(12)
                .collect::<Vec<_>>()
                .join(", "),
            if binds.is_empty() {
                "none".into()
            } else {
                binds.join(", ")
            },
            if clips.is_empty() {
                "none".into()
            } else {
                clips.join(", ")
            }
        ));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Rig warnings for `scene_summary`.
pub fn lints(model: &Model) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(skels) = super::skeletons(model) else {
        return out;
    };
    for s in &skels {
        if !super::bind_defs(&model.ops)
            .iter()
            .any(|(_, def)| def.skeleton == s.name)
            && !model
                .parts
                .iter()
                .any(|p| p.rig.as_ref().is_some_and(|b| b.skeleton == s.name))
        {
            out.push(format!(
                "skeleton `{}` has no bind: nothing moves with it (add a `bind` item)",
                s.name
            ));
        }
    }
    // A part matched by two binds: the later one wins.
    let binds: Vec<(&String, &super::BindDef)> = model
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::Bind { name, def } => Some((name, def)),
            _ => None,
        })
        .collect();
    for p in model
        .parts
        .iter()
        .filter(|p| p.visible && !super::instance_bound(&model.ops, &p.name, p))
    {
        let hits: Vec<&str> = binds
            .iter()
            .filter(|(_, d)| d.parts.iter().any(|q| crate::name_matches(q, &p.name)))
            .map(|(n, _)| n.as_str())
            .collect();
        if hits.len() > 1 {
            out.push(format!(
                "`{}` is bound by {} (the last wins): narrow the patterns",
                p.name,
                hits.join(", ")
            ));
        }
    }
    out
}

/// `verify weights`: smooth-bound parts have every vertex weighted, sums of
/// 1 and at most four bones.
pub fn weights(model: &Model) -> Result<String, String> {
    let mut bad = Vec::new();
    let (mut smooth, mut rigid) = (0, 0);
    for p in &model.parts {
        let Some(b) = &p.rig else { continue };
        if b.bone.is_some() {
            rigid += 1;
            continue;
        }
        smooth += 1;
        if b.weights.len() != p.verts.len() {
            bad.push(format!(
                "`{}`: {} weights for {} verts",
                p.name,
                b.weights.len(),
                p.verts.len()
            ));
            continue;
        }
        let off = b
            .weights
            .iter()
            .filter(|w| {
                (w.iter().sum::<f32>() - 1.0).abs() > 1e-3
                    || w.iter().any(|x| x.is_nan() || *x < 0.0)
            })
            .count();
        if off > 0 {
            bad.push(format!(
                "`{}`: {off} vertex weight(s) don't sum to 1",
                p.name
            ));
        }
    }
    if smooth + rigid == 0 {
        return Err("no part is bound to a skeleton (add a `bind` item)".into());
    }
    if bad.is_empty() {
        Ok(format!(
            "{smooth} smooth part(s) fully weighted (<= 4 bones, sums 1), {rigid} rigid"
        ))
    } else {
        Err(bad.join("; "))
    }
}

/// `verify animation`: checks sampled across a clip.
#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnimationSpec {
    /// The clip (default: the only one).
    #[serde(default)]
    pub clip: Option<String>,
    /// Sample times spread over the clip (default 16).
    #[serde(default)]
    pub samples: Option<u32>,
    /// At every sample: no part passes through another (as `collisions`).
    #[serde(default)]
    pub collisions: Option<crate::commands::CollisionSpec>,
    /// At every sample: nothing sinks below y = 0 by more than this (m).
    #[serde(default, deserialize_with = "num_or_flag_0")]
    #[schemars(with = "Option<f32>")]
    pub floor: Option<f32>,
    /// The pose at the end equals the pose at the start (default on for a
    /// looping clip).
    #[serde(default, rename = "loop")]
    pub looping: Option<bool>,
    /// The largest allowed edge stretch under skinning, as a ratio of the
    /// rest length (default 2: catches collapsing, candy-wrapper joints).
    #[serde(default, deserialize_with = "num_or_flag_2")]
    #[schemars(with = "Option<f32>")]
    pub stretch: Option<f32>,
    /// Skinning keeps each smooth-bound closed part's volume: `true` or a
    /// tolerance (default 0.08 = 8%) on posed / rest volume. Catches a
    /// joint that collapses or balloons.
    #[serde(default, deserialize_with = "num_or_flag_volume")]
    #[schemars(with = "Option<f32>")]
    pub volume: Option<f32>,
    /// Planted feet don't slide: `true` (the toe / foot / paw bones) or
    /// `{bones, contact_height 0.03, tol 0.01, speed [vx, vz]}`.
    #[serde(default)]
    pub foot_slide: Option<FootSlideSpec>,
    /// The centre of mass stays over the support polygon: `true` or
    /// `{margin 0, min_share 1, contact_height 0.02}`.
    #[serde(default)]
    pub balance: Option<BalanceSpec>,
    /// No pops in the motion: `true` or `{max_accel, max_jerk}`.
    #[serde(default)]
    pub smooth: Option<SmoothSpec>,
    /// A biped walk measured against normative human gait (Perry &
    /// Burnfield): `true` or `{side, tolerance, speed, bones}`.
    #[serde(default)]
    pub gait: Option<GaitSpec>,
}

/// `gait`: `true`, or options. `false` turns it off.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum GaitSpec {
    Flag(bool),
    With(Box<GaitOpts>),
}

#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GaitOpts {
    /// `both` (default), `left` or `right`: which legs' joint curves count.
    #[serde(default)]
    pub side: Option<String>,
    /// Band half-width (degrees) for the thigh, knee and ankle curves,
    /// replacing the defaults (thigh 7, knee 10, ankle 7: about 1 SD).
    #[serde(default)]
    pub tolerance: Option<f32>,
    /// Ground velocity `[vx, vz]` (m/s). Default: `foot_slide`'s speed, else
    /// the root motion, else the clip's own (a `mocap` clip's removed
    /// travel), else measured from the planted feet.
    #[serde(default)]
    pub speed: Option<[f32; 2]>,
    /// Bone names; `{side}` becomes `l` / `r`.
    #[serde(default)]
    pub bones: Option<GaitBones>,
}

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct GaitBones {
    /// Hip joint -> knee (`thigh_{side}`).
    pub thigh: String,
    /// Knee -> ankle (`knee_{side}`).
    pub knee: String,
    /// Ankle -> ball of the foot (`ankle_{side}`).
    pub ankle: String,
    /// Its tail is the toe tip (`toe_{side}`, else `ball_{side}`).
    pub toe: String,
    /// Its head is the wrist joint (`wrist_{side}`).
    pub wrist: String,
    /// The pelvis root (`hip`): its head's height is the body's bob.
    pub hip: String,
    /// Head -> tail is the thorax line for trunk lean (`chest`).
    pub chest: String,
}

impl Default for GaitBones {
    fn default() -> Self {
        GaitBones {
            thigh: "thigh_{side}".into(),
            knee: "knee_{side}".into(),
            ankle: "ankle_{side}".into(),
            toe: "toe_{side}".into(),
            wrist: "wrist_{side}".into(),
            hip: "hip".into(),
            chest: "chest".into(),
        }
    }
}

/// `foot_slide`: `true`, or options. `false` turns it off.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum FootSlideSpec {
    Flag(bool),
    With(FootSlideOpts),
}

#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FootSlideOpts {
    /// Bone names or `*` globs whose tips are the feet (default: every
    /// `toe` bone, else every foot / paw / hoof / ball bone, else the leaf
    /// bones whose rest tail is lowest).
    #[serde(default)]
    pub bones: Option<Vec<String>>,
    /// A tip within this height of the ground counts as planted (m, 0.03).
    #[serde(default)]
    pub contact_height: Option<f32>,
    /// The most a planted tip may drift horizontally from where it
    /// planted, after removing `speed` (m, 0.01).
    #[serde(default)]
    pub tol: Option<f32>,
    /// The character's intended ground velocity `[vx, vz]` (m/s) for a
    /// walk in place: planted feet then move at `-speed` relative to the
    /// root, and the slide is measured after adding `speed * dt`. Default:
    /// a `mocap` clip's removed travel (when the clip has no root motion).
    #[serde(default)]
    pub speed: Option<[f32; 2]>,
}

/// `balance`: `true`, or options. `false` turns it off.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum BalanceSpec {
    Flag(bool),
    With(BalanceOpts),
}

#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BalanceOpts {
    /// How far outside the support polygon the centre of mass may sit (m, 0).
    #[serde(default)]
    pub margin: Option<f32>,
    /// The share of grounded samples that must balance (default 1: a walk
    /// is not statically balanced mid-stride, so give e.g. 0.5).
    #[serde(default)]
    pub min_share: Option<f32>,
    /// Vertices within this height of y = 0 form the support (m, 0.02).
    #[serde(default)]
    pub contact_height: Option<f32>,
}

/// `smooth`: `true`, or options. `false` turns it off.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum SmoothSpec {
    Flag(bool),
    With(SmoothOpts),
}

#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmoothOpts {
    /// Absolute acceleration limit for a bone tip (m/s^2). Default: a
    /// sample over 8x the bone's median non-zero acceleration and over 5.
    #[serde(default)]
    pub max_accel: Option<f32>,
    /// Absolute jerk limit for a bone tip (m/s^3); no default limit.
    #[serde(default)]
    pub max_jerk: Option<f32>,
}

/// A number, or `true` for the default (`false` turns the check off).
fn num_or_flag<'de, D: serde::Deserializer<'de>>(d: D, on: f32) -> Result<Option<f32>, D::Error> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum V {
        B(bool),
        N(f32),
    }
    Ok(match <Option<V> as serde::Deserialize>::deserialize(d)? {
        None | Some(V::B(false)) => None,
        Some(V::B(true)) => Some(on),
        Some(V::N(x)) => Some(x),
    })
}

fn num_or_flag_0<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    num_or_flag(d, 0.0)
}

fn num_or_flag_volume<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    num_or_flag(d, 0.08)
}

fn num_or_flag_2<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    num_or_flag(d, 2.0)
}

fn times(c: &Clip, n: u32) -> Vec<f32> {
    let n = n.max(2);
    (0..n)
        .map(|i| {
            if c.looping {
                c.duration * i as f32 / n as f32
            } else {
                c.duration * i as f32 / (n - 1) as f32
            }
        })
        .collect()
}

pub fn posed_model(model: &Model, c: &Clip, s: &Skeleton, t: f32) -> Model {
    let mut m = Model::named(&model.name);
    m.ops = model.ops.clone();
    m.parts = super::posed_parts(model, c, s, t);
    m
}

/// Runs the animation checks; each result is (name, passed, detail).
pub fn animation(
    model: &Model,
    spec: &AnimationSpec,
    scope: &[String],
) -> Vec<(String, bool, String)> {
    match super::clip(model, spec.clip.as_deref()) {
        Ok((c, s)) => animation_of(model, &c, &s, spec, scope),
        Err(e) => vec![("animation".to_string(), false, e.to_string())],
    }
}

/// `animation` on an already resolved clip and skeleton.
fn animation_of(
    model: &Model,
    c: &Clip,
    s: &Skeleton,
    spec: &AnimationSpec,
    scope: &[String],
) -> Vec<(String, bool, String)> {
    let mut out = Vec::new();
    let (c, s) = (c.clone(), s.clone());
    let ts = times(&c, spec.samples.unwrap_or(16));
    let in_scope = |n: &str| scope.is_empty() || scope.iter().any(|p| crate::name_matches(p, n));
    let label = |k: &str| format!("animation `{}` {k}", c.name);
    if spec.looping.unwrap_or(c.looping) {
        // Root motion is the entity's travel: the loop is judged in place.
        let a = in_place(&c, &s, 0.0);
        let b = in_place(&c, &s, c.duration - SEAM_EPS);
        let worst = a
            .iter()
            .zip(&b)
            .zip(&s.bones)
            .map(|(((h0, t0), (h1, t1)), bone)| {
                ((*h0 - *h1).length().max((*t0 - *t1).length()), &bone.name)
            })
            .max_by(|x, y| x.0.total_cmp(&y.0));
        let scale = s
            .bones
            .iter()
            .map(|b| (b.tail - b.head).length())
            .fold(0.0f32, f32::max)
            .max(1e-3);
        // Tip velocity just inside each side of the seam (one-sided
        // differences over `h`): equal positions with a reversed or
        // stalled motion still pop.
        let h = (c.duration / 20.0).clamp(2.0 * SEAM_EPS, 1.0 / 120.0);
        let at = |t: f32| in_place(&c, &s, t);
        let (s0, s1) = (at(h), at(c.duration - SEAM_EPS - h));
        let vel_floor = SEAM_VEL_FLOOR * scale;
        let mut vworst: Option<(f32, f32, f32, f32, &str)> = None; // excess, diff, |v| start, |v| end
        for (i, bone) in s.bones.iter().enumerate() {
            let v0 = (s0[i].1 - a[i].1) / h;
            let v1 = (b[i].1 - s1[i].1) / h;
            let d = (v0 - v1).length();
            let m = v0.length().max(v1.length());
            let allowed = (SEAM_VEL_REL * m).max(vel_floor);
            let over = d - allowed;
            if over > 0.0 && vworst.is_none_or(|w| over > w.0) {
                vworst = Some((over, d, v0.length(), v1.length(), &bone.name));
            }
        }
        match (worst, vworst) {
            (Some((d, bone)), _) if d > 0.02 * scale => out.push((
                label("loop"),
                false,
                format!("the end pose doesn't match the start: `{bone}` is {d:.3} m off (make the last key equal the first)"),
            )),
            (_, Some((_, d, v0, v1, bone))) => out.push((
                label("loop"),
                false,
                format!("the motion jumps at the seam: `{bone}` tip moves at {v0:.2} m/s leaving t = 0 and {v1:.2} m/s arriving at t = {:.3} (velocity differs by {d:.2} m/s; make the first and last keys' tangents continue each other)", c.duration),
            )),
            _ => out.push((
                label("loop"),
                true,
                "the end pose and tip velocities match the start".into(),
            )),
        }
    }
    let max_stretch = spec.stretch.unwrap_or(2.0);
    let mut worst_stretch: (f32, String, f32, usize) = (1.0, String::new(), 0.0, 0);
    let mut floor_fail: Option<(f32, String, f32)> = None;
    let vol_tol = spec.volume;
    // (|ratio - 1|, ratio, part, time, nearest bone)
    let mut worst_vol: Option<(f32, f32, String, f32, String)> = None;
    let mut coll_fail: Option<String> = None;
    let coll = match &spec.collisions {
        Some(crate::commands::CollisionSpec::On(true)) => Some((Vec::new(), 0.2)),
        Some(crate::commands::CollisionSpec::With { allow, max }) => {
            Some((allow.clone(), max.unwrap_or(0.2)))
        }
        _ => None,
    };
    for &t in &ts {
        let pm = posed_model(model, &c, &s, t);
        for (rest, posed) in model.parts.iter().zip(&pm.parts) {
            if !rest.visible
                || rest.rig.as_ref().is_none_or(|b| b.bone.is_some())
                || !in_scope(&rest.name)
            {
                continue;
            }
            for tri in rest.indices.as_chunks::<3>().0 {
                for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                    let (a, b) = (a as usize, b as usize);
                    let l0 = (rest.world_vert(a) - rest.world_vert(b)).length();
                    if l0 < 1e-6 {
                        continue;
                    }
                    let l1 = (Vec3::from(posed.verts[a]) - Vec3::from(posed.verts[b])).length();
                    // Stretching counts fully; the inside of a bending
                    // joint compresses by nature, so only a collapse to
                    // under a fifth (a pinch) counts, scaled to compare.
                    let r = (l1 / l0).max(l0 / l1.max(1e-9) * max_stretch / 5.0);
                    if r > worst_stretch.0 {
                        worst_stretch = (r, rest.name.clone(), t, a);
                    }
                }
            }
        }
        if vol_tol.is_some() {
            for (name, ratio, bone) in super::skin::volume_ratios(&model.parts, &pm.parts, &s) {
                let visible = model.parts.iter().any(|p| p.name == name && p.visible);
                let dev = (ratio - 1.0).abs();
                if visible
                    && in_scope(&name)
                    && ratio.is_finite()
                    && worst_vol.as_ref().is_none_or(|w| dev > w.0)
                {
                    worst_vol = Some((dev, ratio, name, t, bone));
                }
            }
        }
        if let Some(tol) = spec.floor {
            for p in pm.parts.iter().filter(|p| p.visible && in_scope(&p.name)) {
                let (lo, _) = p.bounds();
                if lo.y < -tol && floor_fail.as_ref().is_none_or(|f| lo.y < f.0) {
                    floor_fail = Some((lo.y, p.name.clone(), t));
                }
            }
        }
        if let Some((allow, max)) = &coll
            && coll_fail.is_none()
        {
            let report = crate::contact::collisions(&pm, allow, *max, scope);
            if let Some(o) = report.bad.first() {
                coll_fail = Some(format!(
                    "at t = {t:.3}: `{}` passes through `{}` ({:.0}% of the smaller, {:.3} m deep){}",
                    o.a,
                    o.b,
                    o.fraction * 100.0,
                    o.depth,
                    if report.bad.len() > 1 {
                        format!(" and {} more pair(s)", report.bad.len() - 1)
                    } else {
                        String::new()
                    }
                ));
            }
        }
    }
    let n = ts.len();
    if worst_stretch.0 > max_stretch {
        // Where, and which bones share that vertex.
        let part = model.parts.iter().find(|p| p.name == worst_stretch.1);
        let at = part
            .map(|p| p.world_vert(worst_stretch.3))
            .unwrap_or(Vec3::ZERO);
        let bones: Vec<String> = part
            .and_then(|p| p.rig.as_ref())
            .and_then(|b| {
                Some((
                    b.joints.get(worst_stretch.3)?,
                    b.weights.get(worst_stretch.3)?,
                ))
            })
            .map(|(j, w)| {
                (0..4)
                    .filter(|&k| w[k] > 0.01)
                    .filter_map(|k| {
                        s.bones
                            .get(j[k] as usize)
                            .map(|b| format!("{} {:.2}", b.name, w[k]))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push((
            label("stretch"),
            false,
            format!(
                "`{}` stretches an edge {:.2}x at t = {:.3} (max {max_stretch}) near [{:.3}, {:.3}, {:.3}], weighted {}: the bones there pull apart (blend less, `paint` the region onto one bone, or soften the keys)",
                worst_stretch.1,
                worst_stretch.0,
                worst_stretch.2,
                at.x,
                at.y,
                at.z,
                if bones.is_empty() { "?".into() } else { bones.join(" / ") }
            ),
        ));
    } else {
        out.push((
            label("stretch"),
            true,
            format!(
                "worst edge stretch {:.2}x over {n} samples",
                worst_stretch.0
            ),
        ));
    }
    if let Some(tol) = vol_tol {
        match worst_vol {
            Some((dev, ratio, part, t, bone)) if dev > tol => out.push((
                label("volume"),
                false,
                format!(
                    "`{part}` is {:.0}% of its rest volume at t = {t:.3} (tolerance {:.0}%), the most moved vertex weighted to `{bone}`: the skin collapses or balloons there (blend more, bind with more bones, or add a corrective)",
                    ratio * 100.0,
                    tol * 100.0
                ),
            )),
            Some((_, ratio, part, t, _)) => out.push((
                label("volume"),
                true,
                format!(
                    "worst volume `{part}` {:.1}% of rest at t = {t:.3} over {n} samples (tolerance {:.0}%)",
                    ratio * 100.0,
                    tol * 100.0
                ),
            )),
            None => out.push((
                label("volume"),
                true,
                "no smooth-bound closed part to measure".into(),
            )),
        }
    }
    if let Some(tol) = spec.floor {
        match floor_fail {
            Some((y, p, t)) => out.push((
                label("floor"),
                false,
                format!("`{p}` sinks to y = {y:.3} at t = {t:.3} (tolerance {tol})"),
            )),
            None => out.push((
                label("floor"),
                true,
                format!("nothing below y = -{tol} over {n} samples"),
            )),
        }
    }
    if coll.is_some() {
        match coll_fail {
            Some(f) => out.push((label("collisions"), false, f)),
            None => out.push((
                label("collisions"),
                true,
                format!("no part passes through another over {n} samples"),
            )),
        }
    }
    let foot = match &spec.foot_slide {
        Some(FootSlideSpec::Flag(true)) => Some(FootSlideOpts::default()),
        Some(FootSlideSpec::With(o)) => Some(o.clone()),
        _ => None,
    };
    let smooth = match &spec.smooth {
        Some(SmoothSpec::Flag(true)) => Some(SmoothOpts::default()),
        Some(SmoothSpec::With(o)) => Some(o.clone()),
        _ => None,
    };
    if foot.is_some() || smooth.is_some() {
        let f = Fine::new(&c, spec.samples);
        let tips = f.tips(&c, &s);
        if let Some(o) = &foot {
            let trav: Vec<Vec3> = f.ts.iter().map(|&t| c.travel(&s, t).0).collect();
            let delta = c.root_delta(&s).0;
            // No stated speed: a mocap clip's removed travel, unless the
            // clip carries its own root motion (that is the travel).
            let mut o = o.clone();
            if o.speed.is_none() && delta.x.hypot(delta.z) <= 1e-4 {
                o.speed = c.speed;
            }
            out.push(foot_slide(
                &label("foot_slide"),
                &s,
                &f,
                &tips,
                (&trav, delta),
                &o,
            ));
        }
        if let Some(o) = &smooth {
            out.push(smooth_check(&label("smooth"), &s, &f, &tips, o));
        }
    }
    let balance = match &spec.balance {
        Some(BalanceSpec::Flag(true)) => Some(BalanceOpts::default()),
        Some(BalanceSpec::With(o)) => Some(o.clone()),
        _ => None,
    };
    if let Some(o) = &balance {
        out.push(balance_check(
            &label("balance"),
            model,
            &c,
            &s,
            &ts,
            o,
            scope,
        ));
    }
    let gait = match &spec.gait {
        Some(GaitSpec::Flag(true)) => Some(GaitOpts::default()),
        Some(GaitSpec::With(o)) => Some((**o).clone()),
        _ => None,
    };
    if let Some(mut o) = gait {
        if o.speed.is_none()
            && let Some(FootSlideSpec::With(f)) = &spec.foot_slide
        {
            o.speed = f.speed;
        }
        out.extend(gait_check(&label("gait"), &c, &s, &o));
    }
    out
}

// ---------------------------------------------------------------------------
// `gait`: a biped walk against normative human gait kinematics.
//
// Source: J. Perry & J. M. Burnfield, "Gait Analysis: Normal and
// Pathological Function", 2nd ed. (SLACK, 2010), chapters 4-7 (hip, knee,
// ankle sagittal motion and the phases of the cycle), cross-checked against
// D. A. Winter, "Biomechanics and Motor Control of Human Gait" (1991).
// Sagittal plane, degrees, by percent of the gait cycle (GC) from heel
// strike of that leg. Thigh = hip-knee line from vertical (+ forward);
// knee = flexion; ankle = dorsiflexion (+) / plantarflexion (-) from
// standing. Bands are about 1 SD of adult comfortable-speed walking.
// ---------------------------------------------------------------------------

/// Thigh from vertical (+ forward). Initial contact +20, vertical at ~27%,
/// -20 at 50%, -10 at toe-off (62%), peak +25 in mid swing (85%).
const NORM_THIGH: &[(f32, f32)] = &[
    (0.0, 20.0),
    (6.0, 20.0),
    (12.0, 18.0),
    (20.0, 9.0),
    (27.0, 1.0),
    (31.0, -3.0),
    (40.0, -11.0),
    (50.0, -19.0),
    (55.0, -18.0),
    (62.0, -10.0),
    (68.0, 2.0),
    (75.0, 15.0),
    (85.0, 25.0),
    (92.0, 23.0),
    (100.0, 20.0),
];
/// Knee flexion. 0..5 at contact, 15..20 at 12% (loading response), ~5 in
/// mid / terminal stance, 40 at toe-off, peak 60..65 at ~72%, near 0 at
/// the end of swing.
const NORM_KNEE: &[(f32, f32)] = &[
    (0.0, 3.0),
    (6.0, 12.0),
    (12.0, 18.0),
    (20.0, 12.0),
    (31.0, 6.0),
    (40.0, 3.0),
    (45.0, 5.0),
    (50.0, 10.0),
    (56.0, 25.0),
    (62.0, 40.0),
    (68.0, 55.0),
    (72.0, 62.0),
    (76.0, 59.0),
    (81.0, 45.0),
    (87.0, 25.0),
    (93.0, 10.0),
    (100.0, 3.0),
];
/// Ankle. 0 at contact, -5 at foot flat (6%), +5 in mid stance, +10 at 47%
/// (heel rise), -15..-20 at toe-off (62%), back to 0 through swing.
const NORM_ANKLE: &[(f32, f32)] = &[
    (0.0, 0.0),
    (6.0, -5.0),
    (12.0, -2.0),
    (20.0, 2.0),
    (31.0, 5.0),
    (40.0, 8.0),
    (47.0, 10.0),
    (52.0, 6.0),
    (57.0, -5.0),
    (62.0, -17.0),
    (68.0, -10.0),
    (75.0, -4.0),
    (87.0, 0.0),
    (100.0, 0.0),
];
/// How far (percent of the cycle) a measured curve may lead or lag the
/// norm before a deviation counts: timing jitter of a few percent is normal.
const GAIT_SLACK: f32 = 3.0;
/// Stature from the hip joint height (Winter's segment table: greater
/// trochanter at 0.530 of stature).
const HIP_OF_STATURE: f32 = 0.53;
/// The heel point counts as on the ground within this height (m, scaled):
/// a terminal-swing heel skims a centimetre or two above the floor.
const HEEL_CONTACT: f32 = 0.005;
/// The toe tip counts as on the ground within this of its rest height (m,
/// scaled): under the normal 1-2 cm mid-swing toe clearance.
const TOE_CONTACT: f32 = 0.004;
/// The reference adult the time and length bands are stated for (m).
const REF_STATURE: f32 = 1.75;

fn norm_at(table: &[(f32, f32)], p: f32) -> f32 {
    let p = p.rem_euclid(100.0);
    for w in table.windows(2) {
        let ((p0, v0), (p1, v1)) = (w[0], w[1]);
        if p <= p1 {
            let u = if p1 > p0 { (p - p0) / (p1 - p0) } else { 0.0 };
            return v0 + (v1 - v0) * u;
        }
    }
    table.last().map(|k| k.1).unwrap_or(0.0)
}

fn gait_phase(p: f32) -> &'static str {
    match p {
        p if p < 12.0 => "loading response",
        p if p < 31.0 => "mid stance",
        p if p < 50.0 => "terminal stance",
        p if p < 62.0 => "pre-swing",
        p if p < 75.0 => "initial swing",
        p if p < 87.0 => "mid swing",
        _ => "terminal swing",
    }
}

/// One leg's bones, resolved.
struct GaitLeg {
    side: &'static str,
    thigh: usize,
    knee: usize,
    ankle: usize,
    toe: usize,
    wrist: Option<usize>,
}

/// Per sample, in the sagittal frame: forward `f`, up `y`.
struct LegTrack {
    thigh: Vec<f32>,
    knee: Vec<f32>,
    ankle: Vec<f32>,
    /// Height of the heel point (m): below the ankle at rest, carried by
    /// the foot's pitch.
    heel_y: Vec<f32>,
    /// Toe tip height above its rest height (m).
    toe_y: Vec<f32>,
    /// Ankle joint along `f` and across (m).
    ankle_f: Vec<f32>,
    ankle_side: Vec<f32>,
    wrist_f: Vec<f32>,
    contact: Vec<bool>,
}

fn deg_atan2(a: f32, b: f32) -> f32 {
    a.atan2(b).to_degrees()
}

/// Sagittal angles of a leg from its segments: thigh from vertical, shank
/// from vertical (both + forward, measured downward), foot pitch (+ toes
/// up).
fn leg_angles(seg: &[(Vec3, Vec3)], l: &GaitLeg, f: Vec3) -> (f32, f32, f32) {
    let up = Vec3::Y;
    let d = seg[l.thigh].1 - seg[l.thigh].0;
    let w = seg[l.knee].1 - seg[l.knee].0;
    let u = seg[l.ankle].1 - seg[l.ankle].0;
    (
        deg_atan2(d.dot(f), -d.dot(up)),
        deg_atan2(w.dot(f), -w.dot(up)),
        deg_atan2(u.dot(up), u.dot(f)),
    )
}

/// The heel offset from the ankle joint at rest, in (forward, up): on the
/// ground, behind the ankle by 35% of the ankle -> ball run.
fn heel_offset(rest: &[(Vec3, Vec3)], l: &GaitLeg, f: Vec3) -> (f32, f32) {
    let (a, b) = rest[l.ankle];
    (-0.35 * (b - a).dot(f).abs().max(0.02), -a.y)
}

/// `(heel y, toe tip y)` of a posed leg, the heel carried by the foot's
/// pitch change from rest.
pub(crate) fn gait_foot_heights(
    seg: &[(Vec3, Vec3)],
    rest: &[(Vec3, Vec3)],
    thigh: usize,
    knee: usize,
    ankle: usize,
    toe: usize,
    f: Vec3,
) -> (f32, f32) {
    let l = GaitLeg {
        side: "",
        thigh,
        knee,
        ankle,
        toe,
        wrist: None,
    };
    let (_, _, p) = leg_angles(seg, &l, f);
    let (_, _, p0) = leg_angles(rest, &l, f);
    let (hf, hy) = heel_offset(rest, &l, f);
    let q = (p - p0).to_radians();
    let heel = seg[ankle].0.y + hf * q.sin() + hy * q.cos();
    (heel, seg[toe].1.y)
}

fn gait_check(label: &str, c: &Clip, s: &Skeleton, o: &GaitOpts) -> Vec<(String, bool, String)> {
    let mut out = Vec::new();
    let names = o.bones.clone().unwrap_or_default();
    let find = |n: &str| s.bones.iter().position(|b| b.name == n);
    let mut legs = Vec::new();
    let mut missing = Vec::new();
    for side in ["l", "r"] {
        let get = |pat: &str| {
            let n = pat.replace("{side}", side);
            (find(&n), n)
        };
        let mut need = |pat: &str| {
            let (i, n) = get(pat);
            if i.is_none() {
                missing.push(n);
            }
            i
        };
        let (thigh, knee, ankle) = (need(&names.thigh), need(&names.knee), need(&names.ankle));
        let toe = get(&names.toe)
            .0
            .or_else(|| find(&format!("ball_{side}")))
            .or(ankle);
        let wrist = get(&names.wrist).0;
        if let (Some(thigh), Some(knee), Some(ankle), Some(toe)) = (thigh, knee, ankle, toe) {
            legs.push(GaitLeg {
                side: if side == "l" { "left" } else { "right" },
                thigh,
                knee,
                ankle,
                toe,
                wrist,
            });
        }
    }
    let hip = find(&names.hip);
    let chest = find(&names.chest);
    if legs.len() < 2 || hip.is_none() {
        out.push((
            label.to_string(),
            true,
            format!(
                "skipped: not a biped rig (missing {}); name the bones with `bones`",
                if missing.is_empty() {
                    names.hip.clone()
                } else {
                    missing.join(", ")
                }
            ),
        ));
        return out;
    }
    let hip = hip.unwrap_or(0);
    let rest: Vec<(Vec3, Vec3)> = s.bones.iter().map(|b| (b.head, b.tail)).collect();
    let hip_h = 0.5 * (rest[legs[0].thigh].0.y + rest[legs[1].thigh].0.y);
    let stature = hip_h / HIP_OF_STATURE;
    let k = stature / REF_STATURE;

    // Dense samples.
    let n = ((c.duration * 120.0).ceil() as usize).max(120);
    let dt = c.duration / n as f32;
    let ts: Vec<f32> = (0..n).map(|i| i as f32 * dt).collect();
    let segs: Vec<Vec<(Vec3, Vec3)>> = ts.iter().map(|&t| in_place(c, s, t)).collect();

    // Speed: stated, root motion, else measured from the planted ankles.
    let rd = c.root_delta(s).0;
    let measured = {
        let mut v: Vec<Vec3> = Vec::new();
        for l in &legs {
            for i in 0..n {
                let j = (i + 1) % n;
                let (a, b) = (segs[i][l.ankle].0, segs[j][l.ankle].0);
                if a.y.max(b.y) <= rest[l.ankle].0.y + 0.012 * k {
                    let mut d = (b - a) / dt;
                    d.y = 0.0;
                    v.push(-d);
                }
            }
        }
        let mut xs: Vec<f32> = v.iter().map(|d| d.x).collect();
        let mut zs: Vec<f32> = v.iter().map(|d| d.z).collect();
        Vec3::new(median(&mut xs), 0.0, median(&mut zs))
    };
    // The way the feet point at rest is forward unless a velocity says
    // otherwise (a measured one can't be trusted for its direction: a
    // shuffling foot may drift either way).
    let facing = {
        let mut d = Vec3::ZERO;
        for l in &legs {
            d += rest[l.ankle].1 - rest[l.ankle].0;
        }
        d.y = 0.0;
        d.try_normalize().unwrap_or(Vec3::Z)
    };
    let (vel, vsrc) = if let Some([x, z]) = o.speed {
        (Vec3::new(x, 0.0, z), "stated")
    } else if rd.x.hypot(rd.z) > 1e-4 {
        (Vec3::new(rd.x, 0.0, rd.z) / c.duration, "root motion")
    } else if let Some([x, z]) = c.speed {
        (Vec3::new(x, 0.0, z), "the clip's speed")
    } else {
        (
            facing * measured.dot(facing).max(0.0),
            "measured from the planted feet",
        )
    };
    let speed = vel.length();
    let f = if speed > 1e-3 { vel / speed } else { facing };
    let lat = f.cross(Vec3::Y).normalize_or_zero();

    // Tracks.
    let tracks: Vec<LegTrack> = legs
        .iter()
        .map(|l| {
            let (_, sh0, p0) = leg_angles(&rest, l, f);
            let toe0 = rest[l.toe].1.y;
            let mut t = LegTrack {
                thigh: Vec::new(),
                knee: Vec::new(),
                ankle: Vec::new(),
                heel_y: Vec::new(),
                toe_y: Vec::new(),
                ankle_f: Vec::new(),
                ankle_side: Vec::new(),
                wrist_f: Vec::new(),
                contact: Vec::new(),
            };
            for seg in &segs {
                let (th, sh, p) = leg_angles(seg, l, f);
                t.thigh.push(th);
                t.knee.push(th - sh);
                t.ankle.push((p - sh) - (p0 - sh0));
                let (heel, toe) = gait_foot_heights(seg, &rest, l.thigh, l.knee, l.ankle, l.toe, f);
                t.heel_y.push(heel);
                t.toe_y.push(toe - toe0);
                let a = seg[l.ankle].0;
                t.ankle_f.push((a - seg[hip].0).dot(f));
                t.ankle_side.push(a.dot(lat));
                let hc = 0.5 * (seg[legs[0].thigh].0 + seg[legs[1].thigh].0);
                t.wrist_f
                    .push(l.wrist.map(|w| (seg[w].0 - hc).dot(f)).unwrap_or(0.0));
                t.contact
                    .push(heel <= HEEL_CONTACT * k || toe - toe0 <= TOE_CONTACT * k);
            }
            t
        })
        .collect();

    // Stance runs (cyclic) that include a heel contact; heel strike = start.
    let runs = |t: &LegTrack| -> Vec<(usize, usize)> {
        let Some(start) = (0..n).find(|&i| !t.contact[i]) else {
            return Vec::new();
        };
        let mut v = Vec::new();
        let mut cur: Option<(usize, usize, bool)> = None;
        for kk in 1..=n {
            let i = (start + kk) % n;
            if t.contact[i] {
                let heel = t.heel_y[i] <= HEEL_CONTACT * k;
                cur = Some(match cur {
                    Some((a, len, h)) => (a, len + 1, h || heel),
                    None => (i, 1, heel),
                });
            } else if let Some((a, len, h)) = cur.take()
                && h
                && len as f32 >= 0.08 * n as f32 / 2.0
            {
                v.push((a, len));
            }
        }
        // A brief touch (a toe scuffing in swing) is not a stance.
        let longest = v.iter().map(|r| r.1).max().unwrap_or(0);
        v.retain(|r| 2 * r.1 >= longest);
        v.sort();
        v
    };
    let stance: Vec<Vec<(usize, usize)>> = tracks.iter().map(runs).collect();
    if stance.iter().any(|r| r.is_empty()) {
        out.push((
            format!("{label} cycle"),
            false,
            format!(
                "no heel strike found: a foot never plants (heel within 5 mm of the ground, or toe tip within 4 mm of its rest height) for {}; is the clip walking?",
                legs.iter()
                    .zip(&stance)
                    .filter(|(_, r)| r.is_empty())
                    .map(|(l, _)| l.side)
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        ));
        return out;
    }
    let cycles = stance[0].len().max(1);
    let period = c.duration / cycles as f32;
    // Percent of the cycle at each sample, per leg.
    let pct: Vec<Vec<f32>> = stance
        .iter()
        .map(|r| {
            (0..n)
                .map(|i| {
                    let last = r
                        .iter()
                        .rev()
                        .find(|(a, _)| *a <= i)
                        .or(r.last())
                        .map(|(a, _)| *a)
                        .unwrap_or(0);
                    let d = (i + n - last) % n;
                    100.0 * (d as f32 * dt / period).fract()
                })
                .collect()
        })
        .collect();
    let sides: Vec<usize> = match o.side.as_deref() {
        Some("left") | Some("l") => vec![0],
        Some("right") | Some("r") => vec![1],
        _ => vec![0, 1],
    };

    // Joint curves.
    // A sample's deviation from a norm curve, allowing GAIT_SLACK percent of
    // timing lead or lag: (deviation, the nearest normal value).
    let deviation = |table: &[(f32, f32)], v: f32, p: f32| {
        [
            -1.0f32,
            -2.0 / 3.0,
            -1.0 / 3.0,
            0.0,
            1.0 / 3.0,
            2.0 / 3.0,
            1.0,
        ]
        .iter()
        .map(|d| norm_at(table, p + d * GAIT_SLACK))
        .map(|nv| ((v - nv).abs(), nv))
        .fold((f32::MAX, 0.0), |a, b| if b.0 < a.0 { b } else { a })
    };
    let curve = |name: &str,
                 table: &[(f32, f32)],
                 tol: f32,
                 get: &dyn Fn(&LegTrack) -> &Vec<f32>| {
        // (deviation, value, normal, pct, side), overall / in stance / in swing.
        type W<'a> = Option<(f32, f32, f32, f32, &'a str)>;
        let mut worst: [W; 3] = [None, None, None];
        let mut rms = 0.0f32;
        let mut cnt = 0usize;
        for &li in &sides {
            let v = get(&tracks[li]);
            for i in 0..n {
                let p = pct[li][i];
                let (dev, nb) = deviation(table, v[i], p);
                rms += (v[i] - norm_at(table, p)).powi(2);
                cnt += 1;
                let w = Some((dev, v[i], nb, p, legs[li].side));
                for k in [0, if p < 62.0 { 1 } else { 2 }] {
                    if worst[k].is_none_or(|x| dev > x.0) {
                        worst[k] = w;
                    }
                }
            }
        }
        let rms = (rms / cnt.max(1) as f32).sqrt();
        let range = sides
            .iter()
            .map(|&li| {
                let v = get(&tracks[li]);
                let (lo, hi) = v
                    .iter()
                    .fold((f32::MAX, f32::MIN), |a, &x| (a.0.min(x), a.1.max(x)));
                let imax = (0..n).max_by(|&a, &b| v[a].total_cmp(&v[b])).unwrap_or(0);
                let imin = (0..n).min_by(|&a, &b| v[a].total_cmp(&v[b])).unwrap_or(0);
                format!(
                    "{} {lo:.0}..{hi:.0} (min at {:.0}%, max at {:.0}%)",
                    legs[li].side, pct[li][imin], pct[li][imax]
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let (nlo, nhi) = table
            .iter()
            .fold((f32::MAX, f32::MIN), |a, k| (a.0.min(k.1), a.1.max(k.1)));
        let Some((dev, val, nv, p, side)) = worst[0] else {
            return (format!("{label} {name}"), true, "no samples".to_string());
        };
        let pass = dev <= tol;
        let what = |val: f32, nv: f32| match (name, val > nv) {
            ("knee", true) => "the knee is too bent",
            ("knee", false) => "the knee is too straight",
            ("thigh", true) => "the thigh is too far forward",
            ("thigh", false) => "the thigh is too far back",
            ("ankle", true) => {
                "too much dorsiflexion: toes too high, or the shank too far over the foot"
            }
            (_, _) => "too much plantarflexion: toes pointed down",
        };
        let part = |w: W, which: &str| match w {
            Some((d, v, nv, p, sd)) if d > tol => format!(
                "; in {which}: {v:.0} deg at {p:.0}% GC ({}, {sd}), normal {nv:.0} ({})",
                gait_phase(p),
                what(v, nv)
            ),
            _ => String::new(),
        };
        (
            format!("{label} {name}"),
            pass,
            if pass {
                format!(
                    "worst {name} {val:.0} deg at {p:.0}% GC ({}, {side}), normal {nv:.0} +- {tol:.0}; rms {rms:.1} deg; measured {range}; normal {nlo:.0}..{nhi:.0}",
                    gait_phase(p)
                )
            } else {
                format!(
                    "{name}: {val:.0} deg at {p:.0}% GC ({}, {side}), normal {nv:.0} +- {tol:.0}: {}{}{}; rms {rms:.1} deg; measured {range}; normal {nlo:.0}..{nhi:.0}",
                    gait_phase(p),
                    what(val, nv),
                    part(worst[1], "stance"),
                    part(worst[2], "swing"),
                )
            },
        )
    };
    let tol = |d: f32| o.tolerance.unwrap_or(d);
    out.push(curve("thigh", NORM_THIGH, tol(7.0), &|t| &t.thigh));
    out.push(curve("knee", NORM_KNEE, tol(10.0), &|t| &t.knee));
    out.push(curve("ankle", NORM_ANKLE, tol(7.0), &|t| &t.ankle));

    // Knee hyperextension.
    let kmin = sides
        .iter()
        .flat_map(|&li| tracks[li].knee.iter().copied())
        .fold(f32::MAX, f32::min);
    out.push((
        format!("{label} knee_extension"),
        kmin >= -5.0,
        format!("least knee flexion {kmin:.1} deg (hyperextension past -5 fails)"),
    ));

    // Timing.
    let stance_share: Vec<f32> = stance
        .iter()
        .map(|r| r.iter().map(|x| x.1).sum::<usize>() as f32 / n as f32)
        .collect();
    let double = (0..n)
        .filter(|&i| tracks[0].contact[i] && tracks[1].contact[i])
        .count() as f32
        / n as f32;
    let tk = k.sqrt();
    // The cycle band is for a comfortable speed (~1.35 x sqrt(stature /
    // reference) m/s). Slower or faster walks change cadence too: cadence
    // grows about with the square root of speed (Grieve & Gear 1966,
    // Ergonomics 9(5); Grieve 1968), so outside +-15% of the comfortable
    // speed the band scales by sqrt(edge / speed), continuous at the edge
    // (a 0.95 m/s stroll may take a 1.35 s cycle). Unknown speed: as is.
    let vref = 1.35 * tk;
    let vf = if speed > 0.3 {
        let edge = speed.clamp(0.85 * vref, 1.15 * vref);
        (edge / speed).sqrt()
    } else {
        1.0
    };
    let (plo, phi) = (0.9 * tk * vf, 1.3 * tk * vf);
    let stance_ok = stance_share.iter().all(|&x| (0.55..=0.65).contains(&x));
    let double_ok = (0.08..=0.32).contains(&double);
    let period_ok = (plo..=phi).contains(&period);
    let strikes: Vec<String> = legs
        .iter()
        .zip(&stance)
        .map(|(l, r)| {
            format!(
                "{} {}",
                l.side,
                r.iter()
                    .map(|(a, _)| format!("t={:.3}", ts[*a]))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    out.push((
        format!("{label} timing"),
        stance_ok && double_ok && period_ok,
        format!(
            "cycle {period:.2} s (normal {plo:.2}..{phi:.2} s at stature {stature:.2} m and {speed:.2} m/s; cadence {:.0} steps/min){}; stance {} (normal 55..65%){}; double support {:.0}% (normal ~20%, 8..32){}; heel strikes {}",
            120.0 / period,
            if period_ok { "" } else { " FAIL" },
            legs.iter()
                .zip(&stance_share)
                .map(|(l, x)| format!("{} {:.0}%", l.side, 100.0 * x))
                .collect::<Vec<_>>()
                .join(", "),
            if stance_ok { "" } else { " FAIL" },
            100.0 * double,
            if double_ok { "" } else { " FAIL" },
            strikes.join("; ")
        ),
    ));

    // Stride.
    let stride = speed * period;
    let sr = stride / stature;
    out.push((
        format!("{label} stride"),
        (0.6..=1.0).contains(&sr),
        format!(
            "speed {speed:.2} m/s ({vsrc}; the planted feet say {:.2}), stride {stride:.2} m = {sr:.2} x stature (normal 0.6..1.0; ~0.8 at a comfortable {:.2} m/s)",
            measured.length(),
            1.35 * tk
        ),
    ));

    // Step width: lateral distance between the ankles while flat.
    let mean_side = |t: &LegTrack| {
        let v: Vec<f32> = (0..n)
            .filter(|&i| t.heel_y[i] <= HEEL_CONTACT * k)
            .map(|i| t.ankle_side[i])
            .collect();
        v.iter().sum::<f32>() / v.len().max(1) as f32
    };
    let width = (mean_side(&tracks[0]) - mean_side(&tracks[1])).abs();
    let (wlo, whi) = (0.05 * k, 0.15 * k);
    out.push((
        format!("{label} step_width"),
        (wlo..=whi).contains(&width),
        format!(
            "step width {:.1} cm between the planted ankles (normal 8..10 cm, {:.0}..{:.0} accepted)",
            100.0 * width,
            100.0 * wlo,
            100.0 * whi
        ),
    ));

    // Pelvis bob: two peaks per stride, lowest in double support.
    let hy: Vec<f32> = segs.iter().map(|g| g[hip].0.y).collect();
    let (lo, hi) = hy
        .iter()
        .fold((f32::MAX, f32::MIN), |a, &x| (a.0.min(x), a.1.max(x)));
    let p2p = hi - lo;
    let prom = 0.3 * p2p;
    let w = (n / 16).max(1);
    let peaks = (0..n)
        .filter(|&i| {
            let v = hy[i];
            (1..=w).all(|d| v >= hy[(i + d) % n] && v > hy[(i + n - d) % n])
                && (1..=n / 4).any(|d| v - hy[(i + d) % n] >= prom)
                && (1..=n / 4).any(|d| v - hy[(i + n - d) % n] >= prom)
        })
        .count();
    let (blo, bhi) = (0.02 * k, 0.07 * k);
    let per_cycle = peaks as f32 / cycles as f32;
    let ilo = (0..n).min_by(|&a, &b| hy[a].total_cmp(&hy[b])).unwrap_or(0);
    let ihi = (0..n).max_by(|&a, &b| hy[a].total_cmp(&hy[b])).unwrap_or(0);
    out.push((
        format!("{label} pelvis"),
        (blo..=bhi).contains(&p2p) && (per_cycle - 2.0).abs() < 0.5,
        format!(
            "vertical excursion {:.1} cm (normal 4..5, {:.0}..{:.0} accepted), {peaks} peak(s) over {cycles} cycle(s) (normal 2 per cycle); lowest at {:.0}% GC of the left leg ({}), highest at {:.0}% ({}; normal: lowest in double support, highest in mid stance)",
            100.0 * p2p,
            100.0 * blo,
            100.0 * bhi,
            pct[0][ilo],
            gait_phase(pct[0][ilo]),
            pct[0][ihi],
            gait_phase(pct[0][ihi]),
        ),
    ));

    // Trunk lean.
    if let Some(ch) = chest {
        let lean: Vec<f32> = segs
            .iter()
            .map(|g| {
                let d = g[ch].1 - g[ch].0;
                deg_atan2(d.dot(f), d.dot(Vec3::Y))
            })
            .collect();
        let worst = lean
            .iter()
            .fold(0.0f32, |a, &x| if x.abs() > a.abs() { x } else { a });
        let mean = lean.iter().sum::<f32>() / n as f32;
        out.push((
            format!("{label} trunk"),
            worst.abs() < 10.0,
            format!(
                "thorax lean {mean:.1} deg mean, {worst:.1} worst (+ forward; normal 2..5, under 10)"
            ),
        ));
    }

    // Arms: anti-phase with the same-side leg, beside the thighs.
    let mut arm_ok = true;
    let mut arm_lines = Vec::new();
    for (l, t) in legs.iter().zip(&tracks) {
        if l.wrist.is_none() {
            continue;
        }
        let (a, b) = (&t.wrist_f, &t.ankle_f);
        let ma = a.iter().sum::<f32>() / n as f32;
        let mb = b.iter().sum::<f32>() / n as f32;
        let (mut sab, mut saa, mut sbb) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..n {
            sab += (a[i] - ma) * (b[i] - mb);
            saa += (a[i] - ma).powi(2);
            sbb += (b[i] - mb).powi(2);
        }
        let r = sab / (saa * sbb).sqrt().max(1e-9);
        let (lo, hi) = a
            .iter()
            .fold((f32::MAX, f32::MIN), |q, &x| (q.0.min(x), q.1.max(x)));
        let ex = hi - lo;
        let ok_r = r < -0.5;
        let ok_ex = (0.1 * k..=0.5 * k).contains(&ex);
        let ok_c = (-0.06 * k..=0.12 * k).contains(&ma);
        arm_ok &= ok_r && ok_ex && ok_c;
        arm_lines.push(format!(
            "{}: correlation with the {} foot {r:.2}{} (normal: opposite, below -0.5); wrist swings {:.0} cm{} (normal 10..{:.0}), centred {:.0} cm {} the hip joints{} (normal -{:.0}..{:.0}: beside the thighs)",
            l.side,
            l.side,
            if ok_r { "" } else { " FAIL (in phase with the leg)" },
            100.0 * ex,
            if ok_ex { "" } else { " FAIL" },
            50.0 * k,
            100.0 * ma.abs(),
            if ma >= 0.0 { "ahead of" } else { "behind" },
            if ok_c { "" } else { " FAIL" },
            6.0 * k,
            12.0 * k
        ));
    }
    if !arm_lines.is_empty() {
        out.push((format!("{label} arms"), arm_ok, arm_lines.join("; ")));
    }

    // Toe clearance in mid swing.
    let mut cl = Vec::new();
    let mut cl_ok = true;
    for ((l, t), r) in legs.iter().zip(&tracks).zip(&stance) {
        let Some(&(a, len)) = r.first() else { continue };
        let off = (a + len) % n;
        let next = r.get(1).map(|x| x.0).unwrap_or(a + n);
        let swing = (next + n - off) % n;
        let swing = if swing == 0 { n } else { swing };
        let (s0, s1) = ((swing as f32 * 0.3) as usize, (swing as f32 * 0.8) as usize);
        let mut m = f32::MAX;
        let mut at = 0;
        for d in s0..s1.max(s0 + 1) {
            let i = (off + d) % n;
            if t.toe_y[i] < m {
                m = t.toe_y[i];
                at = i;
            }
        }
        let ok = (0.005 * k..=0.06 * k).contains(&m);
        cl_ok &= ok;
        cl.push(format!(
            "{} {:.1} cm at {:.0}% GC{}",
            l.side,
            100.0 * m,
            pct[legs.iter().position(|x| x.side == l.side).unwrap_or(0)][at],
            if ok {
                ""
            } else if m < 0.005 * k {
                " FAIL (the toe scuffs)"
            } else {
                " FAIL (a high step: lower the swing foot)"
            }
        ));
    }
    out.push((
        format!("{label} toe_clearance"),
        cl_ok,
        format!(
            "least toe-tip lift over its rest height in mid swing: {} (normal 1..2 cm, {:.1}..{:.0} accepted)",
            cl.join(", "),
            0.5 * k,
            6.0 * k
        ),
    ));
    out
}

/// Offset from the clip's end for the "just before the seam" sample (s).
const SEAM_EPS: f32 = 1e-4;
/// Seam velocity tolerance: relative to the faster side...
const SEAM_VEL_REL: f32 = 0.5;
/// ...and an absolute floor of this many longest-bone lengths per second.
const SEAM_VEL_FLOOR: f32 = 2.0;
/// Samples per second of clip the motion-quality checks look at.
const FINE_HZ: f32 = 60.0;
/// Default `smooth` spike rule: this many times the bone's median
/// non-zero acceleration, and at least `SPIKE_FLOOR` m/s^2.
const SPIKE_RATIO: f32 = 8.0;
const SPIKE_FLOOR: f32 = 5.0;

/// A dense, evenly spaced sampling of the clip (at least 60 per second).
struct Fine {
    ts: Vec<f32>,
    dt: f32,
    wrap: bool,
}

impl Fine {
    fn new(c: &Clip, samples: Option<u32>) -> Fine {
        let n = ((c.duration * FINE_HZ).ceil() as u32)
            .max(samples.unwrap_or(0))
            .max(8);
        let ts = times(c, n);
        let dt = if c.looping {
            c.duration / n as f32
        } else {
            c.duration / (n - 1) as f32
        };
        Fine {
            ts,
            dt: dt.max(1e-6),
            wrap: c.looping,
        }
    }

    /// Tail (tip) of every bone at every sample: `[bone][sample]`.
    fn tips(&self, c: &Clip, s: &Skeleton) -> Vec<Vec<Vec3>> {
        let mut out = vec![Vec::with_capacity(self.ts.len()); s.bones.len()];
        for &t in &self.ts {
            for (o, (_, tail)) in out.iter_mut().zip(in_place(c, s, t)) {
                o.push(tail);
            }
        }
        out
    }
}

fn foot_bones(s: &Skeleton, o: &FootSlideOpts) -> Vec<usize> {
    if let Some(pats) = &o.bones {
        return (0..s.bones.len())
            .filter(|&i| {
                pats.iter()
                    .any(|p| crate::name_matches(p, &s.bones[i].name))
            })
            .collect();
    }
    let has = |i: usize, words: &[&str]| {
        let n = s.bones[i].name.to_lowercase();
        words.iter().any(|w| n.contains(w))
    };
    let all = 0..s.bones.len();
    let toes: Vec<usize> = all.clone().filter(|&i| has(i, &["toe"])).collect();
    if !toes.is_empty() {
        return toes;
    }
    let feet: Vec<usize> = all
        .clone()
        .filter(|&i| has(i, &["foot", "paw", "hoof", "ball"]))
        .collect();
    if !feet.is_empty() {
        return feet;
    }
    let leaf = |i: usize| !s.bones.iter().any(|b| b.parent == Some(i));
    let low = all
        .clone()
        .filter(|&i| leaf(i))
        .map(|i| s.bones[i].tail.y)
        .fold(f32::MAX, f32::min);
    all.filter(|&i| leaf(i) && s.bones[i].tail.y <= low + 0.05)
        .collect()
}

/// Bone segments at `t` with any root motion taken out.
fn in_place(c: &Clip, s: &Skeleton, t: f32) -> Vec<(Vec3, Vec3)> {
    s.posed_segments(&c.sample_in_place(s, t))
}

/// `tips` are in place (root motion removed); `travel` (per sample, and
/// the whole loop's delta) puts the travel back, carried on across the
/// seam, so planted feet of a root-motion walk measure 0 in the world
/// with no `speed`.
fn foot_slide(
    label: &str,
    s: &Skeleton,
    f: &Fine,
    tips: &[Vec<Vec3>],
    travel: (&[Vec3], Vec3),
    o: &FootSlideOpts,
) -> (String, bool, String) {
    let (trav, delta) = travel;
    let bones = foot_bones(s, o);
    if bones.is_empty() {
        return (
            label.to_string(),
            false,
            "no foot bones found: name them with `bones`".into(),
        );
    }
    let ch = o.contact_height.unwrap_or(0.03);
    let tol = o.tol.unwrap_or(0.01);
    let [vx, vz] = o.speed.unwrap_or([0.0, 0.0]);
    let n = f.ts.len();
    let mut worst: Option<(f32, usize, f32, f32)> = None; // slide, bone, from, to
    let mut shares = Vec::new();
    for &b in &bones {
        let p = &tips[b];
        let planted = |i: usize| p[i].y <= ch;
        let count = (0..n).filter(|&i| planted(i)).count();
        shares.push(format!(
            "`{}` {:.0}%",
            s.bones[b].name,
            100.0 * count as f32 / n as f32
        ));
        // Start just after an airborne sample so a run across the seam is
        // one run.
        let start = if f.wrap {
            (0..n)
                .find(|&i| !planted(i))
                .map(|i| (i + 1) % n)
                .unwrap_or(0)
        } else {
            0
        };
        let mut run: Option<(usize, usize)> = None; // (origin index, steps)
        for k in 0..n {
            let i = (start + k) % n;
            if !planted(i) {
                run = None;
                continue;
            }
            let (o_i, steps) = match run {
                Some((o_i, st)) => (o_i, st + 1),
                None => (i, 0),
            };
            run = Some((o_i, steps));
            let dt = steps as f32 * f.dt;
            // Loops crossed since the run began (the run wrapped).
            let cycles = if i < o_i || (steps > 0 && i == o_i) {
                1.0
            } else {
                0.0
            };
            let w =
                |j: usize, c: f32| p[j] + trav.get(j).copied().unwrap_or(Vec3::ZERO) + delta * c;
            let (pi, po) = (w(i, cycles), w(o_i, 0.0));
            let dx = pi.x - po.x + vx * dt;
            let dz = pi.z - po.z + vz * dt;
            let d = (dx * dx + dz * dz).sqrt();
            if worst.is_none_or(|w| d > w.0) {
                worst = Some((d, b, f.ts[o_i], f.ts[i]));
            }
        }
    }
    let planted = shares.join(", ");
    match worst {
        Some((d, b, t0, t1)) if d > tol => (
            label.to_string(),
            false,
            format!(
                "`{}` slides {d:.3} m while planted between t = {t0:.3} and t = {t1:.3} (tol {tol}{}); planted share: {planted}",
                s.bones[b].name,
                if o.speed.is_some() {
                    format!(", after removing speed [{vx}, {vz}]")
                } else {
                    ", speed 0: pass `speed` for a walk in place".into()
                }
            ),
        ),
        Some((d, b, t0, t1)) => (
            label.to_string(),
            true,
            format!(
                "worst slide {d:.4} m on `{}` (t = {t0:.3}..{t1:.3}, tol {tol}); planted share: {planted}",
                s.bones[b].name
            ),
        ),
        None => (
            label.to_string(),
            true,
            format!("no foot is ever within {ch} m of the ground (planted share: {planted})"),
        ),
    }
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn smooth_check(
    label: &str,
    s: &Skeleton,
    f: &Fine,
    tips: &[Vec<Vec3>],
    o: &SmoothOpts,
) -> (String, bool, String) {
    let n = f.ts.len();
    let dt2 = f.dt * f.dt;
    // Sample indices whose neighbours exist (all of them when looping).
    let range: Vec<usize> = if f.wrap {
        (0..n).collect()
    } else {
        (1..n.saturating_sub(1)).collect()
    };
    let at = |i: usize, d: isize| -> usize { ((i as isize + d).rem_euclid(n as isize)) as usize };
    let mut bad: Vec<(f32, String)> = Vec::new();
    let (mut max_a, mut max_j) = (0.0f32, 0.0f32);
    for (b, p) in tips.iter().enumerate() {
        let acc: Vec<Vec3> = range
            .iter()
            .map(|&i| (p[at(i, 1)] - p[i] * 2.0 + p[at(i, -1)]) / dt2)
            .collect();
        let mag: Vec<f32> = acc.iter().map(|a| a.length()).collect();
        let mut nz: Vec<f32> = mag.iter().copied().filter(|&a| a > 0.01).collect();
        if nz.is_empty() {
            continue;
        }
        let med = median(&mut nz);
        let (ai, am) = mag
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
            .map(|(i, a)| (i, *a))
            .unwrap_or((0, 0.0));
        max_a = max_a.max(am);
        let limit = o.max_accel.unwrap_or((SPIKE_RATIO * med).max(SPIKE_FLOOR));
        if am > limit {
            let t = f.ts[range[ai]];
            let against = if o.max_accel.is_some() {
                format!("a limit of {limit:.1}")
            } else {
                format!("a median {med:.1}")
            };
            bad.push((
                am / limit,
                format!(
                    "pop at t={t:.2} on `{}`: {am:.1} m/s^2 against {against}",
                    s.bones[b].name
                ),
            ));
        }
        // Jerk between consecutive acceleration samples.
        for w in 1..acc.len() {
            if !f.wrap && range[w] != range[w - 1] + 1 {
                continue;
            }
            let j = (acc[w] - acc[w - 1]).length() / f.dt;
            max_j = max_j.max(j);
            if let Some(lim) = o.max_jerk
                && j > lim
            {
                bad.push((
                    j / lim,
                    format!(
                        "jerk {j:.0} m/s^3 at t={:.2} on `{}` (limit {lim})",
                        f.ts[range[w]], s.bones[b].name
                    ),
                ));
                break;
            }
        }
        if f.wrap && !acc.is_empty() {
            let j = (acc[0] - acc[acc.len() - 1]).length() / f.dt;
            max_j = max_j.max(j);
        }
    }
    if bad.is_empty() {
        (
            label.to_string(),
            true,
            format!(
                "no spikes over {n} samples ({:.0}/s): peak tip acceleration {max_a:.1} m/s^2, peak jerk {max_j:.0} m/s^3",
                1.0 / f.dt
            ),
        )
    } else {
        bad.sort_by(|a, b| b.0.total_cmp(&a.0));
        let more = bad.len().saturating_sub(3);
        let mut text = bad
            .iter()
            .take(3)
            .map(|b| b.1.clone())
            .collect::<Vec<_>>()
            .join("; ");
        if more > 0 {
            text += &format!(" and {more} more bone(s)");
        }
        (label.to_string(), false, text)
    }
}

fn cross2(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Counter-clockwise convex hull (monotone chain).
fn hull2(mut p: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let mut h: Vec<[f32; 2]> = Vec::new();
    for pass in 0..2 {
        let base = h.len();
        let it: Box<dyn Iterator<Item = &[f32; 2]>> = if pass == 0 {
            Box::new(p.iter())
        } else {
            Box::new(p.iter().rev())
        };
        for &q in it {
            while h.len() >= base + 2 && cross2(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    h
}

fn dist_seg(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let l2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if l2 < 1e-12 {
        0.0
    } else {
        (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / l2).clamp(0.0, 1.0)
    };
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()
}

/// Signed distance from `p` to the hull's boundary: positive inside,
/// negative outside (a point or segment hull is always outside, or 0 on it).
fn hull_margin(h: &[[f32; 2]], p: [f32; 2]) -> f32 {
    let n = h.len();
    let d = (0..n)
        .map(|i| dist_seg(p, h[i], h[(i + 1) % n]))
        .fold(f32::MAX, f32::min);
    let inside = n >= 3 && (0..n).all(|i| cross2(h[i], h[(i + 1) % n], p) >= 0.0);
    if inside { d } else { -d }
}

fn balance_check(
    label: &str,
    model: &Model,
    c: &Clip,
    s: &Skeleton,
    ts: &[f32],
    o: &BalanceOpts,
    scope: &[String],
) -> (String, bool, String) {
    let margin = o.margin.unwrap_or(0.0);
    let min_share = o.min_share.unwrap_or(1.0);
    let ch = o.contact_height.unwrap_or(0.02);
    let only = (!scope.is_empty()).then_some(scope);
    let none = std::collections::BTreeMap::new();
    let (mut grounded, mut balanced, mut airborne) = (0usize, 0usize, 0usize);
    let mut worst: Option<(f32, f32)> = None; // overhang, t
    for &t in ts {
        let pm = posed_model(model, c, s, t);
        // Centre of mass from the same mass report as `inspect mass`.
        let (_, json) = crate::inspect::mass_of(&pm, only, &none, 1000.0);
        let Some(com) = json["center_of_mass"]
            .as_array()
            .and_then(|a| Some([a.first()?.as_f64()? as f32, a.get(2)?.as_f64()? as f32]))
        else {
            return (
                label.to_string(),
                false,
                "no closed visible parts to weigh".into(),
            );
        };
        let pts: Vec<[f32; 2]> = pm
            .parts
            .iter()
            .filter(|p| {
                p.visible && only.is_none_or(|o| o.iter().any(|n| crate::name_matches(n, &p.name)))
            })
            .flat_map(|p| {
                (0..p.verts.len()).filter_map(move |i| {
                    let v = p.world_vert(i);
                    (v.y <= ch).then_some([v.x, v.z])
                })
            })
            .collect();
        if pts.is_empty() {
            airborne += 1;
            continue;
        }
        grounded += 1;
        let m = hull_margin(&hull2(pts), com) + margin;
        if m >= -1e-5 {
            balanced += 1;
        }
        let over = (-m).max(0.0);
        if worst.is_none_or(|w| over > w.0) {
            worst = Some((over, t));
        }
    }
    if grounded == 0 {
        return (
            label.to_string(),
            true,
            format!(
                "airborne at all {airborne} samples (nothing within {ch} m of the ground): not judged"
            ),
        );
    }
    let share = balanced as f32 / grounded as f32;
    let (over, t) = worst.unwrap_or((0.0, 0.0));
    let text = format!(
        "{:.0}% of {grounded} grounded samples balanced (need {:.0}%); worst overhang {over:.3} m at t = {t:.3} (margin {margin}){}",
        share * 100.0,
        min_share * 100.0,
        if airborne > 0 {
            format!("; {airborne} airborne sample(s) not judged")
        } else {
            String::new()
        }
    );
    (label.to_string(), share + 1e-6 >= min_share, text)
}

/// The model posed at `t` of a clip (default: the only clip).
pub fn posed(model: &Model, clip: Option<&str>, t: f32) -> Result<Model, crate::ModelerError> {
    let (c, s) = super::clip(model, clip)?;
    Ok(posed_model(model, &c, &s, t))
}

/// `inspect pose`: bone world heads, tails and rotations at `t`.
pub fn pose_report(
    model: &Model,
    clip: Option<&str>,
    t: f32,
    bones: Option<&[String]>,
) -> Result<(String, serde_json::Value), crate::ModelerError> {
    let (c, s) = super::clip(model, clip)?;
    let pose = c.sample(&s, t);
    let world = s.world(&pose);
    let segs = s.posed_segments(&pose);
    let mut lines = vec![format!(
        "pose of clip `{}` at t = {t} (wrapped {:.3}, duration {}{}):",
        c.name,
        c.wrap(t),
        c.duration,
        if c.looping { ", loop" } else { "" }
    )];
    let mut rows = Vec::new();
    for (i, b) in s.bones.iter().enumerate() {
        if let Some(pats) = bones
            && !pats.iter().any(|p| crate::name_matches(p, &b.name))
        {
            continue;
        }
        let (h, tl) = segs[i];
        let (_, r, _) = world[i].to_scale_rotation_translation();
        // Turned from rest: the world rotation less the bone's rest frame.
        let r = if b.rest == bevy::math::Quat::IDENTITY {
            r
        } else {
            (r * b.rest.inverse()).normalize()
        };
        let (ax, ang) = r.to_axis_angle();
        lines.push(format!(
            "- `{}`: head [{:.3}, {:.3}, {:.3}] tail [{:.3}, {:.3}, {:.3}], turned {:.1} deg about [{:.2}, {:.2}, {:.2}] from rest",
            b.name, h.x, h.y, h.z, tl.x, tl.y, tl.z, ang.to_degrees(), ax.x, ax.y, ax.z
        ));
        rows.push(serde_json::json!({
            "bone": b.name, "head": h.to_array(), "tail": tl.to_array(), "rotation": r.to_array()
        }));
    }
    Ok((
        lines.join("\n"),
        serde_json::json!({"clip": c.name, "time": t, "bones": rows}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(items: serde_json::Value) -> crate::commands::Session {
        let mut s = crate::commands::Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("items must be an array")
        };
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    fn run(s: &crate::commands::Session, spec: serde_json::Value) -> Vec<(String, bool, String)> {
        let spec: AnimationSpec = serde_json::from_value(spec).unwrap_or_else(|e| panic!("{e}"));
        animation(&s.model, &spec, &[])
    }

    fn find<'a>(r: &'a [(String, bool, String)], what: &str) -> &'a (String, bool, String) {
        r.iter()
            .find(|c| c.0.ends_with(what))
            .unwrap_or_else(|| panic!("no `{what}` result in {r:?}"))
    }

    /// Two feet that plant for half the cycle, sliding back at 1 m/s
    /// relative to the root, then lift and swing forward.
    fn feet() -> crate::commands::Session {
        session(json!([
            {"type": "skeleton", "name": "rig", "bones": {
                "foot_l": {"head": [0.1, 0.1, 0.0], "tail": [0.1, 0.0, 0.0]},
                "foot_r": {"head": [-0.1, 0.1, 0.0], "tail": [-0.1, 0.0, 0.0]}}},
            {"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1.0,
             "interp": "linear", "tracks": {"foot_l": {"translate": [
                [0, [0, 0, 0]], [0.5, [0, 0, -0.5]], [0.52, [0, 0.1, -0.5]],
                [0.98, [0, 0.1, 0]], [1.0, [0, 0, 0]]]}},
             "mirror": {"foot_r": {"from": "foot_l", "phase": 0.5}}}
        ]))
    }

    #[test]
    fn foot_slide_follows_the_stated_speed() {
        let s = feet();
        let ok = run(
            &s,
            json!({"foot_slide": {"speed": [0, 1], "tol": 0.02}, "loop": false}),
        );
        let r = find(&ok, "foot_slide");
        assert!(r.1, "{}", r.2);
        assert!(
            r.2.contains("`foot_l` 50%") || r.2.contains("`foot_l` 5"),
            "{}",
            r.2
        );
        for speed in [json!([0, 0.5]), json!([0, 0]), json!([0.5, 1])] {
            let bad = run(
                &s,
                json!({"foot_slide": {"speed": speed, "tol": 0.02}, "loop": false}),
            );
            let r = find(&bad, "foot_slide");
            assert!(!r.1, "{}", r.2);
            assert!(r.2.contains("`foot_"), "{}", r.2);
        }
        // `true` picks the foot bones and, with no speed, fails the slide.
        let t = run(&s, json!({"foot_slide": true, "loop": false}));
        assert!(!find(&t, "foot_slide").1);
    }

    #[test]
    fn the_clips_own_speed_is_the_default() {
        let s = feet();
        let spec: AnimationSpec = serde_json::from_value(
            json!({"foot_slide": {"tol": 0.02}, "gait": false, "loop": false}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (mut c, sk) = crate::anim::clip(&s.model, None).unwrap_or_else(|e| panic!("{e}"));
        // No speed anywhere: the feet slide.
        let r = animation_of(&s.model, &c, &sk, &spec, &[]);
        assert!(!find(&r, "foot_slide").1);
        // The clip's own speed makes the same check pass; a wrong one fails.
        c.speed = Some([0.0, 1.0]);
        let r = animation_of(&s.model, &c, &sk, &spec, &[]);
        assert!(find(&r, "foot_slide").1, "{}", find(&r, "foot_slide").2);
        c.speed = Some([0.0, 0.5]);
        let r = animation_of(&s.model, &c, &sk, &spec, &[]);
        assert!(!find(&r, "foot_slide").1);
        // An explicit speed wins over the clip's.
        let spec: AnimationSpec = serde_json::from_value(
            json!({"foot_slide": {"tol": 0.02, "speed": [0, 1]}, "loop": false}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let r = animation_of(&s.model, &c, &sk, &spec, &[]);
        assert!(find(&r, "foot_slide").1);
    }

    fn bar_rig(clip: serde_json::Value) -> crate::commands::Session {
        session(json!([
            {"type": "primitive", "name": "bar", "kind": "box", "size": [0.2, 1.0, 0.2],
             "translation": [0, 0.5, 0]},
            {"type": "skeleton", "name": "rig", "bones": {
                "lower": {"head": [0, 0, 0], "tail": [0, 0.5, 0]},
                "upper": {"parent": "lower", "head": [0, 0.5, 0], "tail": [0, 1, 0]}}},
            {"type": "bind", "name": "b", "skeleton": "rig", "parts": ["bar"], "mode": "smooth"},
            clip
        ]))
    }

    #[test]
    fn volume_catches_a_collapsing_bend_and_passes_a_translate() {
        let bent = bar_rig(json!({"type": "clip", "name": "bend", "skeleton": "rig",
            "duration": 1.0, "loop": false, "interp": "linear", "tracks": {"upper": {
                "rotate": [[0, {"z": 0}], [1, {"z": 150}]]}}}));
        let r = run(&bent, json!({"volume": true, "loop": false}));
        let v = find(&r, "volume");
        assert!(!v.1, "{}", v.2);
        assert!(v.2.contains("`bar`") && v.2.contains("`"), "{}", v.2);
        let moved = bar_rig(json!({"type": "clip", "name": "slide", "skeleton": "rig",
            "duration": 1.0, "loop": false, "interp": "linear", "tracks": {"lower": {
                "translate": [[0, [0, 0, 0]], [1, [1, 0, 0]]]}}}));
        let r = run(&moved, json!({"volume": true, "loop": false}));
        let v = find(&r, "volume");
        assert!(v.1, "{}", v.2);
        assert!(v.2.contains("worst volume"), "{}", v.2);
    }

    #[test]
    fn foot_bone_defaults() {
        let s = feet();
        let (_, sk) = crate::anim::clip(&s.model, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(foot_bones(&sk, &FootSlideOpts::default()).len(), 2);
        let o = FootSlideOpts {
            bones: Some(vec!["*_r".into()]),
            ..Default::default()
        };
        assert_eq!(foot_bones(&sk, &o), vec![1]);
    }

    fn arm(extra: serde_json::Value) -> crate::commands::Session {
        let mut items = vec![
            json!({"type": "skeleton", "name": "rig", "bones": {
                "arm": {"head": [0, 1, 0], "tail": [0.5, 1, 0]}}}),
            json!({"type": "clip", "name": "sine", "skeleton": "rig", "duration": 1.0,
                "tracks": {"arm": {"rotate": {"wave": {"axis": "z", "amount": 30, "period": 1}}}}}),
        ];
        if let serde_json::Value::Array(v) = extra {
            items.extend(v);
        }
        session(serde_json::Value::Array(items))
    }

    #[test]
    fn smooth_passes_a_sine_and_catches_a_pop() {
        let keys: Vec<serde_json::Value> = (0..8)
            .map(|k| {
                let a = 30.0 * (std::f32::consts::TAU * k as f32 / 8.0).sin();
                let a = if k == 3 { a + 40.0 } else { a };
                json!([k as f32 / 8.0, {"z": a}])
            })
            .collect();
        let s = arm(json!([
            {"type": "clip", "name": "pop", "skeleton": "rig", "duration": 1.0,
             "tracks": {"arm": {"rotate": keys}}}
        ]));
        let ok = run(&s, json!({"clip": "sine", "smooth": true}));
        let r = find(&ok, "smooth");
        assert!(r.1, "{}", r.2);
        let bad = run(&s, json!({"clip": "pop", "smooth": true}));
        let r = find(&bad, "smooth");
        assert!(!r.1, "{}", r.2);
        assert!(
            r.2.contains("pop at t=") && r.2.contains("`arm`"),
            "{}",
            r.2
        );
        // An explicit limit is absolute.
        let lim = run(&s, json!({"clip": "sine", "smooth": {"max_accel": 1.0}}));
        assert!(!find(&lim, "smooth").1);
    }

    #[test]
    fn loop_compares_velocity_at_the_seam() {
        let s = arm(json!([
            {"type": "clip", "name": "periodic", "skeleton": "rig", "duration": 1.0,
             "tracks": {"arm": {"rotate": [[0, {"z": 0}], [0.5, {"z": 40}], [1, {"z": 0}]]}}},
            {"type": "clip", "name": "reverses", "skeleton": "rig", "duration": 1.0,
             "interp": "linear",
             "tracks": {"arm": {"rotate": [[0, {"z": 0}], [0.5, {"z": 40}], [1, {"z": 0}]]}}}
        ]));
        for clip in ["sine", "periodic"] {
            let r = run(&s, json!({"clip": clip}));
            assert!(find(&r, "loop").1, "{clip}: {}", find(&r, "loop").2);
        }
        let r = run(&s, json!({"clip": "reverses"}));
        let l = find(&r, "loop");
        assert!(
            !l.1 && l.2.contains("seam") && l.2.contains("`arm`"),
            "{}",
            l.2
        );
    }

    fn biped(clip: serde_json::Value) -> crate::commands::Session {
        session(json!([
            {"type": "body", "name": "guy", "preset": "biped", "tris": 3000},
            clip
        ]))
    }

    #[test]
    fn balance_passes_an_idle_sway_and_fails_a_lean() {
        let idle = biped(
            json!({"type": "clip", "name": "idle", "skeleton": "guy_rig",
            "duration": 2.0, "tracks": {"spine": {"rotate": {"wave": {"axis": "x", "amount": 2, "period": 2}}}}}),
        );
        let r = run(&idle, json!({"samples": 6, "balance": true}));
        assert!(find(&r, "balance").1, "{}", find(&r, "balance").2);
        let lean = biped(
            json!({"type": "clip", "name": "lean", "skeleton": "guy_rig",
            "duration": 2.0, "tracks": {"spine": {"translate": {"wave": {"axis": "z", "amount": 0.7, "period": 2}}}}}),
        );
        let r = run(&lean, json!({"samples": 6, "balance": true}));
        let b = find(&r, "balance");
        assert!(!b.1 && b.2.contains("overhang"), "{}", b.2);
        // A share short of 1 is allowed when asked for.
        let r = run(
            &lean,
            json!({"samples": 6, "balance": {"min_share": 0.0, "margin": 0.05}}),
        );
        assert!(find(&r, "balance").1);
    }

    /// A world-aligned biped skeleton: hip joints 0.86 m up, legs straight
    /// down, feet forward, arms hanging.
    fn gait_rig() -> serde_json::Value {
        json!({"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
            "hip": {"head": [0, 0.97, 0], "tail": [0, 1.07, 0]},
            "chest": {"parent": "hip", "head": [0, 1.17, 0], "tail": [0, 1.47, 0]},
            "thigh_l": {"parent": "hip", "head": [0.09, 0.935, 0], "tail": [0.09, 0.505, 0]},
            "knee_l": {"parent": "thigh_l", "head": [0.09, 0.505, 0], "tail": [0.05, 0.075, 0]},
            "ankle_l": {"parent": "knee_l", "head": [0.05, 0.075, 0], "tail": [0.05, 0.03, 0.1]},
            "toe_l": {"parent": "ankle_l", "head": [0.05, 0.03, 0.1], "tail": [0.05, 0.025, 0.14]},
            "shoulder_l": {"parent": "chest", "head": [0.18, 1.45, 0], "tail": [0.2, 1.17, 0]},
            "elbow_l": {"parent": "shoulder_l", "head": [0.2, 1.17, 0], "tail": [0.2, 0.93, 0]},
            "wrist_l": {"parent": "elbow_l", "head": [0.2, 0.93, 0], "tail": [0.2, 0.85, 0]}}})
    }

    /// A walk keyed straight from joint-angle tables (FK), the pelvis lowered
    /// so the lowest foot point touches the ground at every key. `knee` /
    /// `thigh` / `ankle` map (percent, table value) to the keyed angle;
    /// `arm` is the arm's swing sign (-1 = opposite the same-side leg).
    fn gait_walk(
        thigh: &dyn Fn(f32, f32) -> f32,
        knee: &dyn Fn(f32, f32) -> f32,
        ankle: &dyn Fn(f32, f32) -> f32,
        arm: f32,
    ) -> crate::commands::Session {
        let dur = 1.08;
        let n = 54;
        let key = |f: &dyn Fn(f32) -> serde_json::Value| -> Vec<serde_json::Value> {
            (0..=n)
                .map(|i| {
                    let p = 100.0 * i as f32 / n as f32;
                    json!([dur * i as f32 / n as f32, f(p)])
                })
                .collect()
        };
        let legs = |hip: Option<Vec<serde_json::Value>>| {
            let mut tracks = json!({
                "thigh_l": {"rotate": key(&|p| json!({"x": -thigh(p, norm_at(NORM_THIGH, p))}))},
                "knee_l": {"rotate": key(&|p| json!({"x": knee(p, norm_at(NORM_KNEE, p))}))},
                "ankle_l": {"rotate": key(&|p| json!({"x": -ankle(p, norm_at(NORM_ANKLE, p))}))},
                "shoulder_l": {"rotate": key(&|p| {
                    json!({"x": arm * 15.0 * (std::f32::consts::TAU * (p - 50.0) / 100.0).cos()})
                })}
            });
            if let (Some(h), Some(t)) = (hip, tracks.as_object_mut()) {
                t.insert("hip".into(), json!({"translate": h}));
            }
            json!({"type": "clip", "name": "walk", "skeleton": "rig", "duration": dur,
                "interp": "linear", "tracks": tracks,
                "mirror": {"thigh_r": {"from": "thigh_l", "phase": 0.5},
                           "knee_r": {"from": "knee_l", "phase": 0.5},
                           "ankle_r": {"from": "ankle_l", "phase": 0.5},
                           "shoulder_r": {"from": "shoulder_l", "phase": 0.5}}})
        };
        let raw = session(json!([gait_rig(), legs(None)]));
        let (c, s) = crate::anim::clip(&raw.model, None).unwrap_or_else(|e| panic!("{e}"));
        let rest: Vec<(Vec3, Vec3)> = s.bones.iter().map(|b| (b.head, b.tail)).collect();
        let idx = |n: &str| {
            s.bones
                .iter()
                .position(|b| b.name == n)
                .unwrap_or_else(|| panic!("no bone {n}"))
        };
        let hip: Vec<serde_json::Value> = (0..=n)
            .map(|i| {
                let t = dur * i as f32 / n as f32;
                let seg = in_place(&c, &s, t);
                // The stance leg(s) stand on the ground: left from 0 to 62%,
                // right half a cycle later.
                let p = 100.0 * i as f32 / n as f32;
                let low = ["l", "r"]
                    .iter()
                    .filter(|sd| {
                        let q = if **sd == "l" { p } else { (p + 50.0) % 100.0 };
                        q < 62.0
                    })
                    .map(|sd| {
                        let toe = idx(&format!("toe_{sd}"));
                        let (heel, tip) = gait_foot_heights(
                            &seg,
                            &rest,
                            idx(&format!("thigh_{sd}")),
                            idx(&format!("knee_{sd}")),
                            idx(&format!("ankle_{sd}")),
                            toe,
                            Vec3::Z,
                        );
                        heel.min(tip - rest[toe].1.y)
                    })
                    .fold(f32::MAX, f32::min);
                json!([t, [0, -low, 0]])
            })
            .collect();
        session(json!([gait_rig(), legs(Some(hip))]))
    }

    fn gait_run(s: &crate::commands::Session) -> Vec<(String, bool, String)> {
        run(s, json!({"loop": false, "gait": true}))
            .into_iter()
            .filter(|r| r.0.contains("gait"))
            .collect()
    }

    #[test]
    fn gait_takes_the_clips_own_speed_by_default() {
        let s = gait_walk(&|_, v| v, &|_, v| v, &|_, v| v, -1.0);
        let spec: AnimationSpec = serde_json::from_value(json!({"loop": false, "gait": true}))
            .unwrap_or_else(|e| panic!("{e}"));
        let (mut c, sk) = crate::anim::clip(&s.model, None).unwrap_or_else(|e| panic!("{e}"));
        let stride = |c: &Clip| {
            let r = animation_of(&s.model, c, &sk, &spec, &[]);
            find(&r, "gait stride").2.clone()
        };
        assert!(stride(&c).contains("measured"), "{}", stride(&c));
        // A mocap clip's removed travel is the speed (and sets the stride).
        c.speed = Some([0.0, 1.37]);
        let d = stride(&c);
        assert!(d.contains("speed 1.37 m/s (the clip's speed"), "{d}");
        // A stated speed wins over it.
        let spec2: AnimationSpec =
            serde_json::from_value(json!({"loop": false, "gait": {"speed": [0, 0.9]}}))
                .unwrap_or_else(|e| panic!("{e}"));
        let r = animation_of(&s.model, &c, &sk, &spec2, &[]);
        let d = &find(&r, "gait stride").2;
        assert!(d.contains("speed 0.90 m/s (stated"), "{d}");
        // The cycle band follows the speed outside the comfortable one.
        let band = |v: f32| {
            let spec: AnimationSpec =
                serde_json::from_value(json!({"loop": false, "gait": {"speed": [0, v]}}))
                    .unwrap_or_else(|e| panic!("{e}"));
            let r = animation_of(&s.model, &c, &sk, &spec, &[]);
            let d = find(&r, "gait timing").2.clone();
            d.split("(normal ")
                .nth(1)
                .and_then(|x| x.split(" s").next())
                .and_then(|x| x.split("..").nth(1))
                .and_then(|x| x.parse::<f32>().ok())
                .unwrap_or_else(|| panic!("{d}"))
        };
        let (slow, comfy, near, fast) = (band(0.7), band(1.3), band(1.2), band(1.9));
        assert!(
            slow > comfy * 1.1 && fast < comfy * 0.97,
            "{slow} {comfy} {fast}"
        );
        assert_eq!(
            comfy, near,
            "the band is unchanged near the comfortable speed"
        );
    }

    #[test]
    fn gait_passes_the_normative_curves() {
        let s = gait_walk(&|_, v| v, &|_, v| v, &|_, v| v, -1.0);
        let r = gait_run(&s);
        // Every measure but timing: a walk keyed by angles alone doesn't
        // plant its feet (they skid), so its contact spans are off.
        for name in [
            "thigh",
            "knee",
            "ankle",
            "knee_extension",
            "stride",
            "step_width",
            "pelvis",
            "trunk",
            "arms",
            "toe_clearance",
        ] {
            let c = find(&r, &format!("gait {name}"));
            assert!(c.1, "{name}: {}", c.2);
        }
        let t = find(&r, "gait timing");
        assert!(t.2.contains("cycle 1.08 s"), "{}", t.2);
    }

    #[test]
    fn gait_catches_a_crouched_shuffle() {
        // Knee bent 35 more through stance (a crouch), eased in and out.
        let ease = |u: f32| {
            let u = u.clamp(0.0, 1.0);
            u * u * (3.0 - 2.0 * u)
        };
        let s = gait_walk(
            &|_, v| v,
            &|p, v| v + 35.0 * ease((p - 2.0) / 8.0) * ease((60.0 - p) / 8.0),
            &|_, v| v,
            -1.0,
        );
        let r = gait_run(&s);
        let k = find(&r, "gait knee");
        assert!(!k.1, "{}", k.2);
        assert!(
            k.2.contains("in stance") && k.2.contains("too bent"),
            "{}",
            k.2
        );
    }

    #[test]
    fn gait_catches_a_march_and_in_phase_arms() {
        // High stepping: hip and knee flexed far more in swing.
        let march = gait_walk(
            &|p, v| if p > 62.0 { v + 25.0 } else { v },
            &|p, v| if p > 62.0 { v + 35.0 } else { v },
            &|_, v| v,
            -1.0,
        );
        let r = gait_run(&march);
        let k = find(&r, "gait knee");
        assert!(!k.1 && k.2.contains("swing"), "{}", k.2);
        let toe = find(&r, "gait toe_clearance");
        assert!(!toe.1 && toe.2.contains("high step"), "{}", toe.2);
        // Arms swinging with the same-side leg.
        let s = gait_walk(&|_, v| v, &|_, v| v, &|_, v| v, 1.0);
        let r = gait_run(&s);
        let a = find(&r, "gait arms");
        assert!(!a.1 && a.2.contains("in phase"), "{}", a.2);
    }

    #[test]
    fn gait_skips_a_non_biped() {
        let s = arm(json!([]));
        let r = run(&s, json!({"clip": "sine", "gait": true}));
        let g = find(&r, "gait");
        assert!(g.1 && g.2.contains("skipped"), "{}", g.2);
        let a: AnimationSpec = serde_json::from_value(json!({"gait": {"side": "left",
            "tolerance": 9, "speed": [0, 1.3], "bones": {"thigh": "upleg_{side}"}}}))
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(a.gait, Some(GaitSpec::With(ref o)) if o.tolerance == Some(9.0)));
    }

    #[test]
    fn new_fields_parse() {
        let a: AnimationSpec = serde_json::from_value(json!({
            "foot_slide": {"bones": ["toe_*"], "contact_height": 0.04, "tol": 0.02, "speed": [0, 1.2]},
            "balance": {"margin": 0.02, "min_share": 0.6},
            "smooth": {"max_accel": 30, "max_jerk": 900}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            matches!(a.foot_slide, Some(FootSlideSpec::With(ref o)) if o.speed == Some([0.0, 1.2]))
        );
        assert!(matches!(a.balance, Some(BalanceSpec::With(ref o)) if o.min_share == Some(0.6)));
        assert!(matches!(a.smooth, Some(SmoothSpec::With(ref o)) if o.max_jerk == Some(900.0)));
        let b: AnimationSpec =
            serde_json::from_value(json!({"foot_slide": true, "balance": true, "smooth": false}))
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(b.foot_slide, Some(FootSlideSpec::Flag(true))));
        assert!(matches!(b.smooth, Some(SmoothSpec::Flag(false))));
        assert!(
            serde_json::from_value::<AnimationSpec>(json!({"foot_slide": {"speeed": [0, 1]}}))
                .is_err()
        );
        let v: crate::commands::VerifySpec = serde_json::from_value(
            json!({"animation": {"clip": "walk", "foot_slide": true, "smooth": {"max_accel": 20}}}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(v.animation.is_some());
    }
}
