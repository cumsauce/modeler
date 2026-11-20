//! Poses, layers, root motion, events and Euler spins.

use bevy::math::{Quat, Vec3};
use serde_json::{Value, json};

use super::{Clip, Skeleton};

fn session(items: Value) -> crate::commands::Session {
    let mut s = crate::commands::Session::default();
    let Value::Array(items) = items else {
        panic!("items")
    };
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    s
}

fn try_session(items: Value) -> Result<crate::commands::Session, String> {
    let mut s = crate::commands::Session::default();
    let Value::Array(items) = items else {
        panic!("items")
    };
    s.apply_values(None, items).map_err(|e| e.to_string())?;
    Ok(s)
}

fn clip(s: &crate::commands::Session, n: &str) -> (Clip, Skeleton) {
    super::clip(&s.model, Some(n)).unwrap_or_else(|e| panic!("{e}"))
}

/// A biped-ish rig: hip (root), spine, legs with knees and feet, arms.
fn rig() -> Value {
    json!({"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
        "hip": {"head": [0, 1.0, 0], "tail": [0, 1.1, 0]},
        "spine": {"parent": "hip", "head": [0, 1.1, 0], "tail": [0, 1.5, 0]},
        "arm_l": {"parent": "spine", "head": [0.2, 1.45, 0], "tail": [0.5, 1.45, 0]},
        "thigh_l": {"parent": "hip", "head": [0.1, 0.95, 0], "tail": [0.1, 0.5, 0.02]},
        "shin_l": {"parent": "thigh_l", "head": [0.1, 0.5, 0.02], "tail": [0.1, 0.05, 0]},
        "foot_l": {"parent": "shin_l", "head": [0.1, 0.05, 0], "tail": [0.1, 0.0, 0.12]}
    }})
}

fn bone(s: &Skeleton, n: &str) -> usize {
    s.bone(n).unwrap_or_else(|| panic!("bone {n}"))
}

fn qclose(a: Quat, b: Quat) -> bool {
    1.0 - a.normalize().dot(b.normalize()).abs() < 1e-7
}

fn qx(deg: f32) -> Quat {
    Quat::from_rotation_x(deg.to_radians())
}

#[test]
fn pose_keys_hit_the_pose_and_blend() {
    let s = session(json!([rig(),
        {"type": "pose", "name": "crouch", "skeleton": "rig",
         "bones": {"thigh_l": {"rotate": {"x": -60}}, "hip": {"translate": [0, -0.3, 0]}}},
        {"type": "clip", "name": "c", "skeleton": "rig", "duration": 1, "loop": false,
         "interp": "linear", "poses": [[0, "rest"], [0.5, "crouch"], [1, "rest"]]}]));
    let (c, k) = clip(&s, "c");
    let (th, hip) = (bone(&k, "thigh_l"), bone(&k, "hip"));
    let p = c.sample(&k, 0.5);
    assert!(qclose(p[th].1, qx(-60.0)));
    assert!((p[hip].0 - Vec3::new(0.0, -0.3, 0.0)).length() < 1e-5);
    let p = c.sample(&k, 0.25);
    assert!(qclose(p[th].1, qx(-30.0)), "halfway: {:?}", p[th].1);
    assert!((p[hip].0.y + 0.15).abs() < 1e-5);
    let p = c.sample(&k, 0.0);
    assert!(qclose(p[th].1, Quat::IDENTITY));
    // The pose itself is reachable as a static clip.
    let (pc, _) = clip(&s, "crouch");
    assert!(pc.pose);
    assert!(qclose(pc.sample(&k, 0.0)[th].1, qx(-60.0)));
    // ...but is not listed or exported as a clip.
    assert!(!super::clip_names(&s.model).contains(&"crouch".to_string()));
}

#[test]
fn mirrored_pose_fills_the_other_side() {
    let s = session(json!([rig(),
        {"type": "pose", "name": "p", "skeleton": "rig", "mirror": true,
         "bones": {"arm_l": {"rotate": {"z": 40}}, "thigh_r": {"rotate": {"x": 10}}}}]));
    let (c, k) = clip(&s, "p");
    let p = c.sample(&k, 0.0);
    let z = |d: f32| Quat::from_rotation_z(d.to_radians());
    assert!(qclose(p[bone(&k, "arm_l")].1, z(40.0)));
    assert!(
        qclose(p[bone(&k, "arm_r")].1, z(-40.0)),
        "{:?}",
        p[bone(&k, "arm_r")].1
    );
    // `_r` given: `_l` filled from it.
    assert!(qclose(p[bone(&k, "thigh_l")].1, qx(10.0)));
}

#[test]
fn pose_ik_is_solved_once() {
    let target = [0.1, 0.4, 0.3];
    let s = session(json!([rig(),
        {"type": "pose", "name": "kick", "skeleton": "rig",
         "ik": {"foot_l": {"target": target, "pole": [0.1, 0.6, 1.0]}}}]));
    let (c, k) = clip(&s, "kick");
    let segs = k.posed_segments(&c.sample(&k, 0.0));
    let foot = segs[bone(&k, "foot_l")].0;
    assert!(
        (foot - Vec3::from(target)).length() < 1e-3,
        "foot at {foot:?}"
    );
    // Baked to rotations: the static clip has no IK entries left.
    assert!(c.ik.is_empty());
}

#[test]
fn explicit_track_overrides_the_pose() {
    let s = session(json!([rig(),
        {"type": "pose", "name": "crouch", "skeleton": "rig",
         "bones": {"thigh_l": {"rotate": {"x": -60}, "translate": [0, 0.01, 0]}}},
        {"type": "clip", "name": "c", "skeleton": "rig", "duration": 1,
         "poses": [[0, "crouch"], [1, "crouch"]],
         "tracks": {"thigh_l": {"rotate": [[0, {"x": 20}]]}}}]));
    let (c, k) = clip(&s, "c");
    let p = c.sample(&k, 0.5);
    let th = bone(&k, "thigh_l");
    assert!(qclose(p[th].1, qx(20.0)));
    // The pose's translate channel stays.
    assert!((p[th].0.y - 0.01).abs() < 1e-5);
}

fn walk_and_wave(extra: Value) -> crate::commands::Session {
    let mut wave = json!({"type": "clip", "name": "wave", "skeleton": "rig", "over": "walk",
        "additive": true, "bones": ["arm_*", "spine"],
        "tracks": {"arm_l": {"rotate": [[0, {"z": 0}], [0.5, {"z": 60}], [1, {"z": 0}]]},
                   "thigh_l": {"rotate": [[0, {"x": 90}]]}}});
    if let (Value::Object(w), Value::Object(e)) = (&mut wave, extra) {
        w.extend(e);
    }
    session(json!([rig(),
        {"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1,
         "tracks": {"thigh_l": {"rotate": [[0, {"x": 25}], [0.5, {"x": -25}], [1, {"x": 25}]]},
                    "arm_l": {"rotate": [[0, {"x": -20}], [0.5, {"x": 20}], [1, {"x": -20}]]},
                    "hip": {"translate": {"wave": {"axis": "y", "amount": 0.02, "period": 0.5}}}},
         "mirror": {"thigh_r": {"from": "thigh_l", "phase": 0.5}}},
        wave]))
}

#[test]
fn additive_weight_zero_is_the_base() {
    let s = walk_and_wave(json!({"weight": 0}));
    let (w, k) = clip(&s, "walk");
    let (l, _) = clip(&s, "wave");
    assert_eq!(l.duration, 1.0, "duration from `over`");
    for i in 0..9 {
        let t = i as f32 / 8.0;
        assert_eq!(w.sample(&k, t), l.sample(&k, t), "t = {t}");
    }
}

#[test]
fn additive_layer_composes_and_bones_limit_it() {
    let s = walk_and_wave(json!({}));
    let (w, k) = clip(&s, "walk");
    let (l, _) = clip(&s, "wave");
    let (arm, th) = (bone(&k, "arm_l"), bone(&k, "thigh_l"));
    for t in [0.1f32, 0.5, 0.8] {
        let (b, p) = (w.sample(&k, t), l.sample(&k, t));
        let delta = super::rot_at(
            l.tracks
                .iter()
                .find(|x| x.0 == arm)
                .and_then(|x| x.1.rotate.as_ref())
                .unwrap_or_else(|| panic!("arm track")),
            t,
            1.0,
            true,
        );
        assert!(qclose(p[arm].1, b[arm].1 * delta), "t = {t}");
        // thigh_l is outside `bones`: the base's.
        assert_eq!(p[th], b[th]);
    }
    // Half weight: half the offset.
    let h = walk_and_wave(json!({"weight": 0.5}));
    let (hl, _) = clip(&h, "wave");
    let (b, p) = (w.sample(&k, 0.5), hl.sample(&k, 0.5));
    let want = b[arm].1 * Quat::from_rotation_z(30f32.to_radians());
    assert!(qclose(p[arm].1, want));
}

#[test]
fn override_layer_replaces_the_base() {
    let s = walk_and_wave(json!({"additive": false}));
    let (l, k) = clip(&s, "wave");
    let arm = bone(&k, "arm_l");
    let p = l.sample(&k, 0.5);
    assert!(qclose(p[arm].1, Quat::from_rotation_z(60f32.to_radians())));
}

/// A root-motion walk: the hip travels 1.2 m a loop in z and each foot
/// stays put in the world while it is down.
fn rm_walk(root_motion: Value) -> Value {
    let mut c = json!({"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1,
        "interp": "linear",
        "tracks": {
            "hip": {"translate": [[0, [0, 0, 0]], [1, [0, 0, 1.2]]]},
            "foot_l": {"translate": [[0, [0, 0, 0.3]], [0.5, [0, 0, -0.3]], [0.505, [0, 0.1, -0.306]],
                                     [0.995, [0, 0.1, 0.306]], [1, [0, 0, 0.3]]]}},
        "mirror": {"foot_r": {"from": "foot_l", "phase": 0.5}},
        "events": [[0, "footstep_l"], [0.5, "footstep_r", {"volume": 0.8}]]});
    if !root_motion.is_null() {
        c["root_motion"] = root_motion;
    }
    c
}

fn rm_session(root_motion: Value) -> crate::commands::Session {
    session(json!([rig(),
        {"type": "primitive", "name": "body", "kind": "box", "size": [0.3, 0.3, 0.2],
         "translation": [0, 1.0, 0]},
        {"type": "bind", "name": "b", "skeleton": "rig", "parts": ["body"], "bone": "hip"},
        rm_walk(root_motion)]))
}

#[test]
fn root_motion_moves_the_hip_here_not_in_the_export() {
    let s = rm_session(json!(true));
    let (c, k) = clip(&s, "walk");
    let hip = bone(&k, "hip");
    assert!(c.root_motion.is_some());
    // Modeler playback travels.
    assert!((c.sample(&k, 0.5)[hip].0.z - 0.6).abs() < 1e-5);
    // In place: the travel is gone.
    assert!(c.sample_in_place(&k, 0.5)[hip].0.z.abs() < 1e-5);
    let (d, _) = c.root_delta(&k);
    assert!(
        (d - Vec3::new(0.0, 0.0, 1.2)).length() < 1e-3,
        "delta {d:?}"
    );

    let dir = std::env::temp_dir().join(format!("modeler-root-motion-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let path = dir.join("walk.gltf");
    crate::gltf::export_gltf(&s.model, &path).unwrap_or_else(|e| panic!("{e}"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    let anim = &v["animations"][0];
    let rm = &anim["extras"]["root_motion"];
    assert_eq!(rm["bone"], "hip");
    assert_eq!(rm["axes"], "xz");
    let dz = rm["delta"][2].as_f64().unwrap_or(0.0);
    assert!((dz - 1.2).abs() < 1e-3, "{rm}");
    let samples = rm["samples"].as_array().cloned().unwrap_or_default();
    assert!(samples.len() > 10);
    let mid = samples
        .iter()
        .find(|s| (s[0].as_f64().unwrap_or(0.0) - 0.5).abs() < 1e-4)
        .unwrap_or_else(|| panic!("no sample at 0.5"));
    assert!((mid[3].as_f64().unwrap_or(0.0) - 0.6).abs() < 1e-3);
    let ev = anim["extras"]["events"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[1]["name"], "footstep_r");
    assert_eq!(ev[1]["data"]["volume"], 0.8);
    // The hip's exported translation stays at its rest head (z = 0).
    let bin_uri = v["buffers"][0]["uri"].as_str().unwrap_or_default();
    let bin = std::fs::read(dir.join(bin_uri)).unwrap_or_else(|e| panic!("{e}"));
    let hip_node = v["nodes"]
        .as_array()
        .and_then(|n| n.iter().position(|n| n["name"] == "hip"))
        .unwrap_or_else(|| panic!("hip node"));
    let ch = anim["channels"]
        .as_array()
        .and_then(|c| {
            c.iter()
                .find(|c| c["target"]["node"] == hip_node && c["target"]["path"] == "translation")
        })
        .unwrap_or_else(|| panic!("hip translation channel"));
    let sampler = &anim["samplers"][ch["sampler"].as_u64().unwrap_or(0) as usize];
    let acc = &v["accessors"][sampler["output"].as_u64().unwrap_or(0) as usize];
    let view = &v["bufferViews"][acc["bufferView"].as_u64().unwrap_or(0) as usize];
    let off = view["byteOffset"].as_u64().unwrap_or(0) as usize;
    let n = acc["count"].as_u64().unwrap_or(0) as usize;
    for i in 0..n {
        let at = off + i * 12 + 8;
        let z = f32::from_le_bytes([bin[at], bin[at + 1], bin[at + 2], bin[at + 3]]);
        assert!(z.abs() < 1e-3, "exported hip z {z} at sample {i}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn foot_slide(s: &crate::commands::Session) -> ((bool, String), (bool, String)) {
    let spec: super::check::AnimationSpec = serde_json::from_value(
        json!({"clip": "walk", "foot_slide": {"bones": ["foot_*"]}, "loop": true}),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let out = super::check::animation(&s.model, &spec, &[]);
    let pass = |k: &str| {
        out.iter()
            .find(|r| r.0.ends_with(k))
            .map(|r| (r.1, r.2.clone()))
            .unwrap_or_else(|| panic!("no {k} in {out:?}"))
    };
    (pass("loop"), pass("foot_slide"))
}

#[test]
fn foot_slide_passes_a_planted_root_motion_walk_without_speed() {
    let (lp, (ok, why)) = foot_slide(&rm_session(json!({"bone": "hip", "axes": "xz"})));
    assert!(ok, "{why}");
    // In place, the end pose matches the start (the feet's quick lifts
    // trip the seam-velocity half of the check, which is fine here).
    assert!(!lp.1.contains("doesn't match the start"), "loop: {}", lp.1);
    // Without root motion the hip snaps back at the seam under a planted
    // foot, and the loop doesn't close.
    let (lp, (ok, why)) = foot_slide(&rm_session(Value::Null));
    assert!(!ok, "{why}");
    assert!(lp.1.contains("doesn't match the start"), "{}", lp.1);
}

#[test]
fn events_are_validated() {
    for bad in [json!([[2.0, "x"]]), json!([[0.5, ""]]), json!([[0.5]])] {
        let r = try_session(json!([rig(),
            {"type": "clip", "name": "c", "skeleton": "rig", "duration": 1, "events": bad}]));
        assert!(r.is_err(), "{bad} should fail");
    }
    let s = rm_session(json!(true));
    let (c, _) = clip(&s, "walk");
    assert_eq!(c.events.len(), 2);
    assert_eq!(c.events[0].name, "footstep_l");
}

#[test]
fn euler_channel_spins_a_full_turn_and_loops() {
    let s = session(json!([rig(),
        {"type": "clip", "name": "spin", "skeleton": "rig", "duration": 1,
         "tracks": {"hip": {"rotate": {"euler": [[0, {"y": 0}], [1, {"y": 360}]]}}}}]));
    let (c, k) = clip(&s, "spin");
    let hip = bone(&k, "hip");
    let fwd = |t: f32| c.sample(&k, t)[hip].1 * Vec3::Z;
    let ang = |v: Vec3| v.x.atan2(v.z).to_degrees().rem_euclid(360.0);
    // Quarter points: 90, 180, 270 degrees, constant speed through the seam.
    for (t, want) in [(0.25f32, 90.0f32), (0.5, 180.0), (0.75, 270.0)] {
        let a = ang(fwd(t));
        assert!((a - want).abs() < 0.5, "t {t}: {a}");
    }
    let mut last = 0.0f32;
    for i in 1..40 {
        let a = ang(fwd(i as f32 / 40.0));
        assert!(a > last, "monotonic at {i}: {a} after {last}");
        last = a;
    }
    // Loops: t = 1 is t = 0.
    assert!(qclose(c.sample(&k, 0.999_99)[hip].1, Quat::IDENTITY));
    // Mirrored too: a mirrored euler spins the other way.
    let m = session(json!([rig(),
        {"type": "clip", "name": "spin", "skeleton": "rig", "duration": 1,
         "tracks": {"arm_l": {"rotate": {"euler": [[0, {"y": 0}], [1, {"y": 360}]], "interp": "linear"}}},
         "mirror": {"arm_r": {"from": "arm_l"}}}]));
    let (mc, mk) = clip(&m, "spin");
    let r = mc.sample(&mk, 0.25)[bone(&mk, "arm_r")].1 * Vec3::Z;
    assert!((ang(r) - 270.0).abs() < 0.5, "{}", ang(r));
}

#[test]
fn plain_clip_samples_as_before() {
    // The same clip built through the old path (no poses / layers) and
    // through build_clip directly agree bit for bit.
    let item = json!({"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1,
        "tracks": {"thigh_l": {"rotate": [[0, {"x": 25}], [0.5, {"x": -25}], [1, {"x": 25}]]},
                   "hip": {"translate": {"wave": {"axis": "y", "amount": 0.02, "period": 0.5}}}},
        "mirror": {"thigh_r": {"from": "thigh_l", "phase": 0.5}}});
    let s = session(json!([rig(), item]));
    let (c, k) = clip(&s, "walk");
    let def = s
        .model
        .ops
        .iter()
        .find_map(|o| match o {
            crate::Op::Clip { def, .. } => Some(def.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("clip op"));
    // Saved form carries none of the new fields.
    let saved = serde_json::to_value(&def).unwrap_or_default();
    for f in [
        "poses",
        "over",
        "additive",
        "weight",
        "bones",
        "root_motion",
        "events",
        "interp",
    ] {
        assert!(saved.get(f).is_none(), "{f} saved: {saved}");
    }
    let d = super::build_clip("walk", &def, &k, &[]).unwrap_or_else(|e| panic!("{e}"));
    for i in 0..17 {
        let t = i as f32 / 16.0;
        let (a, b) = (c.sample(&k, t), d.sample(&k, t));
        assert_eq!(a, b);
        // The old evaluator: rest, then each track's (t, r, s) replacing it.
        let mut old = k.rest();
        for (bone, track, mirrored, shift) in &c.tracks {
            let tt = c.wrap(t + shift);
            let mut r = track
                .rotate
                .as_ref()
                .map(|ch| super::rot_at(ch, tt, 1.0, true))
                .unwrap_or(Quat::IDENTITY);
            let mut tr = track
                .translate
                .as_ref()
                .map(|ch| super::vec_at(ch, tt, 1.0, true, 0.0))
                .unwrap_or(Vec3::ZERO);
            if *mirrored {
                r = Quat::from_xyzw(r.x, -r.y, -r.z, r.w);
                tr.x = -tr.x;
            }
            old[*bone] = (tr, r, Vec3::ONE);
        }
        for (x, y) in old.iter().zip(&a) {
            assert_eq!(
                x.0.to_array().map(f32::to_bits),
                y.0.to_array().map(f32::to_bits)
            );
            assert_eq!(
                x.1.to_array().map(f32::to_bits),
                y.1.to_array().map(f32::to_bits)
            );
        }
    }
}

#[test]
fn sway_layers_over_a_layered_clip() {
    let mut items = walk_and_wave(json!({}));
    items
        .apply_values(
            None,
            vec![
                json!({"type": "sway", "name": "bounce", "skeleton": "rig", "kind": "spring",
                "over": "wave", "bones": ["arm_*"]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let (c, k) = clip(&items, "bounce");
    assert_eq!(c.duration, 1.0);
    let p = c.sample(&k, 0.3);
    assert!(p.iter().all(|x| x.1.is_finite()));
}

#[test]
fn prefab_instance_carries_layers_root_motion_and_events() {
    let s = rm_session(json!(true));
    let (c, k) = clip(&s, "walk");
    let i = super::Instance {
        name: "inst".into(),
        model: std::sync::Arc::new(s.model.clone()),
        t: Vec3::new(5.0, 0.0, 0.0),
        r: Quat::IDENTITY,
        s: Vec3::splat(2.0),
    };
    let ic = super::instance_clip(&i, &c);
    let ik = super::instance_skeleton(&i, &k);
    assert_eq!(ic.events.len(), 2);
    let (d, _) = ic.root_delta(&ik);
    assert!((d.z - 2.4).abs() < 1e-3, "scaled travel {d:?}");
    let w = walk_and_wave(json!({"weight": 0.5}));
    let (l, lk) = clip(&w, "wave");
    let wi = super::Instance {
        model: std::sync::Arc::new(w.model.clone()),
        ..i
    };
    let il = super::instance_clip(&wi, &l);
    assert!(il.layering.is_some() && il.base.is_some());
    let ilk = super::instance_skeleton(&wi, &lk);
    // Same rotations; translations scale with the instance.
    for (a, b) in il.sample(&ilk, 0.3).iter().zip(l.sample(&lk, 0.3)) {
        assert_eq!(a.1, b.1);
        assert!((a.0 - b.0 * 2.0).length() < 1e-6);
    }
}

// ---- Morph target weights ----

/// A bar bound to a two-bone rig, with two hand-made morph targets on it
/// (`up`: every vertex +0.1 y; `wide`: x doubled), and `items` (clips,
/// poses) added after the part has them, as a human body's shapes are.
fn morph_session(items: Value) -> crate::commands::Session {
    let mut s = session(json!([
        {"type": "primitive", "name": "bar", "kind": "box", "size": [1, 0.2, 0.2], "translation": [0.5, 1, 0]},
        {"type": "skeleton", "name": "rig", "bones": {
            "a": {"head": [0, 1, 0], "tail": [0.5, 1, 0], "radius": 0.1},
            "b": {"parent": "a", "head": [0.5, 1, 0], "tail": [1, 1, 0], "radius": 0.1}}},
        {"type": "bind", "name": "skin", "skeleton": "rig", "parts": ["bar"]},
        {"type": "clip", "name": "bend", "skeleton": "rig", "duration": 1,
         "tracks": {"b": {"rotate": [[0, {"z": 0}], [0.5, {"z": 30}], [1, {"z": 0}]]}}}
    ]));
    let Some(i) = s.model.parts.iter().position(|p| p.name == "bar") else {
        panic!("bar")
    };
    let p = std::sync::Arc::make_mut(&mut s.model.parts[i]);
    let up = vec![[0.0, 0.1, 0.0]; p.verts.len()];
    let wide = p.verts.iter().map(|v| [v[0], 0.0, 0.0]).collect();
    p.morphs = Some(std::sync::Arc::new(vec![
        ("up".to_string(), up),
        ("wide".to_string(), wide),
    ]));
    let Value::Array(items) = items else {
        panic!("items")
    };
    for (k, v) in items.iter().enumerate() {
        match crate::spec::parse_item(k, v) {
            Ok(crate::commands::BatchItem::Op(o)) => s.model.ops.push(o),
            Ok(_) => panic!("not an op"),
            Err(e) => panic!("{e}"),
        }
    }
    s
}

fn try_clip(s: &crate::commands::Session, n: &str) -> Result<(Clip, Skeleton), String> {
    super::clip(&s.model, Some(n)).map_err(|e| e.to_string())
}

fn weight(c: &Clip, t: f32, shape: usize) -> f32 {
    c.morph_weights(t)
        .iter()
        .find(|w| w.0 == "bar" && w.1 == shape)
        .map_or(0.0, |w| w.2)
}

#[test]
fn morph_weights_sample_keys_constants_and_waves() {
    let s = morph_session(json!([
        {"type": "clip", "name": "talk", "skeleton": "rig", "duration": 1, "interp": "linear",
         "morphs": {"bar": {"up": [[0, 0], [0.5, 1], [1, 0]], "wide": 0.25}}},
        {"type": "clip", "name": "breathe", "skeleton": "rig", "duration": 2,
         "morphs": {"bar": {"up": {"wave": {"amount": 0.5, "period": 2}}}}}
    ]));
    let (c, _) = clip(&s, "talk");
    assert!((weight(&c, 0.25, 0) - 0.5).abs() < 1e-5);
    assert!((weight(&c, 0.5, 0) - 1.0).abs() < 1e-5);
    assert!((weight(&c, 0.9, 1) - 0.25).abs() < 1e-5);
    assert!(c.key_times().iter().any(|t| (t - 0.5).abs() < 1e-6));
    let (w, _) = clip(&s, "breathe");
    assert!(
        (weight(&w, 0.5, 0) - 0.5).abs() < 1e-4,
        "{}",
        weight(&w, 0.5, 0)
    );
    assert!(weight(&w, 1.5, 0) < -0.49);
}

#[test]
fn posed_vertices_are_morphed_then_skinned() {
    let s = morph_session(json!([
        {"type": "clip", "name": "talk", "skeleton": "rig", "duration": 1, "interp": "linear",
         "tracks": {"b": {"rotate": [[0, {"z": 0}], [1, {"z": 40}]]}},
         "morphs": {"bar": {"up": [[0, 0], [1, 1]], "wide": [[0, 0], [1, 0.5]]}}}
    ]));
    let (c, k) = clip(&s, "talk");
    let t = 0.6;
    let posed = super::posed_parts(&s.model, &c, &k, t);
    let Some(i) = s.model.parts.iter().position(|p| p.name == "bar") else {
        panic!("bar")
    };
    let base = &s.model.parts[i];
    let Some(rig) = &base.rig else {
        panic!("bound")
    };
    let m = base.morphs.as_ref().unwrap_or_else(|| panic!("morphs"));
    let mats = k.skin_matrices(&c.sample(&k, t));
    let (wu, ww) = (weight(&c, t, 0), weight(&c, t, 1));
    assert!((wu - 0.6).abs() < 1e-5 && (ww - 0.3).abs() < 1e-5);
    let mut want = (**base).clone();
    for (v, (du, dw)) in want.verts.iter_mut().zip(m[0].1.iter().zip(&m[1].1)) {
        for c in 0..3 {
            v[c] += wu * du[c] + ww * dw[c];
        }
    }
    let want = super::skin::pose_part(&want, rig, &mats);
    for (a, b) in posed[i].verts.iter().zip(&want.verts) {
        assert!((Vec3::from(*a) - Vec3::from(*b)).length() < 1e-5);
    }
    // The normals stay unit.
    for n in &posed[i].normals {
        assert!((Vec3::from(*n).length() - 1.0).abs() < 1e-4);
    }
}

#[test]
fn zero_morph_weights_are_bit_identical_to_none() {
    let s = morph_session(json!([
        {"type": "clip", "name": "still", "skeleton": "rig", "duration": 1,
         "tracks": {"b": {"rotate": [[0, {"z": 0}], [0.5, {"z": 30}], [1, {"z": 0}]]}},
         "morphs": {"bar": {"up": 0, "wide": [[0, 0], [1, 0]]}}}
    ]));
    let (with, k) = clip(&s, "still");
    let (without, _) = clip(&s, "bend");
    for t in [0.0, 0.3, 0.7] {
        let a = super::posed_parts(&s.model, &with, &k, t);
        let b = super::posed_parts(&s.model, &without, &k, t);
        for (pa, pb) in a.iter().zip(&b) {
            assert_eq!(pa.verts, pb.verts);
            assert_eq!(pa.normals, pb.normals);
        }
    }
}

#[test]
fn pose_keys_carry_morph_weights() {
    let s = morph_session(json!([
        {"type": "pose", "name": "grin", "skeleton": "rig", "morphs": {"bar": {"up": 1}}},
        {"type": "clip", "name": "c", "skeleton": "rig", "duration": 1, "loop": false,
         "interp": "linear", "poses": [[0, "rest"], [0.5, "grin"], [1, "rest"]]}
    ]));
    let (c, _) = clip(&s, "c");
    assert!((weight(&c, 0.5, 0) - 1.0).abs() < 1e-5);
    assert!((weight(&c, 0.25, 0) - 0.5).abs() < 1e-5);
    assert!(weight(&c, 1.0, 0).abs() < 1e-6);
    let (p, _) = clip(&s, "grin");
    assert!((weight(&p, 0.0, 0) - 1.0).abs() < 1e-6);
}

#[test]
fn morph_layers_add_or_replace() {
    let s = morph_session(json!([
        {"type": "clip", "name": "base", "skeleton": "rig", "duration": 1, "interp": "linear",
         "morphs": {"bar": {"up": 0.4}}},
        {"type": "clip", "name": "add", "skeleton": "rig", "over": "base", "additive": true,
         "weight": 0.5, "morphs": {"bar": {"up": 0.2, "wide": 1}}},
        {"type": "clip", "name": "swap", "skeleton": "rig", "over": "base", "weight": 0.5,
         "morphs": {"bar": {"up": 1}}}
    ]));
    let (a, _) = clip(&s, "add");
    assert!(
        (weight(&a, 0.3, 0) - 0.5).abs() < 1e-5,
        "{}",
        weight(&a, 0.3, 0)
    );
    assert!((weight(&a, 0.3, 1) - 0.5).abs() < 1e-5);
    let (r, _) = clip(&s, "swap");
    assert!((weight(&r, 0.3, 0) - 0.7).abs() < 1e-5);
}

#[test]
fn unknown_morph_names_suggest_the_closest() {
    let s = morph_session(json!([
        {"type": "clip", "name": "c", "skeleton": "rig", "duration": 1,
         "morphs": {"bar": {"wdie": 1}}}
    ]));
    let e = try_clip(&s, "c").err().unwrap_or_default();
    assert!(
        e.contains("did you mean `wide`") && e.contains("up, wide"),
        "{e}"
    );
    let s = morph_session(json!([
        {"type": "clip", "name": "c", "skeleton": "rig", "duration": 1,
         "morphs": {"barr": {"up": 1}}}
    ]));
    let e = try_clip(&s, "c").err().unwrap_or_default();
    assert!(e.contains("did you mean `bar`"), "{e}");
}

#[test]
fn morph_weights_export_as_a_weights_channel() {
    let s = morph_session(json!([
        {"type": "clip", "name": "talk", "skeleton": "rig", "duration": 1, "interp": "linear",
         "morphs": {"bar": {"up": [[0, 0], [0.5, 1], [1, 0]]}}}
    ]));
    let dir = std::env::temp_dir().join(format!("modeler-morph-anim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let path = dir.join("bar.glb");
    crate::gltf::export_glb(&s.model, &path).unwrap_or_else(|e| panic!("{e}"));
    let (doc, buffers, _) = ::gltf::import(&path).unwrap_or_else(|e| panic!("{e}"));
    let Some(anim) = doc.animations().find(|a| a.name() == Some("talk")) else {
        panic!("no talk animation")
    };
    let Some(ch) = anim
        .channels()
        .find(|c| c.target().property() == ::gltf::animation::Property::MorphTargetWeights)
    else {
        panic!("no weights channel")
    };
    assert_eq!(ch.target().node().name(), Some("bar"));
    let Some(mesh) = ch.target().node().mesh() else {
        panic!("mesh")
    };
    let prim = mesh.primitives().next().unwrap_or_else(|| panic!("prim"));
    assert_eq!(prim.morph_targets().count(), 2);
    assert!(ch.target().node().skin().is_some(), "skinned and morphed");
    let r = ch.reader(|b| Some(&buffers[b.index()]));
    let times: Vec<f32> = r.read_inputs().map(|i| i.collect()).unwrap_or_default();
    let Some(::gltf::animation::util::ReadOutputs::MorphTargetWeights(w)) = r.read_outputs() else {
        panic!("weights")
    };
    let w: Vec<f32> = w.into_f32().collect();
    assert_eq!(w.len(), times.len() * 2);
    let at = |t: f32| {
        let k = times
            .iter()
            .position(|x| (x - t).abs() < 1e-4)
            .unwrap_or_else(|| panic!("no key at {t}"));
        (w[2 * k], w[2 * k + 1])
    };
    assert!((at(0.5).0 - 1.0).abs() < 1e-5);
    assert!(at(0.0).0.abs() < 1e-5);
    assert_eq!(at(0.5).1, 0.0);
}

#[test]
fn a_human_talks_over_a_walk_through_apply_ops() {
    if let Err(e) = crate::human::asset::load() {
        eprintln!("skipping: {e}");
        return;
    }
    let s = try_session(json!([
        {"type": "body", "name": "eve", "preset": "human", "gender": 0, "height": 1.626,
         "shapes": {"smile": {"expression": {"mouth-corner-puller": 1}}}},
        {"type": "material", "name": "eve", "color": "#c9a58a", "flat": true},
        {"type": "clip", "name": "walk", "skeleton": "eve_rig", "duration": 1,
         "tracks": {"thigh_l": {"rotate": [[0, {"x": 20}], [0.5, {"x": -20}], [1, {"x": 20}]]}}},
        {"type": "pose", "name": "grin", "skeleton": "eve_rig", "morphs": {"eve": {"smile": 1}}},
        {"type": "clip", "name": "talk", "skeleton": "eve_rig", "over": "walk",
         "morphs": {"eve": {"smile": [[0, 0], [0.5, 1], [1, 0]]}}}
    ]));
    let s = match s {
        Ok(s) => s,
        Err(e) => panic!("{e}"),
    };
    let (c, k) = clip(&s, "talk");
    let posed = super::posed_parts(&s.model, &c, &k, 0.5);
    let (rest, _) = clip(&s, "walk");
    let plain = super::posed_parts(&s.model, &rest, &k, 0.5);
    let Some(i) = s.model.parts.iter().position(|p| p.name == "eve") else {
        panic!("eve")
    };
    assert_ne!(posed[i].verts, plain[i].verts, "the smile moved the face");
    let e = try_session(json!([
        {"type": "body", "name": "eve", "preset": "human",
         "shapes": {"smile": {"expression": {"mouth-corner-puller": 1}}}},
        {"type": "clip", "name": "c", "skeleton": "eve_rig", "duration": 1,
         "morphs": {"eve": {"simle": 1}}}
    ]))
    .err()
    .unwrap_or_default();
    assert!(e.contains("did you mean `smile`"), "{e}");
}
