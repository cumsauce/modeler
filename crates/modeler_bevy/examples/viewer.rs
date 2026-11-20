//! Looks at a modeler export in Bevy with the runtime plugin.
//!
//! ```sh
//! cargo run -p modeler_bevy --example viewer -- <assets dir> <model.gltf inside it>
//! ```
//!
//! Keys: Space plays the next animation clip with a 0.2 s crossfade (through
//! `ModelerAnimator`; root motion moves the model, clip events are printed,
//! and a baked sway clip pauses the live sway),
//! W toggles the live wind, Up / Down change its strength, Left / Right turn
//! it, V cycles the cosmetic variants (`KHR_materials_variants`, through
//! `ModelerVariant`). Drag with the mouse to orbit.
//!
//! `VIEWER_VARIANT=red_team` starts in that variant.
//!
//! `VIEWER_SHOT=out.png` saves a screenshot after a few seconds, prints how
//! many sway rigs and moving joints it found, and quits (a smoke test).

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use modeler_bevy::{
    ModelerAnimator, ModelerClipEvent, ModelerClips, ModelerPlugin, ModelerRootMotion,
    ModelerVariant, Wind,
};
use std::time::Duration;

#[derive(Resource)]
struct Model {
    gltf: Handle<Gltf>,
    spawned: bool,
    clip: Option<usize>,
    names: Vec<String>,
    /// The scene root (where `ModelerVariant` goes).
    root: Option<Entity>,
}

#[derive(Component)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    dist: f32,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(root), Some(file)) = (args.next(), args.next()) else {
        eprintln!("usage: viewer <assets dir> <model.gltf inside it>");
        return;
    };
    App::new()
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            file_path: root,
            ..default()
        }))
        .add_plugins(ModelerPlugin)
        .insert_resource(FileName(file))
        .add_systems(Startup, setup)
        .add_systems(Update, (spawn_scene, keys, orbit, smoke_test, print_events))
        .run();
}

#[derive(Resource)]
struct FileName(String);

fn setup(mut commands: Commands, assets: Res<AssetServer>, file: Res<FileName>) {
    commands.insert_resource(Model {
        gltf: assets.load(file.0.clone()),
        spawned: false,
        clip: None,
        names: Vec::new(),
        root: None,
    });
    commands.spawn((
        Camera3d::default(),
        // Forward decals read the depth prepass.
        bevy::core_pipeline::prepass::DepthPrepass,
        Transform::default(),
        Orbit {
            yaw: 0.6,
            pitch: 0.3,
            dist: 8.0,
        },
    ));
    commands.spawn((
        DirectionalLight {
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 8.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Text::new(
            "Space / 1-9: clip (crossfade)  V: next variant  W: wind on/off  Up/Down: strength  Left/Right: direction",
        ),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(12.0),
            bottom: Val::Px(12.0),
            ..default()
        },
    ));
}

fn spawn_scene(
    mut commands: Commands,
    mut model: ResMut<Model>,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
) {
    if model.spawned {
        return;
    }
    let Some(g) = gltfs.get(&model.gltf) else {
        return;
    };
    let Some(scene) = g
        .default_scene
        .clone()
        .or_else(|| g.scenes.first().cloned())
    else {
        return;
    };
    // `ModelerClips` gives every animation player the graph and transitions;
    // `ModelerRootMotion` moves the entity by the playing clip's travel.
    let clips = ModelerClips::from_gltf(g, &mut graphs);
    model.names = clips.names().iter().map(|n| n.to_string()).collect();
    println!("clips: {:?}", model.names);
    let root = commands
        .spawn((
            WorldAssetRoot(scene),
            clips,
            ModelerRootMotion::default(),
            Transform::default(),
            Visibility::default(),
        ))
        .id();
    model.root = Some(root);
    if let Ok(v) = std::env::var("VIEWER_VARIANT") {
        commands.entity(root).insert(ModelerVariant::new(v));
    }
    model.spawned = true;
}

fn print_events(mut events: MessageReader<ModelerClipEvent>) {
    for e in events.read() {
        println!(
            "event `{}` of `{}` at {:.2}s {:?}",
            e.name, e.clip, e.time, e.data
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn keys(
    input: Res<ButtonInput<KeyCode>>,
    mut model: ResMut<Model>,
    mut wind: ResMut<Wind>,
    mut anim: ModelerAnimator,
    mut commands: Commands,
    variant: Query<&ModelerVariant>,
    parts: Query<&modeler_bevy::ModelerVariants>,
) {
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    let pick = digits
        .iter()
        .position(|k| input.just_pressed(*k))
        .filter(|i| *i < model.names.len())
        .or_else(|| {
            (input.just_pressed(KeyCode::Space) && !model.names.is_empty())
                .then(|| model.clip.map_or(0, |c| (c + 1) % model.names.len()))
        });
    if let (Some(next), Some(root)) = (pick, model.root) {
        let name = model.names[next].clone();
        if anim.play(root, &name, Duration::from_millis(200)) {
            println!("playing `{name}` (crossfade 0.2 s)");
            model.clip = Some(next);
        }
    }
    if input.just_pressed(KeyCode::KeyV)
        && let Some(root) = model.root
    {
        // Every variant the parts know ("default" first).
        let mut names: Vec<String> = parts
            .iter()
            .flat_map(|p| p.0.keys().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|n| n != "default")
            .collect();
        names.insert(0, "default".into());
        let cur = variant
            .get(root)
            .map_or("default".to_string(), |v| v.name.clone());
        let i = names.iter().position(|v| *v == cur).unwrap_or(0);
        let next = names[(i + 1) % names.len()].clone();
        println!("variant `{next}` (of {names:?})");
        commands.entity(root).insert(ModelerVariant::new(next));
    }
    if input.just_pressed(KeyCode::KeyW) {
        wind.enabled = !wind.enabled;
        println!("wind {}", if wind.enabled { "on" } else { "off" });
    }
    let s = wind.strength.unwrap_or(0.5);
    if input.just_pressed(KeyCode::ArrowUp) {
        wind.strength = Some(s + 0.25);
    }
    if input.just_pressed(KeyCode::ArrowDown) {
        wind.strength = Some((s - 0.25).max(0.0));
    }
    let d = wind.direction.unwrap_or(Vec3::X);
    for (key, ang) in [(KeyCode::ArrowLeft, 0.4f32), (KeyCode::ArrowRight, -0.4)] {
        if input.just_pressed(key) {
            wind.direction = Some(Quat::from_rotation_y(ang) * d);
        }
    }
}

/// With `VIEWER_SHOT`: a screenshot at 3 s, a report, and exit.
#[allow(clippy::too_many_arguments)]
fn smoke_test(
    mut commands: Commands,
    time: Res<Time>,
    mut frame: Local<u32>,
    mut start: Local<Option<Vec<Quat>>>,
    sways: Query<&modeler_bevy::Sway>,
    decals: Query<&modeler_bevy::ModelerDecal>,
    joints: Query<&Transform>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(path) = std::env::var("VIEWER_SHOT") else {
        return;
    };
    let rotations = || -> Vec<Quat> {
        sways
            .iter()
            .flat_map(|s| s.joints.iter().flatten())
            .filter_map(|j| joints.get(*j).ok().map(|t| t.rotation))
            .collect()
    };
    let t = time.elapsed_secs();
    if t > 1.5 && start.is_none() {
        *start = Some(rotations());
    }
    if t > 3.0 {
        *frame += 1;
        if *frame == 1 {
            commands
                .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
                .observe(bevy::render::view::screenshot::save_to_disk(path));
        }
        if *frame == 20 {
            let now = rotations();
            let moved = start
                .as_ref()
                .map(|a| {
                    a.iter()
                        .zip(&now)
                        .filter(|(a, b)| a.angle_between(**b) > 1e-4)
                        .count()
                })
                .unwrap_or(0);
            println!(
                "smoke: {} decal(s), {} sway rig(s), {} joint(s), {moved} moved",
                decals.iter().count(),
                sways.iter().count(),
                now.len()
            );
            exit.write(AppExit::Success);
        }
    }
}

fn orbit(
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut cams: Query<(&mut Transform, &mut Orbit)>,
) {
    for (mut t, mut o) in &mut cams {
        if mouse.pressed(MouseButton::Left) {
            o.yaw -= motion.delta.x * 0.005;
            o.pitch = (o.pitch + motion.delta.y * 0.005).clamp(-1.4, 1.4);
        }
        let target = Vec3::new(0.0, o.dist * 0.25, 0.0);
        let dir = Vec3::new(
            o.yaw.sin() * o.pitch.cos(),
            o.pitch.sin(),
            o.yaw.cos() * o.pitch.cos(),
        );
        *t = Transform::from_translation(target + dir * o.dist).looking_at(target, Vec3::Y);
    }
}
