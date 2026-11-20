//! Does a rigged prefab keep its rig when a document instances it?
//! Facts first: each test asserts one expectation about an instance
//! (`inst.*` parts) of a saved `skin` model with a skeleton, bind and clip.

#![allow(clippy::unwrap_used)]

use bevy::math::{Quat, Vec3};
use modeler::commands::Session;
use serde_json::json;

fn setup(tag: &str) -> Session {
    setup_with(
        tag,
        None,
        vec![
            json!({"type": "prefab", "name": "inst", "prefab": "wiggler",
                    "translation": [5, 0, 0]}),
        ],
    )
}

/// Saves the rigged tube (its top bone optionally with a rest roll) and
/// returns a host built from `host` items.
fn setup_with(tag: &str, roll: Option<f32>, host: Vec<serde_json::Value>) -> Session {
    let root = std::env::temp_dir().join(format!(
        "friendslop-prefab-rig-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // SAFETY: each test file runs one test at a time through this lock.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &root) };
    // Build and save the rigged model.
    let mut a = Session::default();
    a.model.name = "wiggler".into();
    a.apply_values(
        None,
        vec![
            json!({"type": "skin", "name": "tube", "smooth": false, "resolution": 0.03,
                   "chains": [[[0, 0.0, 0, 0.1], [0, 0.5, 0, 0.1], [0, 1.0, 0, 0.1]]]}),
            json!({"type": "skeleton", "name": "rig", "bones": {
                "low": {"head": [0, 0.0, 0], "tail": [0, 0.5, 0]},
                "top": {"parent": "low", "head": [0, 0.5, 0], "tail": [0, 1.0, 0],
                        "roll": roll.unwrap_or(0.0)}}}),
            json!({"type": "bind", "name": "tube_bind", "skeleton": "rig", "parts": ["tube"],
                   "mode": "smooth"}),
            json!({"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1.0,
                   "tracks": {"top": {"rotate": [[0, {"z": 0}], [0.5, {"z": 60}], [1, {"z": 0}]]}}}),
        ],
    )
    .unwrap();
    assert!(
        a.model.part("tube").unwrap().rig.is_some(),
        "source is bound"
    );
    a.save_prefab_checked("rigged tube", &["test"], "", true)
        .unwrap();
    // The host instances it, moved.
    let mut h = Session::default();
    h.model.name = "host".into();
    h.apply_values(None, host).unwrap();
    h
}

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn instance_parts_are_present_and_bound() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup("bound");
    let names: Vec<_> = h.model.parts.iter().map(|p| p.name.clone()).collect();
    eprintln!("host parts: {names:?}");
    let p = h.model.part("inst.tube").expect("instance part");
    eprintln!(
        "inst.tube rig: {:?}",
        p.rig.as_ref().map(|r| (&r.skeleton, &r.bind))
    );
    let r = p.rig.as_ref().expect("instance part keeps a Binding");
    assert_eq!(
        (r.skeleton.as_str(), r.bind.as_str()),
        ("inst.rig", "inst.tube_bind")
    );
    assert_eq!(
        r.weights.len(),
        p.verts.len(),
        "smooth weights per vertex survive"
    );
}

#[test]
fn host_sees_the_instance_skeleton_and_clip() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup("skel");
    let skels = modeler::anim::skeletons(&h.model).unwrap();
    let names: Vec<_> = skels.iter().map(|s| s.name.clone()).collect();
    eprintln!("host skeletons: {names:?}");
    assert!(
        names.contains(&"inst.rig".to_string()),
        "skeletons {names:?}"
    );
    let clips = modeler::anim::clips(&h.model).unwrap();
    let cn: Vec<_> = clips.iter().map(|(c, _)| c.name.clone()).collect();
    eprintln!("host clips: {cn:?}");
    assert!(cn.contains(&"inst.walk".to_string()), "clips {cn:?}");
}

#[test]
fn posing_moves_the_instance() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup("pose");
    let (clip, skel) = modeler::anim::clip(&h.model, Some("inst.walk")).unwrap();
    let rest = h.model.part("inst.tube").unwrap().bounds();
    let posed = modeler::anim::posed_parts(&h.model, &clip, &skel, 0.5);
    let p = posed.iter().find(|p| p.name == "inst.tube").unwrap();
    let (lo, hi) = p.bounds();
    eprintln!("rest {rest:?} posed {:?}", (lo, hi));
    // +60 degrees about z swings the tip toward -x.
    assert!(
        (lo.x - rest.0.x).abs() > 0.1 || (hi.x - rest.1.x).abs() > 0.1,
        "top bone should swing the tip in x"
    );
    assert!(lo.x > 4.0, "still at the instance offset");
}

#[test]
fn export_has_a_skin_and_animation() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup("export");
    let dir =
        std::env::temp_dir().join(format!("friendslop-prefab-rig-out-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("host.gltf");
    modeler::gltf::export_gltf(&h.model, &path).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    eprintln!("skins {} animations {}", v["skins"], v["animations"]);
    assert!(
        v["skins"].as_array().is_some_and(|a| !a.is_empty()),
        "no skin"
    );
    assert!(
        v["animations"].as_array().is_some_and(|a| !a.is_empty()),
        "no animation"
    );
}

/// Posing an instance equals posing the prefab, then carrying the result
/// by the instance transform `x -> t + r * (s * x)`.
fn assert_instance_poses_like_prefab(h: &Session, inst: &str, t: Vec3, r: Quat, s: f32) {
    let src = modeler::catalog::prefab_instance("wiggler").unwrap();
    let (sc, sk) = modeler::anim::clip(&src, Some("walk")).unwrap();
    let want = modeler::anim::posed_parts(&src, &sc, &sk, 0.5);
    let want = want.iter().find(|p| p.name == "tube").unwrap();
    let (hc, hk) = modeler::anim::clip(&h.model, Some(&format!("{inst}.walk"))).unwrap();
    let got = modeler::anim::posed_parts(&h.model, &hc, &hk, 0.5);
    let got = got
        .iter()
        .find(|p| p.name == format!("{inst}.tube"))
        .unwrap();
    assert_eq!(want.verts.len(), got.verts.len());
    let mut worst = 0.0f32;
    for (a, b) in want.verts.iter().zip(&got.verts) {
        let a = t + r * (Vec3::from(*a) * s);
        worst = worst.max(a.distance(Vec3::from(*b)));
    }
    eprintln!("{inst}: worst vertex error {worst}");
    assert!(worst < 1e-3, "{inst}: posed vertices off by {worst}");
    // And the pose actually moved something.
    let rest = h.model.part(&format!("{inst}.tube")).unwrap();
    let moved = (0..rest.verts.len())
        .map(|i| rest.world_vert(i).distance(Vec3::from(got.verts[i])))
        .fold(0.0f32, f32::max);
    assert!(
        moved > 0.1,
        "{inst}: the clip should move the tube ({moved})"
    );
}

#[test]
fn rotated_scaled_instance_poses_like_the_prefab() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup_with(
        "rot",
        None,
        vec![json!({"type": "prefab", "name": "big", "prefab": "wiggler",
                    "translation": [1, 0, -2], "rotation": {"y": 90}, "scale": [2, 2, 2]})],
    );
    let r = Quat::from_rotation_y(90f32.to_radians());
    assert_instance_poses_like_prefab(&h, "big", Vec3::new(1.0, 0.0, -2.0), r, 2.0);
}

#[test]
fn rest_rotated_bone_poses_instanced() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup_with(
        "rest",
        Some(45.0),
        vec![
            json!({"type": "prefab", "name": "rolled", "prefab": "wiggler",
                    "translation": [0, 0, 3], "rotation": {"y": 90}, "scale": [2, 2, 2]}),
        ],
    );
    let skel = modeler::anim::skeletons(&h.model).unwrap();
    let s = skel.iter().find(|s| s.name == "rolled.rig").unwrap();
    assert!(s.has_rest(), "the rolled bone keeps a rest frame");
    let r = Quat::from_rotation_y(90f32.to_radians());
    assert_instance_poses_like_prefab(&h, "rolled", Vec3::new(0.0, 0.0, 3.0), r, 2.0);
}

#[test]
fn two_instances_pose_and_export_apart() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup_with(
        "two",
        None,
        vec![
            json!({"type": "prefab", "name": "a", "prefab": "wiggler", "translation": [-2, 0, 0]}),
            json!({"type": "prefab", "name": "b", "prefab": "wiggler", "translation": [2, 0, 0],
                   "rotation": {"y": 90}}),
        ],
    );
    assert_instance_poses_like_prefab(&h, "a", Vec3::new(-2.0, 0.0, 0.0), Quat::IDENTITY, 1.0);
    let r = Quat::from_rotation_y(90f32.to_radians());
    assert_instance_poses_like_prefab(&h, "b", Vec3::new(2.0, 0.0, 0.0), r, 1.0);
    let dir = std::env::temp_dir().join(format!(
        "friendslop-prefab-rig-two-out-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("host.gltf");
    modeler::gltf::export_gltf(&h.model, &path).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        v["skins"].as_array().map(Vec::len),
        Some(2),
        "one skin each"
    );
    let anims: Vec<&str> = v["animations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    eprintln!("animations {anims:?}");
    assert!(
        anims.contains(&"a.walk") && anims.contains(&"b.walk"),
        "{anims:?}"
    );
    // No rig lint for instance skeletons.
    let lints = modeler::anim::check::lints(&h.model);
    assert!(lints.is_empty(), "{lints:?}");
}

#[test]
fn editing_the_prefab_re_derives_the_instance_rig() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = setup("edit");
    let before = modeler::anim::clip(&h.model, Some("inst.walk"))
        .unwrap()
        .0
        .duration;
    // Re-save the prefab with a longer clip.
    let mut a = Session::default();
    a.model = (*modeler::catalog::prefab_instance("wiggler").unwrap()).clone();
    a.apply_values(
        None,
        vec![
            json!({"type": "clip", "name": "walk", "skeleton": "rig", "duration": 2.0,
                    "tracks": {"top": {"rotate": [[0, {"z": 0}], [1, {"z": 60}], [2, {"z": 0}]]}}}),
        ],
    )
    .unwrap();
    // A different file length/mtime: wait past the timestamp granularity.
    std::thread::sleep(std::time::Duration::from_millis(20));
    a.save_prefab_checked("rigged tube", &["test"], "", true)
        .unwrap();
    let after = modeler::anim::clip(&h.model, Some("inst.walk"))
        .unwrap()
        .0
        .duration;
    assert_eq!((before, after), (1.0, 2.0));
}
