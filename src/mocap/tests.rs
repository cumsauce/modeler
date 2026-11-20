//! Mocap tests on synthesised BVH files (no external data).

use bevy::math::{Quat, Vec3};
use serde_json::{Value, json};

use super::bvh::{self, Builder, Chan};
use crate::anim::{Clip, Skeleton};

const L: f32 = 84.0; // source leg (cm): thigh 42 + shin 42
const STEP: f32 = 25.0; // half a stride (cm)
const V: f32 = 100.0; // cm/s

/// CMU-named joints of a T-pose source in cm, facing +z (left = +x).
fn joints(b: &mut Builder, unit: f32) {
    let u = |x: f32, y: f32, z: f32| Vec3::new(x, y, z) * unit;
    let hips = b.joint("Hips", None, Vec3::ZERO);
    for (s, side) in [(1.0, "Left"), (-1.0, "Right")] {
        let up = b.joint(&format!("{side}UpLeg"), Some(hips), u(9.0 * s, -5.0, 0.0));
        let lg = b.joint(&format!("{side}Leg"), Some(up), u(0.0, -42.0, 0.0));
        let ft = b.joint(&format!("{side}Foot"), Some(lg), u(0.0, -42.0, 0.0));
        let toe = b.joint(&format!("{side}ToeBase"), Some(ft), u(0.0, -8.0, 12.0));
        b.end(toe, u(0.0, 0.0, 6.0));
    }
    let sp = b.joint("Spine", Some(hips), u(0.0, 10.0, 0.0));
    let sp1 = b.joint("Spine1", Some(sp), u(0.0, 15.0, 0.0));
    let neck = b.joint("Neck", Some(sp1), u(0.0, 20.0, 0.0));
    let head = b.joint("Head", Some(neck), u(0.0, 10.0, 0.0));
    b.end(head, u(0.0, 18.0, 0.0));
    for (s, side) in [(1.0, "Left"), (-1.0, "Right")] {
        let sh = b.joint(&format!("{side}Shoulder"), Some(sp1), u(3.0 * s, 13.0, 0.0));
        let arm = b.joint(&format!("{side}Arm"), Some(sh), u(15.0 * s, 0.0, 0.0));
        let fa = b.joint(&format!("{side}ForeArm"), Some(arm), u(28.0 * s, 0.0, 0.0));
        let hd = b.joint(&format!("{side}Hand"), Some(fa), u(26.0 * s, 0.0, 0.0));
        b.end(hd, u(18.0 * s, 0.0, 0.0));
    }
}

/// A walk whose stance foot stays exactly put: per half cycle one straight
/// stance leg (hip height follows it), the other swings with a bent knee;
/// the left arm swings, the spine twists. Returns (root position, local
/// rotations by joint index).
fn walk_pose(b: &Builder, t: f32, unit: f32) -> (Vec3, Vec<Quat>) {
    let n = b.bvh.joints.len();
    let idx = |name: &str| {
        b.bvh
            .joints
            .iter()
            .position(|j| j.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    let mut rots = vec![Quat::IDENTITY; n];
    let ph = t.rem_euclid(1.0);
    let theta_max = (STEP / L).asin();
    let mut stance_theta = 0.0;
    for (side, offset) in [("Left", 0.0), ("Right", 0.5)] {
        let p = (ph - offset).rem_euclid(1.0);
        let (thigh, knee) = if p < 0.5 {
            let u = p / 0.5;
            let th = ((-STEP + 2.0 * STEP * u) / L).asin();
            stance_theta = th;
            (th, 0.0)
        } else {
            let u = (p - 0.5) / 0.5;
            let th = theta_max * (std::f32::consts::PI * u).cos();
            (th, 45f32.to_radians() * (std::f32::consts::PI * u).sin())
        };
        rots[idx(&format!("{side}UpLeg"))] = Quat::from_rotation_x(thigh);
        rots[idx(&format!("{side}Leg"))] = Quat::from_rotation_x(knee);
        rots[idx(&format!("{side}Foot"))] = Quat::from_rotation_x(-(thigh + knee));
    }
    let w = std::f32::consts::TAU * t;
    rots[idx("Spine")] = Quat::from_rotation_y(0.1 * w.sin());
    rots[idx("LeftArm")] = Quat::from_rotation_y(0.5 * w.sin()) * Quat::from_rotation_z(-0.6);
    rots[idx("LeftForeArm")] = Quat::from_rotation_y(0.3 + 0.2 * w.cos());
    rots[idx("RightArm")] = Quat::from_rotation_z(0.9);
    let hip_y = 8.0 + L * stance_theta.cos() + 5.0;
    (Vec3::new(0.0, hip_y, V * t) * unit, rots)
}

fn walk_bvh(order: [Chan; 3], unit: f32, secs: f32) -> String {
    let fps = 60.0;
    let mut b = Builder::new(1.0 / fps, order);
    joints(&mut b, unit);
    for k in 0..=(secs * fps).round() as usize {
        let (root, rots) = walk_pose(&b, k as f32 / fps, unit);
        b.frame(root, &rots);
    }
    bvh::write(&b.bvh)
}

/// A rig with modeler names, other proportions than the source and arms in
/// an A-pose (the source is a T-pose).
fn rig() -> Value {
    let (c, s) = (
        std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    );
    let sh = [0.15f32, 1.38];
    let el = [sh[0] + 0.28 * c, sh[1] - 0.28 * s];
    let wr = [el[0] + 0.26 * c, el[1] - 0.26 * s];
    let tip = [wr[0] + 0.08 * c, wr[1] - 0.08 * s];
    json!({"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
        "hip": {"head": [0, 0.92, 0], "tail": [0, 1.0, 0]},
        "spine": {"parent": "hip", "head": [0, 1.0, 0], "tail": [0, 1.2, 0]},
        "chest": {"parent": "spine", "head": [0, 1.2, 0], "tail": [0, 1.42, 0]},
        "neck": {"parent": "chest", "head": [0, 1.42, 0], "tail": [0, 1.52, 0]},
        "head": {"parent": "neck", "head": [0, 1.52, 0], "tail": [0, 1.72, 0]},
        "clavicle_l": {"parent": "chest", "head": [0.02, 1.38, 0], "tail": [sh[0], sh[1], 0]},
        "shoulder_l": {"parent": "clavicle_l", "head": [sh[0], sh[1], 0], "tail": [el[0], el[1], 0]},
        "elbow_l": {"parent": "shoulder_l", "head": [el[0], el[1], 0], "tail": [wr[0], wr[1], 0]},
        "wrist_l": {"parent": "elbow_l", "head": [wr[0], wr[1], 0], "tail": [tip[0], tip[1], 0]},
        "thigh_l": {"parent": "hip", "head": [0.09, 0.90, 0], "tail": [0.09, 0.49, 0]},
        "knee_l": {"parent": "thigh_l", "head": [0.09, 0.49, 0], "tail": [0.09, 0.08, 0]},
        "ankle_l": {"parent": "knee_l", "head": [0.09, 0.08, 0], "tail": [0.09, 0.02, 0.1]},
        "ball_l": {"parent": "ankle_l", "head": [0.09, 0.02, 0.1], "tail": [0.09, 0.0, 0.16]}
    }})
}

fn session(items: Value) -> Result<crate::commands::Session, String> {
    let mut s = crate::commands::Session::default();
    let Value::Array(items) = items else {
        panic!("items")
    };
    s.apply_values(None, items).map_err(|e| e.to_string())?;
    Ok(s)
}

fn mocap(s: &crate::commands::Session, n: &str) -> (Clip, Skeleton) {
    crate::anim::clip(&s.model, Some(n)).unwrap_or_else(|e| panic!("{e}"))
}

fn apply(item: Value) -> (crate::commands::Session, Clip, Skeleton) {
    let s = session(json!([rig(), item])).unwrap_or_else(|e| panic!("{e}"));
    let (c, k) = mocap(&s, "mc");
    (s, c, k)
}

fn angle(a: Vec3, b: Vec3) -> f32 {
    a.normalize()
        .dot(b.normalize())
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

/// Every mapped bone's posed world direction against its source bone's,
/// at every frame (feet planting off, so no IK changes the legs).
#[test]
fn retargeted_bones_point_where_the_source_bones_point() {
    let text = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0);
    let src = bvh::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    let (_, c, k) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig",
        "bvh": text, "feet": false}));
    let report = c.report.clone().unwrap_or_default();
    assert!(report.contains("shoulder_l<-LeftArm"), "{report}");
    assert!(report.contains("chest<-Spine1"), "{report}");
    assert!(report.contains("file units: cm"), "{report}");
    // (target bone, target child or none, source joint, source child or end)
    let pairs = [
        ("hip", Some("spine"), "Hips", Some("Spine")),
        ("spine", Some("chest"), "Spine", Some("Spine1")),
        ("chest", Some("neck"), "Spine1", Some("Neck")),
        ("neck", Some("head"), "Neck", Some("Head")),
        ("head", None, "Head", None),
        (
            "clavicle_l",
            Some("shoulder_l"),
            "LeftShoulder",
            Some("LeftArm"),
        ),
        (
            "shoulder_l",
            Some("elbow_l"),
            "LeftArm",
            Some("LeftForeArm"),
        ),
        ("elbow_l", Some("wrist_l"), "LeftForeArm", Some("LeftHand")),
        ("wrist_l", None, "LeftHand", None),
        (
            "shoulder_r",
            Some("elbow_r"),
            "RightArm",
            Some("RightForeArm"),
        ),
        ("thigh_l", Some("knee_l"), "LeftUpLeg", Some("LeftLeg")),
        ("knee_l", Some("ankle_l"), "LeftLeg", Some("LeftFoot")),
        ("ankle_r", Some("ball_r"), "RightFoot", Some("RightToeBase")),
        ("ball_l", None, "LeftToeBase", None),
    ];
    let j = |n: &str| src.joints.iter().position(|x| x.name == n).unwrap_or(0);
    let b = |n: &str| k.bone(n).unwrap_or_else(|| panic!("{n}"));
    let mut worst = 0.0f32;
    for f in 0..=60 {
        let t = f as f32 / 30.0;
        let w = k.world(&c.sample(&k, t));
        let sw = src.world(&src.locals_at(t));
        for (tb, tc, sj, sc) in pairs {
            let i = b(tb);
            let td = match tc {
                Some(cn) => w[b(cn)].w_axis.truncate() - w[i].w_axis.truncate(),
                None => {
                    let r = w[i].to_scale_rotation_translation().1;
                    r * k.bones[i].rest.inverse() * (k.bones[i].tail - k.bones[i].head)
                }
            };
            let sd = match sc {
                Some(cn) => sw[j(cn)].0 - sw[j(sj)].0,
                None => sw[j(sj)].1 * src.joints[j(sj)].end.unwrap_or(Vec3::Y),
            };
            let a = angle(td, sd);
            worst = worst.max(a);
            assert!(a < 1.0, "{tb} at {t}: {a:.2}° off");
        }
    }
    println!("worst bone direction error {worst:.4}°");
}

#[test]
fn travel_is_removed_and_reported() {
    let text = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0);
    let (_, c, k) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig", "bvh": text}));
    let report = c.report.clone().unwrap_or_default();
    assert!(report.contains("removed (in place"), "{report}");
    let [vx, vz] = c.speed.unwrap_or_else(|| panic!("speed"));
    // 1 m/s scaled by the leg lengths 0.90 / 0.92.
    let want = 0.90 / 0.92;
    assert!(
        vx.abs() < 1e-3 && (vz - want).abs() < 0.01,
        "speed {vx} {vz}"
    );
    let hip = k.bone("hip").unwrap_or(0);
    for f in 0..=40 {
        let w = k.world(&c.sample(&k, f as f32 * 0.05));
        let p = w[hip].w_axis.truncate();
        assert!(p.z.abs() < 0.05, "hip z {} at frame {f}", p.z);
        assert!((p.y - 0.9).abs() < 0.05, "hip y {}", p.y);
    }
    // root_motion keeps the travel.
    let (_, c2, k2) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig",
        "bvh": walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0), "root_motion": true}));
    let (d, _) = c2.root_delta(&k2);
    assert!((d.z - 2.0 * want).abs() < 0.03, "root delta {d:?}");
}

#[test]
fn planted_feet_stay_within_five_millimetres() {
    let text = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0);
    let (s, c, k) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig", "bvh": text}));
    let report = c.report.clone().unwrap_or_default();
    println!("{report}");
    let def = s
        .model
        .ops
        .iter()
        .find_map(|o| match o {
            crate::Op::Clip { def, .. } => Some(def.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("op"));
    let (_, low) = super::lower("mc", &def, &k).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(low.spans.len(), 2, "{report}");
    let vel = Vec3::new(low.speed[0], 0.0, low.speed[1]);
    let mut worst = 0.0f32;
    for (e, spans) in &low.spans {
        assert!(spans.len() >= 2, "{}: {spans:?}", k.bones[*e].name);
        for &(a, b, p) in spans {
            for f in a..=b {
                let t = (f as f32 * low.dt).min(c.duration - 1e-4);
                let w = k.world(&c.sample(&k, t));
                let d = (w[*e].w_axis.truncate() + vel * t - p).length();
                worst = worst.max(d);
            }
        }
    }
    println!("worst planted slide {:.2} mm", worst * 1000.0);
    assert!(worst < 0.005, "planted slide {worst}");
    assert!(report.contains("planted: worst foot slide"), "{report}");
    assert!(c.events.iter().any(|e| e.name == "footstep_l"));
}

#[test]
fn loop_blend_closes_the_seam() {
    let text = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0);
    let (_, c, _) = apply(
        json!({"type": "mocap", "name": "mc", "skeleton": "rig", "bvh": text,
        "range": [0, 1.7], "loop": "blend", "blend": 0.3, "feet": false}),
    );
    let report = c.report.clone().unwrap_or_default();
    assert!(report.contains("loop seam"), "{report}");
    for (_, tr, _, _) in &c.tracks {
        if let Some(crate::anim::Channel::Keys { keys, .. }) = &tr.rotate {
            let (a, b) = (
                Quat::from_array(keys[0].1),
                Quat::from_array(keys[keys.len() - 1].1),
            );
            assert!(a.dot(b).abs() > 1.0 - 1e-6, "seam {a:?} {b:?}");
        }
    }
    assert!((c.duration - 1.4).abs() < 0.02, "duration {}", c.duration);
}

#[test]
fn mirror_swaps_sides() {
    let text = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0);
    let (_, a, k) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig",
        "bvh": text.clone(), "feet": false}));
    let (_, b, _) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig",
        "bvh": text, "feet": false, "mirror": true}));
    let bn = |n: &str| k.bone(n).unwrap_or(0);
    for f in 0..10 {
        let t = f as f32 * 0.1;
        let wa = k.world(&a.sample(&k, t));
        let wb = k.world(&b.sample(&k, t));
        for (l, r) in [
            ("elbow_l", "elbow_r"),
            ("knee_l", "knee_r"),
            ("wrist_l", "wrist_r"),
        ] {
            let pa = wa[bn(l)].w_axis.truncate();
            let pb = wb[bn(r)].w_axis.truncate();
            let m = Vec3::new(-pa.x, pa.y, pa.z);
            assert!((m - pb).length() < 2e-3, "{l} vs {r} at {t}: {m} {pb}");
        }
    }
}

fn same_clip(a: &Clip, b: &Clip, k: &Skeleton, tol: f32) {
    for f in 0..20 {
        let t = f as f32 * 0.07;
        let wa = k.world(&a.sample(k, t));
        let wb = k.world(&b.sample(k, t));
        for (i, (x, y)) in wa.iter().zip(&wb).enumerate() {
            let d = (x.w_axis - y.w_axis).length();
            assert!(
                d < tol,
                "{} at {t}: {d} {:?} vs {:?}",
                k.bones[i].name,
                x.w_axis,
                y.w_axis
            );
        }
    }
}

#[test]
fn channel_orders_and_units_give_the_same_clip() {
    let base = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0);
    let (_, a, k) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig", "bvh": base}));
    for order in [
        [Chan::Xr, Chan::Yr, Chan::Zr],
        [Chan::Yr, Chan::Xr, Chan::Zr],
    ] {
        let (_, b, _) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig",
            "bvh": walk_bvh(order, 1.0, 2.0)}));
        same_clip(&a, &b, &k, 1e-3);
    }
    let inches = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0 / 2.54, 2.0);
    let (_, b, _) = apply(json!({"type": "mocap", "name": "mc", "skeleton": "rig", "bvh": inches}));
    assert!(
        b.report
            .as_deref()
            .unwrap_or("")
            .contains("file units: inches")
    );
    same_clip(&a, &b, &k, 1e-3);
}

#[test]
fn euler_encoding_matches_bvh_composition() {
    let q = (Quat::from_rotation_x(0.3) * Quat::from_rotation_y(-0.7) * Quat::from_rotation_z(1.1))
        .normalize();
    for order in ["ZXY", "XYZ", "YXZ", "ZYX", "XZY", "YZX"] {
        let o = bvh::order_of(order).unwrap_or_else(|| panic!("{order}"));
        let e = bvh::encode(q, o);
        let mut b = Builder::new(0.1, o);
        let j = b.joint("a", None, Vec3::ZERO);
        b.end(j, Vec3::Y);
        b.frame(Vec3::ZERO, &[q]);
        let p = bvh::parse(&bvh::write(&b.bvh)).unwrap_or_else(|e| panic!("{e}"));
        let r = p.local(0, &p.frames[0]).1;
        assert!(r.dot(q).abs() > 1.0 - 1e-6, "{order}: {e:?}");
    }
}

#[test]
fn parser_is_tolerant_and_errors_name_lines() {
    let ok = "HIERARCHY\r\nROOT Hips\r\n{\r\n\tOFFSET 0 0 0  \r\n\tCHANNELS 6 Xposition Yposition Zposition Yrotation Xrotation Zrotation\r\n\tEnd Site\r\n\t{\r\n\t\tOFFSET 0 1e1 0\r\n\t}\r\n}\r\nMOTION\r\nFrames: 2\r\nFrame Time: 3.333333e-02\r\n0 1 0 0 90 0 \r\n0 1 0 0 0 0\r\n";
    let b = bvh::parse(ok).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(b.frames.len(), 2);
    assert_eq!(b.joints[0].end, Some(Vec3::new(0.0, 10.0, 0.0)));
    let bad = ok.replace("0 1 0 0 0 0", "0 1 0 0 zero 0");
    let e = bvh::parse(&bad).err().unwrap_or_default();
    assert!(e.contains("line 15") && e.contains("zero"), "{e}");
    let short = ok.replace("0 1 0 0 0 0", "0 1 0 0");
    let e = bvh::parse(&short).err().unwrap_or_default();
    assert!(e.contains("line 15") && e.contains("4 of 6"), "{e}");
    let ch = ok.replace("CHANNELS 6", "CHANNELS 5");
    let e = bvh::parse(&ch).err().unwrap_or_default();
    assert!(e.contains("line 5"), "{e}");
    let named = "HIERARCHY\nROOT Bip01 Pelvis\n{\nOFFSET 0 0 0\nCHANNELS 3 Zrotation Xrotation Yrotation\n}\nMOTION\nFrames: 1\nFrame Time: 0.1\n0 0 0\n";
    let b = bvh::parse(named).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(b.joints[0].name, "Bip01 Pelvis");
}

/// A modeler clip written as BVH and read back as a mocap item onto the
/// same rig reproduces the pose.
#[test]
fn a_clip_round_trips_through_bvh() {
    let s = session(json!([rig(),
        {"type": "clip", "name": "wave", "skeleton": "rig", "duration": 1.0,
         "tracks": {"shoulder_l": {"rotate": [[0, {"z": 20}], [0.5, {"z": 60, "y": 20}], [1, {"z": 20}]]},
                    "elbow_l": {"rotate": [[0, {"y": 10}], [0.5, {"y": 70}], [1, {"y": 10}]]},
                    "thigh_l": {"rotate": [[0, {"x": -20}], [0.5, {"x": 10}], [1, {"x": -20}]]},
                    "spine": {"rotate": [[0, {"y": -10}], [0.5, {"y": 10}], [1, {"y": -10}]]}}}]))
    .unwrap_or_else(|e| panic!("{e}"));
    let (c, k) = mocap(&s, "wave");
    let text = bvh::from_clip(&c, &k, 60.0, [Chan::Zr, Chan::Xr, Chan::Yr]);
    let (_, b, _) = apply(
        json!({"type": "mocap", "name": "mc", "skeleton": "rig", "bvh": text,
        "fps": 60, "feet": false, "loop": true}),
    );
    let report = b.report.clone().unwrap_or_default();
    assert!(report.contains("root scale 1.0000"), "{report}");
    same_clip(&c, &b, &k, 2e-3);
}

/// The template human (skips without data/human/human.bin).
#[test]
fn retargets_onto_the_human_rig() {
    if crate::human::asset::load().is_err() {
        eprintln!("skipped: no data/human/human.bin");
        return;
    }
    let text = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 2.0);
    let s = session(json!([
        {"type": "body", "name": "eve", "preset": "human"},
        {"type": "mocap", "name": "mc", "skeleton": "eve_rig", "bvh": text}
    ]))
    .unwrap_or_else(|e| panic!("{e}"));
    let (c, k) = mocap(&s, "mc");
    let report = c.report.clone().unwrap_or_default();
    println!("{report}");
    for want in [
        "hip<-Hips",
        "thigh_l<-LeftUpLeg",
        "elbow_r<-RightForeArm",
        "chest<-Spine1",
    ] {
        assert!(report.contains(want), "{want}: {report}");
    }
    // Upper-arm direction follows the source (the human's arms rest in an
    // A-pose, the source's in a T-pose).
    let src = bvh::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    let j = |n: &str| src.joints.iter().position(|x| x.name == n).unwrap_or(0);
    let b = |n: &str| k.bone(n).unwrap_or_else(|| panic!("{n}"));
    for f in 0..10 {
        let t = f as f32 * 0.1;
        let w = k.world(&c.sample(&k, t));
        let sw = src.world(&src.locals_at(t));
        for (tb, tc, sj, sc) in [
            ("shoulder_l", "elbow_l", "LeftArm", "LeftForeArm"),
            ("spine", "chest", "Spine", "Spine1"),
        ] {
            let td = w[b(tc)].w_axis.truncate() - w[b(tb)].w_axis.truncate();
            let sd = sw[j(sc)].0 - sw[j(sj)].0;
            let a = angle(td, sd);
            assert!(a < 3.0, "{tb} at {t}: {a:.2}° (limits may clamp)");
        }
    }
    let slide: f32 = report
        .split("planted: worst foot slide ")
        .nth(1)
        .and_then(|r| r.split(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(f32::MAX);
    assert!(slide < 5.0, "{report}");
}

#[test]
fn the_server_pass_hashes_files_and_names_bad_lines() {
    let text = walk_bvh([Chan::Zr, Chan::Xr, Chan::Yr], 1.0, 0.5);
    let good = text.clone();
    let read = move |f: &str| -> Result<Vec<u8>, String> {
        match f {
            "mocap/walk.bvh" => Ok(good.clone().into_bytes()),
            "mocap/bad.bvh" => Ok(b"HIERARCHY\nROOT a\n{\nOFFSET 0 x 0\n".to_vec()),
            _ => Err(format!("mocap.file `{f}`: missing")),
        }
    };
    let mut items = vec![json!({"type": "repeat", "var": "r", "count": 1, "items": [
        {"type": "mocap", "name": "mc", "skeleton": "rig", "file": "mocap/walk.bvh"}]})];
    super::resolve_files(&mut items, &read).unwrap_or_else(|e| panic!("{e}"));
    let h = items[0]["items"][0]["hash"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_eq!(h.len(), 16);
    // Applied without the project: the remembered text by hash.
    let item = items[0]["items"][0].clone();
    let (_, c, _) = apply(item);
    assert!(c.report.as_deref().unwrap_or("").contains("onto `rig`"));
    let mut bad =
        vec![json!({"type": "mocap", "name": "b", "skeleton": "rig", "file": "mocap/bad.bvh"})];
    let e = super::resolve_files(&mut bad, &read)
        .err()
        .unwrap_or_default();
    assert!(e.contains("line 4") && e.contains("bad.bvh"), "{e}");
}
