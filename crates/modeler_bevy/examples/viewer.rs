//! Looks at a modeler export in Bevy with the runtime plugin.
//!
//! ```sh
//! cargo run -p modeler_bevy --example viewer -- <assets dir> <model.gltf inside it>
//! ```
//!
//! Keys: Space plays the next animation clip (and pauses the live sway),
//! W toggles the live wind, Up / Down change its strength, Left / Right turn
//! it. Drag with the mouse to orbit.
//!
//! `VIEWER_SHOT=out.png` saves a screenshot after a few seconds, prints how
//! many sway rigs and moving joints it found, and quits (a smoke test).

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use modeler_bevy::{ModelerPlugin, Sway, Wind};

#[derive(Resource)]
struct Model {
    gltf: Handle<Gltf>,
    spawned: bool,
    clip: Option<usize>,
    graph: Option<(Handle<AnimationGraph>, Vec<AnimationNodeIndex>, Vec<String>)>,
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
        .add_systems(Update, (spawn_scene, hook_player, keys, orbit, smoke_test))
        .run();
}

#[derive(Resource)]
struct FileName(String);

fn setup(mut commands: Commands, assets: Res<AssetServer>, file: Res<FileName>) {
    commands.insert_resource(Model {
        gltf: assets.load(file.0.clone()),
        spawned: false,
        clip: None,
        graph: None,
    });
    commands.spawn((
        Camera3d::default(),
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
        Text::new("Space: next clip  W: wind on/off  Up/Down: strength  Left/Right: direction"),
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
    commands.spawn(WorldAssetRoot(scene));
    let mut names: Vec<String> = g.named_animations.keys().map(|k| k.to_string()).collect();
    names.sort();
    let clips: Vec<Handle<AnimationClip>> = names
        .iter()
        .filter_map(|n| g.named_animations.get(n.as_str()).cloned())
        .collect();
    let (graph, nodes) = AnimationGraph::from_clips(clips);
    println!("clips: {names:?}");
    model.graph = Some((graphs.add(graph), nodes, names));
    model.spawned = true;
}

fn hook_player(
    mut commands: Commands,
    model: Res<Model>,
    players: Query<Entity, Added<AnimationPlayer>>,
) {
    let Some((graph, _, _)) = &model.graph else {
        return;
    };
    for e in &players {
        commands
            .entity(e)
            .insert(AnimationGraphHandle(graph.clone()));
    }
}

fn keys(
    input: Res<ButtonInput<KeyCode>>,
    mut model: ResMut<Model>,
    mut wind: ResMut<Wind>,
    mut players: Query<&mut AnimationPlayer>,
    mut sways: Query<&mut Sway>,
) {
    if input.just_pressed(KeyCode::Space)
        && let Some((_, nodes, names)) = &model.graph
        && !nodes.is_empty()
    {
        let next = model.clip.map_or(0, |c| (c + 1) % nodes.len());
        for mut p in &mut players {
            p.stop_all();
            p.play(nodes[next]).repeat();
        }
        // The baked sway clip replaces the live sway while it plays.
        for mut s in &mut sways {
            s.live = s.rig.clip.as_deref() != Some(names[next].as_str());
        }
        println!("playing `{}`", names[next]);
        model.clip = Some(next);
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
fn smoke_test(
    mut commands: Commands,
    time: Res<Time>,
    mut frame: Local<u32>,
    mut start: Local<Option<Vec<Quat>>>,
    sways: Query<&Sway>,
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
                "smoke: {} sway rig(s), {} joint(s), {moved} moved",
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
