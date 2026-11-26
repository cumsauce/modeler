//! The post-FK stage of [`super::Clip::sample`]: joint limits, inverse
//! kinematics (`clip` `ik` entries) and `look_at` bones, in that order.
//!
//! An IK entry is keyed by its END bone: that bone's head (its joint) is
//! driven to the target by turning the `chain` bones above it. Two-bone
//! chains are solved analytically in the plane of the pole (or the bend
//! the FK pose already has, else the rest pose's), longer ones by FABRIK.
//! An unreachable target straightens the chain toward it (bones never
//! stretch). Everything works on world rotations converted back into the
//! pose's local offsets, so bones with rest rotations and prefab-instance
//! rigs need nothing special.

use bevy::math::{EulerRot, Mat4, Quat, Vec3};

use super::{Channel, Clip, Limits, LookTarget, Pose, Skeleton};

/// A built IK entry.
#[derive(Clone, Debug)]
pub struct Ik {
    /// The end bone: its head goes to the target.
    pub bone: usize,
    /// The bones that bend, top first (the end bone's parent last).
    pub chain: Vec<usize>,
    /// World points (a translate-like channel; mirroring baked in).
    pub target: Channel,
    pub pole: Option<Vec3>,
    /// 0..1 FK to IK (the x of a scale-like channel; default 1).
    pub weight: Option<Channel>,
    /// The end bone's world rotation as an offset from its rest frame.
    pub rotate: Option<Channel>,
    /// A mirror copy of another bone's entry (for display).
    pub mirrored: bool,
    /// Time shift (s), as for mirrored tracks.
    pub shift: f32,
    /// The world offset the target (and pole) gains per loop: the clip's
    /// root-motion delta for keyed targets of a travelling clip (whose
    /// keys are then stored with that trend taken out, so they repeat),
    /// else zero.
    pub cycle: Vec3,
}

fn rot_of(m: &Mat4) -> Quat {
    m.to_scale_rotation_translation().1
}

fn head_of(m: &Mat4) -> Vec3 {
    m.w_axis.truncate()
}

/// The local pose rotation that gives bone `k` the world rotation `w`.
fn local_for(skel: &Skeleton, world: &[Mat4], k: usize, w: Quat) -> Quat {
    let (_, rl) = skel.bones[k].rest_local(&skel.bones);
    let parent = skel.bones[k]
        .parent
        .map(|p| rot_of(&world[p]))
        .unwrap_or(Quat::IDENTITY);
    ((parent * rl).inverse() * w).normalize()
}

/// Clamps a local rotation to the bone's limits (Euler degrees x, then y,
/// then z, as rotations `{x, y, z}` are written). Within limits it is
/// returned untouched.
pub fn clamp(q: Quat, l: &Limits) -> Quat {
    let (z, y, x) = q.to_euler(EulerRot::ZYX);
    let c = |v: f32, r: Option<[f32; 2]>| match r {
        Some([a, _]) if v.to_degrees() < a => (a.to_radians(), true),
        Some([_, b]) if v.to_degrees() > b => (b.to_radians(), true),
        _ => (v, false),
    };
    let ((cx, ax), (cy, ay), (cz, az)) = (c(x, l.x), c(y, l.y), c(z, l.z));
    if !(ax || ay || az) {
        return q;
    }
    Quat::from_euler(EulerRot::ZYX, cz, cy, cx)
}

/// Runs limits, IK and look_at on `pose` at clip time `t`.
pub(super) fn post(clip: &Clip, skel: &Skeleton, t: f32, pose: &mut Pose, off: Vec3) {
    post_levels(&[clip], skel, t, pose, off);
}

/// Runs limits, then every level's IK entries (a layered clip's bases
/// first, each at its own wrapped time), then look_at, once on the final
/// FK pose. `off` moves every target and pole (in-place sampling of a
/// root-motion clip passes minus the travel).
pub(super) fn post_levels(levels: &[&Clip], skel: &Skeleton, t: f32, pose: &mut Pose, off: Vec3) {
    if pose.len() < skel.bones.len() {
        return;
    }
    for (k, b) in skel.bones.iter().enumerate() {
        if let Some(l) = &b.limits {
            pose[k].1 = clamp(pose[k].1, l);
        }
    }
    for clip in levels {
        let (d, lp) = (clip.duration, clip.looping);
        for ik in &clip.ik {
            let tt = clip.wrap(t + ik.shift);
            // A travelling target is its repeating part plus the body's
            // progress through this loop (a mirrored entry is the other
            // foot relative to the body at the same moment, so it shares
            // the progress, not the shifted one).
            let prog = if d > 0.0 { clip.wrap(t) / d } else { 0.0 };
            let target = super::vec_at(&ik.target, tt, d, lp, 0.0) + ik.cycle * prog + off;
            let weight = ik
                .weight
                .as_ref()
                .map(|c| super::vec_at(c, tt, d, lp, 1.0).x.clamp(0.0, 1.0))
                .unwrap_or(1.0);
            let rotate = ik.rotate.as_ref().map(|c| super::rot_at(c, tt, d, lp));
            if ik.cycle == Vec3::ZERO && off == Vec3::ZERO {
                solve(ik, skel, pose, target, weight, rotate);
            } else {
                // The pole travels with the body (its mean progress).
                let mut k = ik.clone();
                k.pole = k.pole.map(|p| p + ik.cycle * prog + off);
                solve(&k, skel, pose, target, weight, rotate);
            }
        }
    }
    for k in 0..skel.bones.len() {
        look(skel, pose, k);
    }
}

/// Drives `ik`'s end bone head to `target`, blended from the FK pose by
/// `weight`; `rotate` sets the end bone's world rotation (offset from its
/// rest frame).
pub fn solve(
    ik: &Ik,
    skel: &Skeleton,
    pose: &mut Pose,
    target: Vec3,
    weight: f32,
    rotate: Option<Quat>,
) {
    if weight.is_nan() || weight <= 0.0 || ik.chain.is_empty() || !target.is_finite() {
        return;
    }
    let fk: Vec<Quat> = ik.chain.iter().map(|&k| pose[k].1).collect();
    let world = skel.world(pose);
    let mut pts: Vec<Vec3> = ik.chain.iter().map(|&k| head_of(&world[k])).collect();
    pts.push(head_of(&world[ik.bone]));
    if let Some(want) = positions(ik, skel, &world, &pts, target) {
        aim(skel, pose, &ik.chain, ik.bone, &want);
    }
    if weight < 1.0 {
        for (i, &k) in ik.chain.iter().enumerate() {
            pose[k].1 = fk[i].slerp(pose[k].1, weight).normalize();
        }
    }
    if let Some(q) = rotate {
        let world = skel.world(pose);
        let w = (q * skel.bones[ik.bone].rest).normalize();
        let l = local_for(skel, &world, ik.bone, w);
        pose[ik.bone].1 = if weight < 1.0 {
            pose[ik.bone].1.slerp(l, weight).normalize()
        } else {
            l
        };
    }
    for &k in ik.chain.iter().chain(std::iter::once(&ik.bone)) {
        if let Some(l) = &skel.bones[k].limits {
            pose[k].1 = clamp(pose[k].1, l);
        }
    }
}

/// Turns each chain bone, top first, so the next joint lies toward
/// `want[i + 1]` (only directions are used).
fn aim(skel: &Skeleton, pose: &mut Pose, chain: &[usize], end: usize, want: &[Vec3]) {
    let n = chain.len();
    for i in 0..n {
        let world = skel.world(pose);
        let k = chain[i];
        let next = if i + 1 < n { chain[i + 1] } else { end };
        let from = head_of(&world[k]);
        let cur = head_of(&world[next]) - from;
        let to = want[i + 1] - from;
        let (Some(a), Some(b)) = (cur.try_normalize(), to.try_normalize()) else {
            continue;
        };
        let w = (Quat::from_rotation_arc(a, b) * rot_of(&world[k])).normalize();
        pose[k].1 = local_for(skel, &world, k, w);
    }
}

/// Wanted joint positions (root first, the end last), or `None` when the
/// chain has no length or the target sits on the root.
fn positions(
    ik: &Ik,
    skel: &Skeleton,
    world: &[Mat4],
    pts: &[Vec3],
    target: Vec3,
) -> Option<Vec<Vec3>> {
    let n = pts.len() - 1;
    let lens: Vec<f32> = pts.windows(2).map(|w| (w[1] - w[0]).length()).collect();
    let reach: f32 = lens.iter().sum();
    let root = pts[0];
    let dv = target - root;
    let d = dv.length();
    if reach < 1e-9 || d < 1e-9 || !d.is_finite() {
        return None;
    }
    let u = dv / d;
    let straight = || {
        let mut out = vec![root];
        let mut acc = root;
        for l in &lens {
            acc += u * *l;
            out.push(acc);
        }
        out
    };
    if d >= reach {
        return Some(straight());
    }
    if n == 1 {
        return Some(vec![root, target]);
    }
    if n == 2 {
        let (a, b) = (lens[0], lens[1]);
        if a < 1e-9 || b < 1e-9 {
            return Some(straight());
        }
        let w = bend_dir(ik, skel, world, pts, u);
        // Closer than the chain folds: fold fully, pointing at it.
        let dd = d.max((a - b).abs() + 1e-6);
        let c = ((a * a + dd * dd - b * b) / (2.0 * a * dd)).clamp(-1.0, 1.0);
        let s = (1.0 - c * c).max(0.0).sqrt();
        let knee = root + (u * c + w * s) * a;
        return Some(vec![root, knee, target]);
    }
    // FABRIK. A chain lying straight along the target line can't fold:
    // nudge its inner joints toward the bend side first.
    let mut q = pts.to_vec();
    let w = bend_dir(ik, skel, world, pts, u);
    let along = (pts[n] - root).try_normalize().unwrap_or(u);
    if along.dot(u) > 0.999 {
        for (i, p) in q.iter_mut().enumerate().take(n).skip(1) {
            *p += w * (0.05 * reach * (i as f32 / n as f32));
        }
    }
    for _ in 0..64 {
        q[n] = target;
        for i in (0..n).rev() {
            let dir = (q[i] - q[i + 1]).try_normalize().unwrap_or(-u);
            q[i] = q[i + 1] + dir * lens[i];
        }
        q[0] = root;
        for i in 0..n {
            let dir = (q[i + 1] - q[i]).try_normalize().unwrap_or(u);
            q[i + 1] = q[i] + dir * lens[i];
        }
        if (q[n] - target).length() < 1e-5 {
            break;
        }
    }
    Some(q)
}

/// The unit direction, square to `u`, the middle joint of a two-bone
/// chain bends toward: toward the pole; else the bend the FK pose has;
/// else the rest pose's bend (carried by the chain root's parent); else
/// a fixed axis of that parent's frame.
fn bend_dir(ik: &Ik, skel: &Skeleton, world: &[Mat4], pts: &[Vec3], u: Vec3) -> Vec3 {
    let perp = |v: Vec3| (v - u * v.dot(u)).try_normalize();
    if let Some(p) = ik.pole
        && let Some(w) = perp(p - pts[0])
    {
        return w;
    }
    let b = &skel.bones;
    let carry = match b[ik.chain[0]].parent {
        Some(p) => rot_of(&world[p]) * b[p].rest.inverse(),
        None => Quat::IDENTITY,
    };
    // The middle joint's offset from the root -> end line, at FK and rest.
    let side = |p0: Vec3, p1: Vec3, p2: Vec3| {
        let line = (p2 - p0).try_normalize()?;
        let off = (p1 - p0) - line * (p1 - p0).dot(line);
        (off.length() > 1e-4 * (p1 - p0).length()).then_some(off)
    };
    let mid = pts.len() / 2;
    let mut cands: Vec<Vec3> = Vec::new();
    if let Some(o) = side(pts[0], pts[mid], pts[pts.len() - 1]) {
        cands.push(o);
    }
    let rest: Vec<Vec3> = ik
        .chain
        .iter()
        .map(|&k| b[k].head)
        .chain(std::iter::once(b[ik.bone].head))
        .collect();
    if let Some(o) = side(rest[0], rest[mid], rest[rest.len() - 1]) {
        cands.push(carry * o);
    }
    for c in cands {
        if let Some(w) = perp(c) {
            return w;
        }
    }
    for ax in [Vec3::Z, Vec3::X, Vec3::Y] {
        if let Some(w) = perp(carry * ax) {
            return w;
        }
    }
    Vec3::Z
}

/// Aims bone `k`'s `look_at` axis at its target.
fn look(skel: &Skeleton, pose: &mut Pose, k: usize) {
    let Some(la) = &skel.bones[k].look_at else {
        return;
    };
    let world = skel.world(pose);
    let head = head_of(&world[k]);
    let target = match la.target {
        LookTarget::Point(p) => p,
        LookTarget::Bone(i) => head_of(&world[i]),
    };
    let w = rot_of(&world[k]);
    let (Some(a), Some(b)) = (
        (w * la.axis).try_normalize(),
        (target - head).try_normalize(),
    ) else {
        return;
    };
    let d = Quat::IDENTITY.slerp(Quat::from_rotation_arc(a, b), la.weight);
    pose[k].1 = local_for(skel, &world, k, (d * w).normalize());
    if let Some(l) = &skel.bones[k].limits {
        pose[k].1 = clamp(pose[k].1, l);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Clip, Skeleton};
    use bevy::math::{Quat, Vec3};
    use serde_json::{Value, json};

    fn session(items: Value) -> crate::commands::Session {
        let mut s = crate::commands::Session::default();
        let Value::Array(items) = items else { panic!() };
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    fn leg(extra: Value) -> Value {
        let mut bones = json!({
            "hip": {"head": [0, 1.0, 0], "tail": [0, 1.1, 0]},
            "thigh": {"parent": "hip", "head": [0, 0.95, 0]},
            "knee": {"parent": "thigh", "head": [0, 0.5, 0.05]},
            "ankle": {"parent": "knee", "head": [0, 0.05, 0], "tail": [0, 0, 0.15]}
        });
        if let (Value::Object(b), Value::Object(e)) = (&mut bones, extra) {
            for (k, v) in e {
                if let Some(Value::Object(o)) = b.get_mut(&k) {
                    if let Value::Object(v) = v {
                        o.extend(v);
                    }
                } else {
                    b.insert(k, v);
                }
            }
        }
        json!({"type": "skeleton", "name": "rig", "bones": bones})
    }

    fn clip_of(skel: Value, clip: Value) -> (Clip, Skeleton) {
        let s = session(json!([skel, clip]));
        let name = clip["name"].as_str().unwrap_or("c").to_string();
        super::super::clip(&s.model, Some(&name)).unwrap_or_else(|e| panic!("{e}"))
    }

    fn heads(c: &Clip, k: &Skeleton, t: f32) -> Vec<Vec3> {
        k.posed_segments(&c.sample(k, t))
            .into_iter()
            .map(|s| s.0)
            .collect()
    }

    fn idx(k: &Skeleton, n: &str) -> usize {
        k.bone(n).unwrap_or_else(|| panic!("no bone {n}"))
    }

    #[test]
    fn two_bone_reaches_with_the_knee_toward_the_pole() {
        for (pole, side) in [(1.0, 1.0), (-1.0, -1.0)] {
            let (c, k) = clip_of(
                leg(json!({})),
                json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                    "ik": {"ankle": {"target": [0, 0.3, 0.2], "pole": [0, 0.6, pole]}}}),
            );
            let h = heads(&c, &k, 0.3);
            let (t, kn, an) = (h[idx(&k, "thigh")], h[idx(&k, "knee")], h[idx(&k, "ankle")]);
            assert!((an - Vec3::new(0.0, 0.3, 0.2)).length() < 1e-3, "{an:?}");
            let u = (an - t).normalize();
            let off = (kn - t) - u * (kn - t).dot(u);
            assert!(off.z * side > 0.01, "knee {kn:?} for pole {pole}");
            // Bone lengths are kept.
            assert!(
                ((kn - t).length() - (k.bones[2].head - k.bones[1].head).length()).abs() < 1e-4
            );
        }
        // Without a pole the knee keeps bending forward as at rest.
        let (c, k) = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "ik": {"ankle": {"target": [0, 0.3, 0.0]}}}),
        );
        let h = heads(&c, &k, 0.0);
        assert!((h[3] - Vec3::new(0.0, 0.3, 0.0)).length() < 1e-3);
        assert!(h[2].z > 0.05, "knee {:?}", h[2]);
    }

    #[test]
    fn rest_rotations_still_reach() {
        let (c, k) = clip_of(
            leg(json!({"thigh": {"roll": 30}, "knee": {"rest": {"x": 20, "y": 40}}})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "tracks": {"thigh": {"rotate": [[0, {"x": 10}]]}},
                "ik": {"ankle": {"target": [0.1, 0.35, 0.2], "rotate": [[0, {"x": 0}]]}}}),
        );
        let p = c.sample(&k, 0.0);
        let h: Vec<Vec3> = k.posed_segments(&p).into_iter().map(|s| s.0).collect();
        assert!(
            (h[3] - Vec3::new(0.1, 0.35, 0.2)).length() < 1e-3,
            "{:?}",
            h[3]
        );
        // rotate keeps the end bone at its rest world rotation.
        let w = k.world(&p);
        let r = w[3].to_scale_rotation_translation().1;
        assert!(r.angle_between(k.bones[3].rest) < 1e-3);
    }

    #[test]
    fn unreachable_target_straightens_toward_it() {
        let (c, k) = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "ik": {"ankle": {"target": [0.3, -2.0, 0.5]}}}),
        );
        let h = heads(&c, &k, 0.0);
        let u = (Vec3::new(0.3, -2.0, 0.5) - h[1]).normalize();
        for (a, b) in [(1, 2), (2, 3)] {
            let d = (h[b] - h[a]).normalize();
            assert!(d.dot(u) > 1.0 - 1e-5, "{d:?} vs {u:?}");
            assert!(d.is_finite());
        }
    }

    #[test]
    fn weight_blends_fk_to_ik() {
        let track = json!({"thigh": {"rotate": [[0, {"x": -20}], [1, {"x": 20}]]}});
        let fk = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0, "tracks": track}),
        );
        let w0 = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0, "tracks": track,
                "ik": {"ankle": {"target": [0, 0.3, 0.2], "weight": 0}}}),
        );
        let w1 = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0, "tracks": track,
                "ik": {"ankle": {"target": [0, 0.3, 0.2], "weight": 1}}}),
        );
        let ik = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0, "tracks": track,
                "ik": {"ankle": {"target": [0, 0.3, 0.2]}}}),
        );
        let half = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0, "tracks": track,
                "ik": {"ankle": {"target": [0, 0.3, 0.2], "weight": [[0, 0], [1, 1]], }}}),
        );
        for t in [0.0, 0.3, 0.7] {
            assert_eq!(
                fk.0.sample(&fk.1, t),
                w0.0.sample(&w0.1, t),
                "weight 0 = FK"
            );
            assert_eq!(
                ik.0.sample(&ik.1, t),
                w1.0.sample(&w1.1, t),
                "weight 1 = IK"
            );
        }
        // Halfway the ankle sits between the FK and IK places.
        let a = heads(&fk.0, &fk.1, 0.5)[3];
        let b = heads(&ik.0, &ik.1, 0.5)[3];
        let m = heads(&half.0, &half.1, 0.5)[3];
        assert!((m - a).length() > 1e-3 && (m - b).length() > 1e-3);
        assert!((m - a).length() < (b - a).length());
    }

    #[test]
    fn planted_foot_stays_while_the_hip_moves() {
        let (c, k) = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0, "interp": "linear",
                "tracks": {"hip": {"translate": [[0, [0, 0, -0.1]], [0.5, [0, -0.04, 0]], [1, [0, 0, 0.1]]]}},
                "ik": {"ankle": {"target": [[0, [0, 0.12, 0.02]], [0.6, [0, 0.12, 0.02]], [1, [0, 0.2, 0.3]]],
                                 "weight": [[0, 1], [0.6, 1], [1, 0]]}}}),
        );
        for i in 0..=12 {
            let t = i as f32 * 0.05;
            let a = heads(&c, &k, t)[3];
            assert!(
                (a - Vec3::new(0.0, 0.12, 0.02)).length() < 1e-3,
                "t {t}: {a:?}"
            );
        }
    }

    #[test]
    fn limits_clamp_tracks() {
        let (c, k) = clip_of(
            leg(json!({"thigh": {"limits": {"x": [-30, 30]}}})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "tracks": {"thigh": {"rotate": [[0, {"x": 60, "y": 10}]]}}}),
        );
        let p = c.sample(&k, 0.0);
        let (z, y, x) = p[1].1.to_euler(bevy::math::EulerRot::ZYX);
        assert!((x.to_degrees() - 30.0).abs() < 1e-3, "{}", x.to_degrees());
        assert!((y.to_degrees() - 10.0).abs() < 1e-3 && z.abs() < 1e-4);
        // A rotation within limits is left untouched.
        let q = Quat::from_rotation_x(0.2);
        let l = &k.bones[1].limits;
        assert_eq!(super::clamp(q, l.as_ref().unwrap_or_else(|| panic!())), q);
    }

    #[test]
    fn look_at_points_the_axis() {
        let target = Vec3::new(1.0, 1.7, 2.0);
        let (c, k) = clip_of(
            leg(
                json!({"head": {"parent": "hip", "head": [0, 1.5, 0], "tail": [0, 1.7, 0],
                "look_at": {"target": [1.0, 1.7, 2.0], "axis": "z"}}}),
            ),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "tracks": {"hip": {"rotate": [[0, {"y": 30}], [1, {"y": -30}]]}}}),
        );
        let h = idx(&k, "head");
        for t in [0.0, 0.4] {
            let w = k.world(&c.sample(&k, t));
            let (_, r, p) = w[h].to_scale_rotation_translation();
            let a = r * Vec3::Z;
            let ang = a.angle_between(target - p).to_degrees();
            assert!(ang < 0.5, "{ang}");
        }
    }

    #[test]
    fn plain_clips_sample_as_before() {
        let (c, k) = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "tracks": {"knee": {"rotate": [[0, {"x": -20}], [0.5, {"x": 40}], [1, {"x": -20}]]},
                           "hip": {"translate": {"wave": {"axis": "y", "amount": 0.02}}}}}),
        );
        assert!(c.ik.is_empty() && !k.constrained());
        for t in [0.0, 0.25, 0.6] {
            let p = c.sample(&k, t);
            let mut want = k.rest();
            for (b, tr, _, _) in &c.tracks {
                let r = tr
                    .rotate
                    .as_ref()
                    .map(|ch| super::super::rot_at(ch, t, 1.0, true))
                    .unwrap_or(Quat::IDENTITY);
                let v = tr
                    .translate
                    .as_ref()
                    .map(|ch| super::super::vec_at(ch, t, 1.0, true, 0.0))
                    .unwrap_or(Vec3::ZERO);
                want[*b] = (v, r, Vec3::ONE);
            }
            assert_eq!(p, want);
        }
    }

    #[test]
    fn mirrored_ik_entry() {
        let skel = json!({"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
            "hip": {"head": [0, 1.0, 0], "tail": [0, 1.1, 0]},
            "thigh_l": {"parent": "hip", "head": [0.1, 0.95, 0]},
            "knee_l": {"parent": "thigh_l", "head": [0.1, 0.5, 0.05]},
            "ankle_l": {"parent": "knee_l", "head": [0.1, 0.05, 0], "tail": [0.1, 0, 0.15]}}});
        let (c, k) = clip_of(
            skel,
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0, "interp": "linear",
                "ik": {"ankle_l": {"target": [[0, [0.12, 0.1, 0.2]], [0.5, [0.12, 0.1, -0.1]], [1, [0.12, 0.1, 0.2]]]}},
                "mirror": {"ankle_r": {"from": "ankle_l", "phase": 0.5}}}),
        );
        let h = heads(&c, &k, 0.0);
        let (l, r) = (h[idx(&k, "ankle_l")], h[idx(&k, "ankle_r")]);
        assert!((l - Vec3::new(0.12, 0.1, 0.2)).length() < 1e-3, "{l:?}");
        assert!((r - Vec3::new(-0.12, 0.1, -0.1)).length() < 1e-3, "{r:?}");
        let kr = h[idx(&k, "knee_r")];
        assert!(kr.z > 0.0 && kr.x < 0.0, "{kr:?}");
        assert!(
            c.ik_bones()
                .iter()
                .any(|(b, m)| *b == idx(&k, "knee_r") && *m)
        );
    }

    #[test]
    fn four_bone_chain_converges() {
        let (c, k) = clip_of(
            json!({"type": "skeleton", "name": "rig", "bones": {
                "b0": {"head": [0, 0, 0]},
                "b1": {"parent": "b0", "head": [0, 0.25, 0]},
                "b2": {"parent": "b1", "head": [0, 0.5, 0]},
                "b3": {"parent": "b2", "head": [0, 0.75, 0]},
                "tip": {"parent": "b3", "head": [0, 1.0, 0], "tail": [0, 1.1, 0]}}}),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "ik": {"tip": {"target": [0.45, 0.55, 0.2], "chain": 4}}}),
        );
        let h = heads(&c, &k, 0.0);
        assert!(
            (h[4] - Vec3::new(0.45, 0.55, 0.2)).length() < 1e-3,
            "{:?}",
            h[4]
        );
        for i in 0..4 {
            assert!(((h[i + 1] - h[i]).length() - 0.25).abs() < 1e-4);
        }
    }

    #[test]
    fn prefab_instance_rigs_carry_ik() {
        let (c, k) = clip_of(
            leg(json!({})),
            json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1.0,
                "ik": {"ankle": {"target": [0, 0.3, 0.2], "pole": [0, 0.6, 1]}}}),
        );
        let inst = super::super::Instance {
            name: "a".into(),
            model: std::sync::Arc::new(crate::Model::default()),
            t: Vec3::new(2.0, 0.0, 1.0),
            r: Quat::from_rotation_y(1.0),
            s: Vec3::splat(1.5),
        };
        let ik = super::super::instance_skeleton(&inst, &k);
        let ic = super::super::instance_clip(&inst, &c);
        let want = inst.t + inst.r * (inst.s * Vec3::new(0.0, 0.3, 0.2));
        let a = heads(&ic, &ik, 0.0)[3];
        assert!((a - want).length() < 1e-3, "{a:?} vs {want:?}");
    }

    /// The walk from docs/ops/clip.md: a body biped's ankles reach their targets.
    #[test]
    fn biped_walk_plants_the_ankles() {
        let s = session(json!([
            {"type": "body", "name": "guy", "preset": "biped", "resolution": 0.03},
            {"type": "clip", "name": "walk", "skeleton": "guy_rig", "duration": 1.0,
             "tracks": {"hip": {"translate": {"wave": {"axis": "y", "amount": 0.02, "period": 0.5}}}},
             "ik": {"ankle_l": {"target": [[0, [0.1, 0.085, 0.2]], [0.5, [0.1, 0.085, -0.2]],
                                           [0.75, [0.1, 0.2, 0]], [1, [0.1, 0.085, 0.2]]],
                                "pole": [0.1, 0.5, 1.0], "rotate": [[0, {"x": 0}]]}},
             "mirror": {"ankle_r": {"from": "ankle_l", "phase": 0.5}}}
        ]));
        let (c, k) = super::super::clip(&s.model, Some("walk")).unwrap_or_else(|e| panic!("{e}"));
        let h = heads(&c, &k, 0.25);
        let l = h[idx(&k, "ankle_l")];
        let r = h[idx(&k, "ankle_r")];
        assert!((l - Vec3::new(0.1, 0.085, 0.0)).length() < 0.01, "{l:?}");
        assert!((r.y - 0.2).abs() < 0.03 && r.x < 0.0, "{r:?}");
        let kn = h[idx(&k, "knee_l")];
        assert!(kn.z > l.z - 1e-3, "knee not behind {kn:?}");
    }
}
