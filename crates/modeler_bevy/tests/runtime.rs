//! The plugin on a scene shaped like a glTF spawn: a skeleton root with
//! `extras.sway` and named joint children.

use bevy::gltf::GltfExtras;
use bevy::prelude::*;
use modeler_bevy::{ColliderShape, ModelerCollider, ModelerMarker, ModelerPlugin, Sway, Wind};

fn rig_json(kind: &str) -> String {
    let settings = if kind == "wind" {
        r#""kind": "wind", "strength": 1.0"#
    } else {
        r#""kind": "spring", "stiffness": 60, "damping": 3"#
    };
    format!(
        r#"{{"skeleton": "rig", "sway": {{"bones": [
            {{"name": "trunk_0", "head": [0, 0, 0], "tail": [0, 1, 0], "radius": 0.2}},
            {{"name": "trunk_1", "parent": 0, "head": [0, 1, 0], "tail": [0, 2, 0], "radius": 0.1}}],
            {settings}, "clip": "wind"}}}}"#
    )
}

fn scene(app: &mut App, kind: &str) -> (Entity, Entity, Entity) {
    let world = app.world_mut();
    let j0 = world
        .spawn((Name::new("trunk_0"), Transform::default()))
        .id();
    let j1 = world
        .spawn((Name::new("trunk_1"), Transform::from_xyz(0.0, 1.0, 0.0)))
        .id();
    world.entity_mut(j0).add_child(j1);
    let root = world
        .spawn((
            Name::new("rig"),
            Transform::default(),
            GltfExtras {
                value: rig_json(kind),
            },
        ))
        .id();
    world.entity_mut(root).add_child(j0);
    (root, j0, j1)
}

/// The same two-bone rig, with joint rest rotations (a frame turned about y,
/// as an exported rig with `rest` bones has).
fn scene_rest(app: &mut App, kind: &str) -> (Entity, Entity, Entity) {
    let (root, j0, j1) = scene(app, kind);
    let world = app.world_mut();
    if let Some(mut t) = world.get_mut::<Transform>(j0) {
        t.rotation = Quat::from_rotation_y(1.2);
    }
    if let Some(mut t) = world.get_mut::<Transform>(j1) {
        t.rotation = Quat::from_rotation_y(-0.5) * Quat::from_rotation_z(0.6);
    }
    (root, j0, j1)
}

fn rot(app: &App, e: Entity) -> Quat {
    app.world()
        .get::<Transform>(e)
        .map_or(Quat::IDENTITY, |t| t.rotation)
}

/// Joint 1's world rotation with its rest orientation taken out.
fn world_without_rest(app: &App, j0: Entity, j1: Entity, rest: &[Quat; 2]) -> Quat {
    ((rot(app, j0) * rot(app, j1)) * (rest[0] * rest[1]).inverse()).normalize()
}

/// Rotations apart by an angle (radians). `Quat::angle_between` is
/// `2 acos(dot)`, whose f32 noise near equality is about 1e-4 (a 3e-8
/// component difference reads as 1.4e-4), so measure the difference of the
/// (sign-aligned) components instead: the angle is about twice that.
fn apart(a: Quat, b: Quat) -> f32 {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    2.0 * (a - b).length()
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, TransformPlugin, ModelerPlugin));
    app
}

#[test]
fn wind_moves_the_joints_live() {
    let mut app = app();
    let (root, j0, j1) = scene(&mut app, "wind");
    for _ in 0..3 {
        app.update();
    }
    let sway = app.world().get::<Sway>(root);
    assert!(
        sway.is_some_and(|s| s.joints.iter().all(Option::is_some)),
        "Sway with both joints"
    );
    let r0 = app.world().get::<Transform>(j0).map(|t| t.rotation);
    let r1 = app.world().get::<Transform>(j1).map(|t| t.rotation);
    for r in [r0, r1] {
        let r = r.unwrap_or(Quat::IDENTITY);
        assert!(r.angle_between(Quat::IDENTITY) > 1e-3, "{r:?}");
    }
    // The wind doesn't pile up frame after frame: a calm frame returns
    // the joints to rest.
    app.world_mut().resource_mut::<Wind>().enabled = false;
    app.update();
    let r = app
        .world()
        .get::<Transform>(j1)
        .map(|t| t.rotation)
        .unwrap_or(Quat::IDENTITY);
    assert!(r.angle_between(Quat::IDENTITY) < 1e-5, "{r:?}");
}

#[test]
fn spring_follows_a_moved_root() {
    let mut app = app();
    let (_, j0, j1) = scene(&mut app, "spring");
    app.update();
    // The game (or an AnimationPlayer) jerks the root bone sideways.
    if let Some(mut t) = app.world_mut().get_mut::<Transform>(j0) {
        t.translation.x = 0.5;
    }
    let mut moved = 0.0f32;
    for _ in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.update();
        let r = app
            .world()
            .get::<Transform>(j1)
            .map(|t| t.rotation)
            .unwrap_or(Quat::IDENTITY);
        moved = moved.max(r.angle_between(Quat::IDENTITY));
    }
    assert!(moved > 1e-3, "the tip lags behind ({moved})");
}

#[test]
fn colliders_and_markers_from_extras() {
    let mut app = app();
    let a = app
        .world_mut()
        .spawn(GltfExtras {
            value: r#"{"collider": {"shape": "cuboid", "half_extents": [1, 0.5, 2], "body": "dynamic"}}"#
                .into(),
        })
        .id();
    let b = app
        .world_mut()
        .spawn(GltfExtras {
            value: r#"{"marker": "spawn", "data": {"team": 1}}"#.into(),
        })
        .id();
    app.update();
    let c = app.world().get::<ModelerCollider>(a).cloned();
    assert_eq!(
        c,
        Some(ModelerCollider {
            shape: ColliderShape::Cuboid {
                half_extents: Vec3::new(1.0, 0.5, 2.0)
            },
            body: "dynamic".into(),
            sensor: false,
        })
    );
    let m = app.world().get::<ModelerMarker>(b).cloned();
    assert_eq!(
        m.map(|m| (m.kind, m.data["team"].as_i64())),
        Some(("spawn".into(), Some(1)))
    );
}

fn rest_of_scene_rest() -> [Quat; 2] {
    [
        Quat::from_rotation_y(1.2),
        Quat::from_rotation_y(-0.5) * Quat::from_rotation_z(0.6),
    ]
}

#[test]
fn wind_leans_the_same_with_rest_rotations() {
    let mut app = app();
    let (plain, p0, p1) = scene(&mut app, "wind");
    let (rested, r0, r1) = scene_rest(&mut app, "wind");
    app.update();
    for root in [plain, rested] {
        if let Some(mut s) = app.world_mut().get_mut::<Sway>(root) {
            s.phase = 0.0;
        }
    }
    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.update();
        let a = world_without_rest(&app, p0, p1, &[Quat::IDENTITY; 2]);
        let b = world_without_rest(&app, r0, r1, &rest_of_scene_rest());
        assert!(a.angle_between(Quat::IDENTITY) > 1e-3, "leans {a:?}");
        assert!(apart(a, b) < 1e-4, "{a:?} vs {b:?}");
    }
}

#[test]
fn spring_follows_a_moved_root_with_rest_rotations() {
    let mut app = app();
    let (_, p0, p1) = scene(&mut app, "spring");
    let (_, r0, r1) = scene_rest(&mut app, "spring");
    app.update();
    for j in [p0, r0] {
        if let Some(mut t) = app.world_mut().get_mut::<Transform>(j) {
            t.translation.x = 0.5;
        }
    }
    let mut moved = 0.0f32;
    for _ in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.update();
        let a = world_without_rest(&app, p0, p1, &[Quat::IDENTITY; 2]);
        let b = world_without_rest(&app, r0, r1, &rest_of_scene_rest());
        moved = moved.max(a.angle_between(Quat::IDENTITY));
        assert!(apart(a, b) < 1e-3, "{a:?} vs {b:?}");
    }
    assert!(moved > 1e-3, "the tip lags behind ({moved})");
}
