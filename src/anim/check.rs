//! Rig summaries, lints and `verify` checks for skeletons and clips.

use bevy::math::Vec3;

use super::{Clip, Skeleton};
use crate::{Model, Op};

/// The `rig:` lines of `scene_summary` (none without rig items).
pub fn summary(model: &Model) -> Option<String> {
    let mut lines = Vec::new();
    let skels = super::skeletons(model).ok()?;
    for s in &skels {
        let binds: Vec<String> = model
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::Bind { name, def } if def.skeleton == s.name => {
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
        let clips: Vec<String> = model
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::Clip { name, def } if def.skeleton == s.name => Some(format!(
                    "`{name}` {}s{} ({} track(s))",
                    def.duration,
                    if def.looping { " loop" } else { "" },
                    def.tracks.len() + def.mirror.len()
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
        if !model
            .ops
            .iter()
            .any(|o| matches!(o, Op::Bind { def, .. } if def.skeleton == s.name))
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
    for p in model.parts.iter().filter(|p| p.visible) {
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
    let mut out = Vec::new();
    let (c, s) = match super::clip(model, spec.clip.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            out.push(("animation".to_string(), false, e.to_string()));
            return out;
        }
    };
    let ts = times(&c, spec.samples.unwrap_or(16));
    let in_scope = |n: &str| scope.is_empty() || scope.iter().any(|p| crate::name_matches(p, n));
    let label = |k: &str| format!("animation `{}` {k}", c.name);
    if spec.looping.unwrap_or(c.looping) {
        let a = super::posed_segments_at(&c, &s, 0.0);
        let b = super::posed_segments_at(&c, &s, c.duration - 1e-4);
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
        match worst {
            Some((d, bone)) if d > 0.02 * scale => out.push((
                label("loop"),
                false,
                format!("the end pose doesn't match the start: `{bone}` is {d:.3} m off (make the last key equal the first)"),
            )),
            _ => out.push((label("loop"), true, "the end pose matches the start".into())),
        }
    }
    let max_stretch = spec.stretch.unwrap_or(2.0);
    let mut worst_stretch: (f32, String, f32, usize) = (1.0, String::new(), 0.0, 0);
    let mut floor_fail: Option<(f32, String, f32)> = None;
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
    out
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
