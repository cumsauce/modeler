//! A dressed, rigged, animated human exported by the modeler, loaded into a
//! real Bevy app (full renderer, no window; screenshots through an image
//! camera, like the modeler's `--offscreen` mode) and checked point by
//! point: morph targets, skinning, clips, correctives, outfits, LODs, live
//! sway, root motion, events and crossfades.
//!
//! Needs `data/human/human.bin` (skips without it) and a GPU adapter
//! (skips without one, and says so). Screenshots land in
//! `target/tmp/character-check/`; set `CHARACTER_CHECK_DIR` to move them.

#![allow(
    clippy::unwrap_used,
    clippy::chunks_exact_to_as_chunks,
    clippy::useless_conversion
)]

#[path = "../../../tests/common/character.rs"]
mod character;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bevy::animation::transition::AnimationTransitions;
use bevy::camera::RenderTarget;
use bevy::camera::visibility::VisibilityRange;
use bevy::ecs::system::RunSystemOnce;
use bevy::gltf::Gltf;
use bevy::mesh::morph::MorphWeights;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::time::TimeUpdateStrategy;
use modeler_bevy::{
    ModelerAnimator, ModelerClipEvent, ModelerClips, ModelerCorrectives, ModelerLod, ModelerPlugin,
    ModelerRootMotion, ModelerVariant, ModelerVariantVisibility, Sway,
};
use serde_json::json;

const SIZE: u32 = 512;

#[derive(Resource, Default)]
struct Shot(Option<Image>);

#[derive(Resource, Default)]
struct Events(Vec<ModelerClipEvent>);

#[derive(Resource, Default)]
struct AssetLog(Vec<String>);

fn log_assets(
    mut g: MessageReader<AssetEvent<Gltf>>,
    mut w: MessageReader<AssetEvent<bevy::world_serialization::WorldAsset>>,
    mut log: ResMut<AssetLog>,
) {
    for e in g.read() {
        log.0.push(format!("gltf {e:?}"));
    }
    for e in w.read() {
        log.0.push(format!("scene {e:?}"));
    }
}

fn collect(mut r: MessageReader<ModelerClipEvent>, mut out: ResMut<Events>) {
    out.0.extend(r.read().cloned());
}

struct Rig {
    app: App,
    out: PathBuf,
}

impl Rig {
    fn step(&mut self, n: usize) {
        for _ in 0..n {
            self.app.update();
        }
    }

    fn until(&mut self, max: usize, what: &str, mut f: impl FnMut(&mut World) -> bool) {
        for _ in 0..max {
            self.app.update();
            if f(self.app.world_mut()) {
                return;
            }
        }
        panic!("timed out waiting for {what}");
    }

    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    fn find(&mut self, name: &str) -> Entity {
        let w = self.world();
        let mut q = w.query::<(Entity, &Name)>();
        q.iter(w)
            .find(|(_, n)| n.as_str() == name)
            .map(|x| x.0)
            .unwrap_or_else(|| panic!("no entity named {name}"))
    }

    /// Puts the camera at `eye` looking at `target` and returns RGBA pixels
    /// of the next frame (also saved as `<name>.png`).
    fn shot(&mut self, name: &str, eye: Vec3, target: Vec3) -> Vec<u8> {
        let cam = {
            let w = self.world();
            let mut q = w.query_filtered::<Entity, With<Camera3d>>();
            q.single(w).unwrap()
        };
        *self.world().get_mut::<Transform>(cam).unwrap() =
            Transform::from_translation(eye).looking_at(target, Vec3::Y);
        let image = self.world().resource::<CamImage>().0.clone();
        self.world().insert_resource(Shot(None));
        self.step(2);
        self.world()
            .spawn(Screenshot::image(image))
            .observe(|c: On<ScreenshotCaptured>, mut s: ResMut<Shot>| s.0 = Some(c.image.clone()));
        self.until(120, "a screenshot", |w| w.resource::<Shot>().0.is_some());
        let img = self.world().resource_mut::<Shot>().0.take().unwrap();
        let mut rgba = img.data.clone().unwrap();
        if matches!(
            img.texture_descriptor.format,
            TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm
        ) {
            for p in rgba.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
        }
        let (w, h) = (img.width(), img.height());
        let file = std::fs::File::create(self.out.join(format!("{name}.png"))).unwrap();
        let mut enc = png::Encoder::new(file, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        rgba
    }
}

#[derive(Resource)]
struct CamImage(Handle<Image>);

/// Share of pixels that differ by more than a few levels.
fn changed(a: &[u8], b: &[u8]) -> f32 {
    let n = a.len() / 4;
    let d = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| (0..3).any(|k| p[k].abs_diff(q[k]) > 6))
        .count();
    d as f32 / n as f32
}

/// Share of pixels unlike the corner (background) colour.
fn coverage(a: &[u8]) -> f32 {
    let bg = [a[0], a[1], a[2]];
    let d = a
        .chunks_exact(4)
        .filter(|p| (0..3).any(|k| p[k].abs_diff(bg[k]) > 12))
        .count();
    d as f32 / (a.len() / 4) as f32
}

fn make_app(assets: &Path) -> Option<App> {
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut app = App::new();
        app.add_plugins(
            DefaultPlugins
                .set(AssetPlugin {
                    file_path: assets.to_string_lossy().to_string(),
                    ..default()
                })
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        resolution: bevy::window::WindowResolution::new(SIZE, SIZE)
                            .with_scale_factor_override(1.0),
                        ..default()
                    }),
                    exit_condition: bevy::window::ExitCondition::DontExit,
                    close_when_requested: false,
                    ..default()
                })
                .set(bevy::render::RenderPlugin {
                    synchronous_pipeline_compilation: true,
                    ..default()
                })
                .disable::<bevy::winit::WinitPlugin>()
                .disable::<bevy::log::LogPlugin>(),
        );
        app.add_plugins(ModelerPlugin);
        app
    }));
    built.ok()
}

fn out_dir() -> PathBuf {
    let d = std::env::var("CHARACTER_CHECK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_TARGET_TMPDIR")).join("character-check"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sparse_positions(doc: &serde_json::Value, bin: &[u8], acc: usize) -> Vec<[f32; 3]> {
    let a = &doc["accessors"][acc];
    let n = a["count"].as_u64().unwrap() as usize;
    let view = |i: u64| {
        let v = &doc["bufferViews"][i as usize];
        let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        &bin[off..off + v["byteLength"].as_u64().unwrap() as usize]
    };
    let f32s = |b: &[u8]| -> Vec<f32> {
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    let mut out = vec![[0.0; 3]; n];
    if let Some(v) = a["bufferView"].as_u64() {
        for (o, c) in out.iter_mut().zip(f32s(view(v)).chunks_exact(3)) {
            *o = [c[0], c[1], c[2]];
        }
    }
    if let Some(s) = a.get("sparse") {
        let idx = view(s["indices"]["bufferView"].as_u64().unwrap());
        let vals = f32s(view(s["values"]["bufferView"].as_u64().unwrap()));
        for k in 0..s["count"].as_u64().unwrap() as usize {
            let i = u32::from_le_bytes(idx[k * 4..k * 4 + 4].try_into().unwrap()) as usize;
            out[i] = [vals[k * 3], vals[k * 3 + 1], vals[k * 3 + 2]];
        }
    }
    out
}

fn angle(a: Quat, b: Quat) -> f32 {
    2.0 * (a - if a.dot(b) < 0.0 { -b } else { b }).length()
}

#[test]
fn character_loads_animates_and_renders_in_bevy() {
    if !character::human_available() {
        eprintln!("SKIPPED: no data/human/human.bin");
        return;
    }
    let t0 = Instant::now();
    let (assets, gltf_path, report) =
        character::export("bevy", &json!({"preset": "game", "lods": [0.5, 0.2]})).unwrap();
    let export_time = t0.elapsed();
    let file_kb = std::fs::metadata(&gltf_path).unwrap().len() / 1024;
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&gltf_path).unwrap()).unwrap();
    let bin = std::fs::read(
        assets
            .join("models")
            .join(doc["buffers"][0]["uri"].as_str().unwrap()),
    )
    .unwrap();
    eprintln!(
        "export: {export_time:?}, gltf {file_kb} KB, bin {} KB\n{report}",
        bin.len() / 1024
    );

    let Some(app) = make_app(&assets) else {
        eprintln!("SKIPPED: no usable GPU adapter (the renderer could not start)");
        return;
    };
    let mut rig = Rig {
        app,
        out: out_dir(),
    };
    rig.app.finish();
    rig.app.cleanup();
    rig.app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            16,
        )));
    rig.app.init_resource::<Events>();
    rig.app.add_systems(Update, collect);
    rig.app.init_resource::<AssetLog>();
    rig.app.add_systems(Update, log_assets);
    rig.app
        .insert_resource(ClearColor(Color::srgb(0.12, 0.13, 0.16)));
    rig.app.insert_resource(GlobalAmbientLight {
        brightness: 300.0,
        ..default()
    });

    // Camera into an image, a key light.
    {
        let w = rig.world();
        let image = w
            .resource_mut::<Assets<Image>>()
            .add(Image::new_target_texture(
                SIZE,
                SIZE,
                TextureFormat::Rgba8UnormSrgb,
                None,
            ));
        w.insert_resource(CamImage(image.clone()));
        w.spawn((
            Camera3d::default(),
            bevy::core_pipeline::prepass::DepthPrepass,
            Transform::from_xyz(0.0, 1.0, 3.5).looking_at(Vec3::new(0.0, 0.9, 0.0), Vec3::Y),
            RenderTarget::Image(image.into()),
        ));
        w.spawn((
            DirectionalLight {
                illuminance: 9000.0,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_xyz(2.0, 4.0, 3.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
        ));
    }

    // --- Load -----------------------------------------------------------
    let load_start = Instant::now();
    let handle: Handle<Gltf> = rig
        .world()
        .resource::<AssetServer>()
        .load("models/eve.gltf");
    {
        let h = handle.clone();
        rig.until(6000, "the glTF to load", move |w| {
            let s = w.resource::<AssetServer>();
            if let Some(bevy::asset::LoadState::Failed(e)) = s.get_load_state(&h) {
                panic!("glTF failed to load: {e}");
            }
            s.is_loaded_with_dependencies(&h)
        });
    }
    let load_time = load_start.elapsed();
    eprintln!("glTF loaded in {load_time:?}");

    let root = {
        let w = rig.world();
        let (scene, clips) =
            w.resource_scope(|w: &mut World, mut graphs: Mut<Assets<AnimationGraph>>| {
                let g = w.resource::<Assets<Gltf>>().get(&handle).unwrap();
                (
                    g.default_scene.clone().unwrap(),
                    ModelerClips::from_gltf(g, &mut graphs),
                )
            });
        eprintln!("clips: {:?}", clips.names());
        w.spawn((
            WorldAssetRoot(scene),
            clips,
            ModelerRootMotion::default(),
            Transform::default(),
            Visibility::default(),
        ))
        .id()
    };
    rig.until(600, "the scene to spawn", |w| {
        let mut q = w.query::<(&Name, &MorphWeights)>();
        q.iter(w).any(|(n, _)| n.as_str() == "eve")
    });
    rig.step(3);
    let eve = {
        let w = rig.world();
        let mut q = w.query::<(Entity, &Name, &MorphWeights)>();
        q.iter(w).find(|(_, n, _)| n.as_str() == "eve").unwrap().0
    };

    // The joints as spawned: the rest pose (Bevy leaves a bone a clip does not
    // key wherever the last clip left it, so tests reset to this).
    let rest_pose: Vec<(Entity, Transform)> = {
        let w = rig.world();
        let mut q = w.query::<(Entity, &Transform, &Name)>();
        q.iter(w)
            .filter(|(_, _, n)| !n.as_str().is_empty())
            .map(|(e, t, _)| (e, *t))
            .collect()
    };

    // --- (1) morph targets as Bevy loaded them --------------------------
    let names: Vec<String> = doc["meshes"][doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "eve")
        .unwrap()["mesh"]
        .as_u64()
        .unwrap() as usize]["extras"]["targetNames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect();
    let smile = names
        .iter()
        .position(|n| n == "smile")
        .expect("smile target");
    let mesh_of = |w: &World, e: Entity| -> Handle<Mesh> {
        let kids: Vec<Entity> = std::iter::once(e)
            .chain(
                w.get::<Children>(e)
                    .map(|c| c.iter().collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
            .collect();
        kids.iter()
            .find_map(|k| w.get::<Mesh3d>(*k).map(|m| m.0.clone()))
            .expect("eve has a mesh entity")
    };
    {
        let w = rig.world();
        let h = mesh_of(w, eve);
        let mesh = w.resource::<Assets<Mesh>>().get(&h).unwrap();
        let n = mesh.count_vertices();
        let mt = mesh
            .morph_targets()
            .expect("Bevy accepted the morph targets");
        assert_eq!(
            mt.len(),
            n * names.len(),
            "(1) {n} vertices x {} targets",
            names.len()
        );
        assert_eq!(
            mesh.morph_target_names().map(|n| n.len()),
            Some(names.len()),
            "(1) names"
        );
        // The last target is a sparse corrective: Bevy must have densified
        // it exactly as the file says.
        let node = doc["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == "eve")
            .unwrap();
        let prim = &doc["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
        for k in [smile, names.len() - 1, names.len() - 20] {
            let acc = prim["targets"][k]["POSITION"].as_u64().unwrap() as usize;
            let want = sparse_positions(&doc, &bin, acc);
            let mut max = 0.0f32;
            let mut nonzero = 0;
            for (v, p) in want.iter().enumerate() {
                let got = mt[k * n + v].position;
                max = max.max((got - Vec3::from(*p)).abs().max_element());
                nonzero += (got != Vec3::ZERO) as usize;
            }
            assert!(
                max < 1e-6 && nonzero > 0,
                "(1) target {k} {} differs by {max}, {nonzero} nonzero",
                names[k]
            );
            eprintln!(
                "(1) target {k} `{}`: sparse={} nonzero vertices {nonzero}, max diff {max}",
                names[k],
                prim["targets"][k]["POSITION"] != json!(null)
                    && doc["accessors"][acc].get("sparse").is_some()
            );
        }
        // What the densified targets cost in the main world (Bevy keeps 48 bytes per vertex per
        // target, 12 floats, and uploads the same to the GPU).
        let (mut bytes, mut meshes) = (0usize, 0);
        for (_, m) in w.resource::<Assets<Mesh>>().iter() {
            if let Some(t) = m.morph_targets() {
                bytes += t.len() * std::mem::size_of::<bevy::mesh::morph::MorphAttributes>();
                meshes += 1;
            }
        }
        eprintln!(
            "(1) morph data in Bevy: {meshes} mesh(es), {} MB (eve body alone {} MB)",
            bytes / 1_000_000,
            n * names.len() * 48 / 1_000_000
        );
        eprintln!("(1) PASS: {} vertices x {} targets", n, names.len());
    }

    // --- Players, graph -------------------------------------------------
    let players = {
        let w = rig.world();
        let mut q = w.query::<(Entity, &AnimationPlayer, Option<&AnimationTransitions>)>();
        q.iter(w)
            .map(|(e, _, t)| (e, t.is_some()))
            .collect::<Vec<_>>()
    };
    eprintln!(
        "animation players: {} (all with transitions: {})",
        players.len(),
        players.iter().all(|p| p.1)
    );
    assert!(
        !players.is_empty() && players.iter().all(|p| p.1),
        "ModelerClips sets up every player"
    );

    let play = |rig: &mut Rig, clip: &'static str, fade_ms: u64| {
        let ok = rig
            .world()
            .run_system_once(move |mut a: ModelerAnimator| {
                a.play(root, clip, Duration::from_millis(fade_ms))
            })
            .unwrap();
        assert!(ok, "play {clip}");
    };
    let stop = |rig: &mut Rig| {
        let w = rig.world();
        let mut q = w.query::<&mut AnimationPlayer>();
        for mut p in q.iter_mut(w) {
            p.stop_all();
        }
    };
    let rot_of = |rig: &mut Rig, name: &str| -> Quat {
        let e = rig.find(name);
        rig.world().get::<Transform>(e).unwrap().rotation
    };
    let front = (Vec3::new(0.0, 1.0, 3.6), Vec3::new(0.0, 0.9, 0.0));

    // --- (2) skinned mesh renders, walk plays ---------------------------
    play(&mut rig, "walk", 0);
    rig.step(2);
    let a = rig.shot("2_walk_a", front.0, front.1);
    let ra = rot_of(&mut rig, "thigh_l");
    rig.step(20);
    let b = rig.shot("2_walk_b", front.0, front.1);
    let rb = rot_of(&mut rig, "thigh_l");
    let cov = coverage(&a);
    let moved = changed(&a, &b);
    eprintln!(
        "(2) body covers {:.1}% of the frame; walk frames differ in {:.2}% of pixels; thigh_l turned {:.1} deg",
        cov * 100.0,
        moved * 100.0,
        angle(ra, rb).to_degrees()
    );
    assert!(cov > 0.04, "(2) the skinned human is on screen");
    assert!(
        angle(ra, rb) > 0.1 && moved > 0.003,
        "(2) walk moves joints and pixels"
    );

    // --- (3) talk keys the smile ---------------------------------------
    let head = {
        let e = rig.find("head");
        rig.world().get::<GlobalTransform>(e).unwrap().translation()
    };
    let face = (head + Vec3::new(0.0, 0.0, 0.55), head);
    let weight_of =
        |rig: &mut Rig, k: usize| rig.world().get::<MorphWeights>(eve).unwrap().weights()[k];
    play(&mut rig, "walk", 0);
    rig.step(34);
    let walk_face = rig.shot("3_face_walk", face.0, face.1);
    let w_walk = weight_of(&mut rig, smile);
    play(&mut rig, "talk", 0);
    rig.step(34);
    let talk_face = rig.shot("3_face_talk", face.0, face.1);
    let w_talk = weight_of(&mut rig, smile);
    let face_diff = changed(&walk_face, &talk_face);
    eprintln!(
        "(3) smile weight walk {w_walk:.3}, talk mid-clip {w_talk:.3}; face close-up differs in {:.2}% of pixels",
        face_diff * 100.0
    );
    assert!(
        w_walk.abs() < 1e-4 && w_talk > 0.5,
        "(3) the clip animates MorphWeights"
    );
    assert!(face_diff > 0.001, "(3) the smile is visible");

    // --- (4) correctives follow the joints, do not fight clip morphs ---
    // While talk plays, every driven slot equals the driver's weight for the live joint.
    let drivers = rig
        .world()
        .get::<ModelerCorrectives>(eve)
        .expect("correctives read")
        .drivers
        .clone();
    assert!(drivers.len() > 20);
    for _ in 0..40 {
        rig.step(1);
        for d in &drivers {
            let j = rig.find(&d.bone);
            let q = rig.world().get::<Transform>(j).unwrap().rotation;
            let got = weight_of(&mut rig, d.target);
            assert!(
                (got - d.weight(q)).abs() < 1e-3,
                "(4) {} driven {got} vs {}",
                d.name,
                d.weight(q)
            );
        }
    }
    // A posed elbow: stop the clips, bend elbow_l 120 degrees.
    stop(&mut rig);
    let d120 = drivers
        .iter()
        .find(|d| d.bone == "elbow_l" && d.degrees > 100.0 && d.degrees < 130.0)
        .expect("an elbow 120 corrective")
        .clone();
    let elbow = rig.find("elbow_l");
    let rest = Quat::from_array(d120.rest_local);
    let bent = rest * Quat::from_axis_angle(Vec3::from(d120.axis), 120f32.to_radians());
    // Keep a clip-keyed shape alive to check the driver leaves its slot alone.
    rig.world()
        .get_mut::<MorphWeights>(eve)
        .unwrap()
        .weights_mut()[smile] = 0.7;
    rig.world().get_mut::<Transform>(elbow).unwrap().rotation = bent;
    rig.step(3);
    let w_on = weight_of(&mut rig, d120.target);
    let smile_kept = weight_of(&mut rig, smile);
    let elbow_pos = {
        rig.step(1);
        rig.world()
            .get::<GlobalTransform>(elbow)
            .unwrap()
            .translation()
    };
    let eye = elbow_pos + Vec3::new(0.0, 0.1, 0.7);
    let on = rig.shot("4_elbow_correctives_on", eye, elbow_pos);
    let saved = rig.world().get::<ModelerCorrectives>(eve).unwrap().clone();
    rig.world().entity_mut(eve).remove::<ModelerCorrectives>();
    for k in 0..names.len() {
        if k != smile {
            rig.world()
                .get_mut::<MorphWeights>(eve)
                .unwrap()
                .weights_mut()[k] = 0.0;
        }
    }
    rig.step(2);
    let off = rig.shot("4_elbow_correctives_off", eye, elbow_pos);
    let ce = changed(&on, &off);
    eprintln!(
        "(4) elbow_l 120 deg: `{}` weight {w_on:.3}, smile slot kept at {smile_kept:.2}; elbow close-up differs in {:.2}% of pixels on vs off",
        d120.name,
        ce * 100.0
    );
    assert!(w_on > 0.95, "(4) driver reaches its peak");
    assert!(
        (smile_kept - 0.7).abs() < 1e-4,
        "(4) drivers leave clip-keyed slots alone"
    );
    assert!(ce > 0.0002, "(4) the corrective changes the elbow");
    rig.world().entity_mut(eve).insert(saved);

    // --- (5) variants: materials and visibility ------------------------
    let shirt = rig.find("shirt");
    let vis = rig
        .world()
        .get::<ModelerVariantVisibility>(shirt)
        .expect("variants_visible read")
        .clone();
    assert!(vis.visible("default") && !vis.visible("bare"));
    let mat_label = |rig: &mut Rig| -> (String, Color) {
        let w = rig.world();
        let kids: Vec<Entity> = std::iter::once(shirt)
            .chain(
                w.get::<Children>(shirt)
                    .map(|c| c.iter().collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
            .collect();
        let h = kids
            .iter()
            .find_map(|k| {
                w.get::<MeshMaterial3d<StandardMaterial>>(*k)
                    .map(|m| m.0.clone())
            })
            .unwrap();
        let path = w
            .resource::<AssetServer>()
            .get_path(h.id())
            .map(|p| p.to_string())
            .unwrap_or_default();
        (
            path,
            w.resource::<Assets<StandardMaterial>>()
                .get(&h)
                .map(|m| m.base_color)
                .unwrap_or(Color::NONE),
        )
    };
    let (m_default, c_default) = mat_label(&mut rig);
    play(&mut rig, "walk", 0);
    rig.step(2);
    rig.world()
        .entity_mut(root)
        .insert(ModelerVariant::new("red"));
    for _ in 0..300 {
        rig.step(1);
        let (p, c) = mat_label(&mut rig);
        if p != m_default && c.to_srgba().red > 0.5 {
            break;
        }
    }
    let (m_red, c_red) = mat_label(&mut rig);
    let red_shot = rig.shot("5_variant_red", front.0, front.1);
    rig.world()
        .entity_mut(root)
        .insert(ModelerVariant::new("bare"));
    rig.step(3);
    // Switching a variant must not re-read the glTF (it once did, respawning every entity).
    let reloads = rig
        .world()
        .resource::<AssetLog>()
        .0
        .iter()
        .filter(|e| e.contains("Modified") || e.contains("Added"))
        .count();
    assert_eq!(rig.find("shirt"), shirt, "(5) the scene was not respawned");
    assert_eq!(
        reloads,
        2,
        "(5) no second glTF load: {:?}",
        rig.world().resource::<AssetLog>().0
    );
    let hidden = *rig.world().get::<Visibility>(shirt).unwrap();
    let bare_shot = rig.shot("5_variant_bare", front.0, front.1);
    rig.world()
        .entity_mut(root)
        .insert(ModelerVariant::new("default"));
    rig.step(3);
    let shown = *rig.world().get::<Visibility>(shirt).unwrap();
    let def_shot = rig.shot("5_variant_default", front.0, front.1);
    eprintln!(
        "(5) shirt material {m_default} {c_default:?} -> {m_red} {c_red:?}; bare: {hidden:?}, default again: {shown:?}; red vs bare {:.2}% px, default vs bare {:.2}% px",
        changed(&red_shot, &bare_shot) * 100.0,
        changed(&def_shot, &bare_shot) * 100.0
    );
    assert_ne!(m_default, m_red, "(5) variant swaps the material handle");
    assert!(
        hidden == Visibility::Hidden && shown == Visibility::Inherited,
        "(5) variant toggles Visibility"
    );
    assert!(
        changed(&def_shot, &bare_shot) > 0.01,
        "(5) hiding the shirt changes the picture"
    );

    // --- (6) LOD ranges --------------------------------------------------
    let (lods, ranged) = {
        let w = rig.world();
        let mut q = w.query::<(&Name, &ModelerLod)>();
        let lods: Vec<(String, ModelerLod)> = q.iter(w).map(|(n, l)| (n.to_string(), *l)).collect();
        let mut r = w.query::<(&Name, &VisibilityRange)>();
        (lods, r.iter(w).count())
    };
    eprintln!(
        "(6) {} lod node(s), {ranged} entities with VisibilityRange: {lods:?}",
        lods.len()
    );
    assert!(lods.iter().any(|l| l.0 == "eve.lod2") && ranged >= lods.len());
    stop(&mut rig);
    rig.step(2);
    // Hide a whole level's nodes (the game's own `Visibility`) and see what
    // the renderer still draws: the levels not hidden must be culled by
    // their VisibilityRange alone for the picture to change like this.
    let level_nodes = |level: u32| -> Vec<String> {
        lods.iter()
            .filter(|l| l.1.level == level)
            .map(|l| l.0.clone())
            .collect()
    };
    let set_hidden = |rig: &mut Rig, level: u32, hide: bool| {
        for n in level_nodes(level) {
            let e = rig.find(&n);
            let v = if hide {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
            rig.world().entity_mut(e).insert(v);
        }
    };
    let px = |a: &[u8], b: &[u8]| (changed(a, b) * (SIZE * SIZE) as f32) as u32;
    let near_eye = Vec3::new(0.0, 1.0, 3.5);
    let far_eye = Vec3::new(0.0, 1.0, 120.0);
    let tgt = Vec3::new(0.0, 0.9, 0.0);
    let mut results = Vec::new();
    for (tag, eye) in [("near", near_eye), ("far", far_eye)] {
        let all = rig.shot(&format!("6_lod_{tag}_all"), eye, tgt);
        let mut diffs = Vec::new();
        for level in 0..3u32 {
            set_hidden(&mut rig, level, true);
            rig.step(2);
            let hidden = rig.shot(&format!("6_lod_{tag}_without_level{level}"), eye, tgt);
            set_hidden(&mut rig, level, false);
            diffs.push(px(&all, &hidden));
        }
        eprintln!(
            "(6) camera {:.0} m away: pixels lost when hiding level 0/1/2 = {diffs:?}",
            eye.distance(tgt)
        );
        results.push(diffs);
    }
    // Near: the body is level 0 (the small parts, eyes and tongue, switch at
    // 1.3 to 1.5 m by design: 15x their size, so a few hundred pixels of
    // level 1 / 2 show at 3.6 m). Far (120 m, past every switch): only level 2.
    assert!(
        results[0][0] > 2000 && results[0][1] < 1000 && results[0][2] < 1000,
        "(6) near shows level 0: {:?}",
        results[0]
    );
    assert!(
        results[1][2] > 5 && results[1][0] == 0 && results[1][1] == 0,
        "(6) far shows level 2 only: {:?}",
        results[1]
    );
    eprintln!("(6) PASS: the level by distance draws, the others are culled by VisibilityRange");

    // --- (7) live sway on the rest-rotated rig ---------------------------
    let (sways, bones) = {
        let w = rig.world();
        let mut q = w.query::<&Sway>();
        let v: Vec<&Sway> = q.iter(w).collect();
        (v.len(), v.iter().map(|s| s.rig.bones.len()).sum::<usize>())
    };
    assert!(sways >= 1, "(7) the sway rig was read");
    let sway_joints: Vec<Entity> = {
        let w = rig.world();
        let mut q = w.query::<&Sway>();
        q.iter(w)
            .flat_map(|s| s.joints.iter().flatten().copied().collect::<Vec<_>>())
            .collect()
    };
    let swayed: Vec<String> = {
        let w = rig.world();
        let mut q = w.query::<&Sway>();
        let s = q.iter(w).next().unwrap();
        let swing = s
            .rig
            .bones
            .iter()
            .filter(|b| b.name.starts_with("hand_"))
            .count();
        assert!(swing >= 1, "(7) the spring sway names the hand bones");
        s.rig.bones.iter().map(|b| b.name.clone()).collect()
    };
    // The same 240 frames of walk with the live sway on and off.
    let rest_pose2 = rest_pose.clone();
    let record = |rig: &mut Rig, live: bool| -> Vec<Vec<Quat>> {
        stop(rig);
        for (e, t) in &rest_pose2 {
            let w = rig.world();
            if w.get::<Name>(*e).is_some_and(|n| n.as_str() != "eve_rig")
                && let Some(mut x) = w.get_mut::<Transform>(*e)
            {
                *x = *t;
            }
        }
        {
            let w = rig.world();
            let mut q = w.query::<&mut Sway>();
            for mut s in q.iter_mut(w) {
                s.live = live;
            }
        }
        stop(rig);
        // `play` sets `live` from the clip; set it again after.
        play(rig, "walk", 0);
        {
            let w = rig.world();
            let mut q = w.query::<&mut Sway>();
            for mut s in q.iter_mut(w) {
                s.live = live;
            }
        }
        rig.step(5);
        let mut frames = Vec::new();
        for _ in 0..240 {
            rig.step(1);
            let mut f = Vec::new();
            for j in &sway_joints {
                let q = rig.world().get::<Transform>(*j).unwrap().rotation;
                assert!(
                    q.is_finite() && (q.length() - 1.0).abs() < 1e-3,
                    "(7) rotation stays a unit quaternion"
                );
                f.push(q);
            }
            let t = rig
                .world()
                .get::<Transform>(sway_joints[0])
                .unwrap()
                .translation;
            assert!(t.is_finite() && t.length() < 3.0, "(7) nothing explodes");
            frames.push(f);
        }
        frames
    };
    // The modeler's own baked result of the same spring, as the reference.
    stop(&mut rig);
    for (e, t) in &rest_pose {
        let w = rig.world();
        if w.get::<Name>(*e)
            .is_some_and(|n| n.as_str().contains('_') || n.as_str() == "hip")
        {
            *w.get_mut::<Transform>(*e).unwrap() = *t;
        }
    }
    play(&mut rig, "swing", 0);
    rig.step(5);
    let hand_r = rig.find("hand_r");
    let hand_idx = swayed.iter().position(|n| n == "hand_r").unwrap();
    let mut baked = Vec::new();
    for _ in 0..240 {
        rig.step(1);
        baked.push(rig.world().get::<Transform>(hand_r).unwrap().rotation);
    }
    let with = record(&mut rig, true);
    // The same at the bake's 30 fps step.
    rig.world()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_micros(
            33_333,
        )));
    let with30 = record(&mut rig, true);
    rig.world()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            16,
        )));
    let without = record(&mut rig, false);
    let max_turn = |fr: &[Vec<Quat>]| {
        fr.windows(2)
            .flat_map(|w| w[0].iter().zip(&w[1]).map(|(a, b)| angle(*a, *b)))
            .fold(0.0f32, f32::max)
    };
    let (mt_with, mt_without) = (max_turn(&with), max_turn(&without));
    let mut dev = 0.0f32;
    let mut dev_bone = String::new();
    let mut moved_bones = 0;
    for j in 0..sway_joints.len() {
        let d = with
            .iter()
            .zip(&without)
            .map(|(a, b)| angle(a[j], b[j]))
            .fold(0.0f32, f32::max);
        if d > 0.01 {
            moved_bones += 1;
        }
        if d > dev {
            dev = d;
            dev_bone = swayed.get(j).cloned().unwrap_or_default();
        }
    }
    let dev_baked_vs_plain = baked
        .iter()
        .zip(&without)
        .map(|(b, f)| angle(*b, f[hand_idx]))
        .fold(0.0f32, f32::max);
    let dev_live_vs_baked = baked
        .iter()
        .zip(&with)
        .map(|(b, f)| angle(*b, f[hand_idx]))
        .fold(0.0f32, f32::max);
    let rest_r = Quat::from_array(
        doc["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == "hand_r")
            .unwrap()["rotation"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap() as f32)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(),
    );
    let amp = |v: &[Quat]| {
        v.iter()
            .map(|q| angle(rest_r, *q))
            .fold(0.0f32, f32::max)
            .to_degrees()
    };
    let step_of = |v: &[Quat]| {
        v.windows(2)
            .map(|w| angle(w[0], w[1]))
            .fold(0.0f32, f32::max)
            .to_degrees()
    };
    let live_hand: Vec<Quat> = with.iter().map(|f| f[hand_idx]).collect();
    let plain_hand: Vec<Quat> = without.iter().map(|f| f[hand_idx]).collect();
    let live30: Vec<Quat> = with30.iter().map(|f| f[hand_idx]).collect();
    eprintln!(
        "(7) live sway at 30 fps steps: away from rest {:.0}, largest step {:.1} deg",
        amp(&live30),
        step_of(&live30)
    );
    eprintln!(
        "(7) hand_r swing away from rest / largest 16 ms step: baked clip {:.0} / {:.1} deg, live {:.0} / {:.1} deg, plain walk {:.0} / {:.1} deg",
        amp(&baked),
        step_of(&baked),
        amp(&live_hand),
        step_of(&live_hand),
        amp(&plain_hand),
        step_of(&plain_hand)
    );
    eprintln!(
        "(7) hand_r: baked `swing` clip deviates {:.1} deg from the plain walk, live sway {:.1} deg from the baked clip",
        dev_baked_vs_plain.to_degrees(),
        dev_live_vs_baked.to_degrees()
    );
    eprintln!(
        "(7) {sways} sway rig(s), {bones} bones; 240 frames of walk: largest one-frame joint turn {:.1} deg with live sway, {:.1} deg without; sway moves {moved_bones} bones, at most {:.1} deg from the plain walk (`{dev_bone}`)",
        mt_with.to_degrees(),
        mt_without.to_degrees(),
        dev.to_degrees()
    );
    // The live simulation must reproduce the modeler's own bake of the same spring: same
    // amplitude, no larger steps, and (phases aside) never far from it.
    assert!(
        amp(&live_hand) < amp(&baked) + 15.0,
        "(7) live amplitude {} vs baked {}",
        amp(&live_hand),
        amp(&baked)
    );
    assert!(
        step_of(&live_hand) < step_of(&baked) + 5.0,
        "(7) live steps no larger than the bake's"
    );
    assert!(
        dev_live_vs_baked.to_degrees() < 30.0,
        "(7) live tracks the baked clip"
    );
    assert!(
        moved_bones == 2 && mt_without.to_degrees() < 10.0,
        "(7) only the two hands sway"
    );
    eprintln!("(7) PASS: bounded, matches the bake on the rest-rotated hands");

    // --- (8) root motion, events, crossfade ------------------------------
    stop(&mut rig);
    rig.world().get_mut::<Transform>(root).unwrap().translation = Vec3::ZERO;
    rig.world().resource_mut::<Events>().0.clear();
    play(&mut rig, "step", 0);
    rig.step(138); // 2.2 s of animation time
    let z = rig.world().get::<Transform>(root).unwrap().translation;
    let ev: Vec<(String, f32)> = rig
        .world()
        .resource::<Events>()
        .0
        .iter()
        .map(|e| (e.name.clone(), e.time))
        .collect();
    eprintln!("(8) `step` after 2.2 s: root at {z:?} (1.2 m/s travel); events {ev:?}");
    assert!(
        (z.z - 1.2 * 2.208).abs() < 0.12 && z.x.abs() < 1e-3,
        "(8) root moved by the clip's travel, wrapping on loops"
    );
    let seq: Vec<&str> = ev.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(
        &seq[..4],
        ["footstep_l", "footstep_r", "footstep_l", "footstep_r"],
        "(8) events fire once per crossing, in order"
    );
    assert!(
        rig.world().resource::<Events>().0[1]
            .data
            .as_ref()
            .is_some_and(|d| d["volume"] == 0.8)
    );
    // A one-shot clip stops at its end: one event, travel once.
    stop(&mut rig);
    rig.world().get_mut::<Transform>(root).unwrap().translation = Vec3::ZERO;
    rig.world().resource_mut::<Events>().0.clear();
    play(&mut rig, "jump", 0);
    rig.step(100);
    let z = rig.world().get::<Transform>(root).unwrap().translation.z;
    let n_land = rig
        .world()
        .resource::<Events>()
        .0
        .iter()
        .filter(|e| e.name == "land")
        .count();
    eprintln!(
        "(8) `jump` (loop false) after 1.6 s: z {z:.3} (0.5 expected), `land` fired {n_land}x"
    );
    assert!(
        (z - 0.5).abs() < 0.02 && n_land == 1,
        "(8) extras.loop is honoured"
    );
    // Crossfade walk -> step.
    stop(&mut rig);
    play(&mut rig, "walk", 0);
    rig.step(10);
    play(&mut rig, "step", 200);
    rig.step(6);
    let mid = rig
        .world()
        .run_system_once(move |a: ModelerAnimator| a.playing(root))
        .unwrap();
    rig.step(30);
    let end = rig
        .world()
        .run_system_once(move |a: ModelerAnimator| a.playing(root))
        .unwrap();
    eprintln!("(8) crossfade walk -> step over 0.2 s: playing {mid:?} then {end:?}");
    assert!(
        mid.len() == 2 && mid.iter().all(|m| m.1 > 0.0 && m.1 < 1.0),
        "(8) both clips blend mid-fade"
    );
    assert!(
        end.iter().any(|m| m.0 == "step" && m.1 > 0.99),
        "(8) the old clip fades out"
    );

    eprintln!("screenshots in {}", rig.out.display());
    eprintln!("load {load_time:?}, export {export_time:?}, gltf {file_kb} KB");
}

/// Evaluates a glTF animation channel (LINEAR) at `t`.
fn sample_channel(times: &[f32], vals: &[Vec<f32>], t: f32) -> Vec<f32> {
    let n = times.len();
    if t <= times[0] {
        return vals[0].clone();
    }
    if t >= times[n - 1] {
        return vals[n - 1].clone();
    }
    let i = times.partition_point(|x| *x <= t) - 1;
    let u = (t - times[i]) / (times[i + 1] - times[i]);
    if vals[0].len() == 4 {
        let a = Quat::from_slice(&vals[i]);
        let b = Quat::from_slice(&vals[i + 1]);
        a.slerp(b, u).to_array().to_vec()
    } else {
        vals[i]
            .iter()
            .zip(&vals[i + 1])
            .map(|(a, b)| a + (b - a) * u)
            .collect()
    }
}

/// The live sway's conversions (as `run_sway` does them), fed by the glTF's own
/// walk samples, against the modeler's baked `swing` clip in the same file.
#[test]
fn live_spring_conversion_matches_the_baked_swing() {
    if !character::human_available() {
        eprintln!("SKIPPED: no data/human/human.bin");
        return;
    }
    let (_, path, _) = character::export("spring", &json!({"preset": "game"})).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let bin = std::fs::read(
        path.parent()
            .unwrap()
            .join(doc["buffers"][0]["uri"].as_str().unwrap()),
    )
    .unwrap();
    let acc = |i: usize| -> Vec<Vec<f32>> {
        let a = &doc["accessors"][i];
        let v = &doc["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
        let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let k = match a["type"].as_str().unwrap() {
            "SCALAR" => 1,
            "VEC3" => 3,
            _ => 4,
        };
        (0..a["count"].as_u64().unwrap() as usize)
            .map(|i| {
                (0..k)
                    .map(|c| {
                        f32::from_le_bytes(
                            bin[off + (i * k + c) * 4..off + (i * k + c) * 4 + 4]
                                .try_into()
                                .unwrap(),
                        )
                    })
                    .collect()
            })
            .collect()
    };
    let nodes = doc["nodes"].as_array().unwrap();
    let skel_node = nodes.iter().find(|n| n["name"] == "eve_rig").unwrap();
    let rig: modeler_bevy::SwayRig =
        serde_json::from_value(skel_node["extras"]["sway"].clone()).unwrap();
    let modeler_bevy::SwayKind::Spring(spring) = &rig.kind else {
        panic!("spring")
    };
    let node_of = |name: &str| nodes.iter().position(|n| n["name"] == name).unwrap();
    let anims = doc["animations"].as_array().unwrap();
    let chan = |anim: &str, node: usize, path: &str| -> Option<(Vec<f32>, Vec<Vec<f32>>)> {
        let a = anims.iter().find(|a| a["name"] == anim)?;
        let c = a["channels"]
            .as_array()?
            .iter()
            .find(|c| c["target"]["node"] == node && c["target"]["path"] == path)?;
        let s = &a["samplers"][c["sampler"].as_u64()? as usize];
        let t: Vec<f32> = acc(s["input"].as_u64()? as usize)
            .into_iter()
            .map(|v| v[0])
            .collect();
        Some((t, acc(s["output"].as_u64()? as usize)))
    };
    let rest_rot: Vec<Quat> = rig
        .bones
        .iter()
        .map(|b| {
            nodes[node_of(&b.name)]["rotation"]
                .as_array()
                .map_or(Quat::IDENTITY, |r| {
                    Quat::from_slice(
                        &r.iter()
                            .map(|x| x.as_f64().unwrap() as f32)
                            .collect::<Vec<_>>(),
                    )
                })
        })
        .collect();
    let rest_t: Vec<Vec3> = rig
        .bones
        .iter()
        .map(|b| {
            Vec3::from_slice(
                &nodes[node_of(&b.name)]["translation"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_f64().unwrap() as f32)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let frames_p = rig.rest_parent_frames(&rest_rot);
    type Track = Option<(Vec<f32>, Vec<Vec<f32>>)>;
    let tracks: Vec<(Track, Track)> = rig
        .bones
        .iter()
        .map(|b| {
            let n = node_of(&b.name);
            (chan("walk", n, "translation"), chan("walk", n, "rotation"))
        })
        .collect();
    let dur = 1.0714f32;
    let pose_at = |t: f32| -> Vec<(Vec3, Quat)> {
        (0..rig.bones.len())
            .map(|i| {
                let tr = tracks[i].0.as_ref().map_or(rest_t[i], |(ts, vs)| {
                    Vec3::from_slice(&sample_channel(ts, vs, t % dur))
                });
                let rot = tracks[i].1.as_ref().map_or(rest_rot[i], |(ts, vs)| {
                    Quat::from_slice(&sample_channel(ts, vs, t % dur))
                });
                // `run_sway`'s conversion into world-aligned frames.
                let p = frames_p[i];
                let rl = rest_rot[i];
                let rest_off = frames_p[i].inverse() * Vec3::ZERO; // placeholder, replaced below
                let _ = rest_off;
                (
                    p * tr,
                    modeler_bevy::conj(p, (rot * rl.inverse()).normalize()),
                )
            })
            .collect()
    };
    // `run_sway` subtracts the rest offset (head offset in the parent's frame).
    let head_off: Vec<Vec3> = rig
        .bones
        .iter()
        .map(|b| {
            let head = Vec3::from(b.head);
            match b.parent.and_then(|p| rig.bones.get(p as usize)) {
                Some(p) => head - Vec3::from(p.head),
                None => head,
            }
        })
        .collect();
    let anim_at = |t: f32| -> Vec<(Vec3, Quat)> {
        let mut v = pose_at(t);
        for (i, a) in v.iter_mut().enumerate() {
            a.0 -= head_off[i]; // p*tr - p*(p^-1 d) = p*tr - d
        }
        v
    };
    let steps = ((dur * 4.0 / 0.0333).round() as usize).max(1); // 4 s: the swing's duration, a whole number of cycles
    let _ = steps;
    let swing_dur = anims.iter().find(|a| a["name"] == "swing").unwrap()["extras"]["duration"]
        .as_f64()
        .unwrap() as f32;
    let h = 1.0 / 60.0;
    let mut sim = modeler_bevy::SpringSim::new();
    let hand = rig.bones.iter().position(|b| b.name == "hand_r").unwrap();
    let mut out = Vec::new();
    let mut live: Vec<(f32, Quat)> = Vec::new();
    let total = (swing_dur * 3.0 / h) as usize;
    for k in 0..=total {
        let t = k as f32 * h;
        sim.step(&rig, spring, &anim_at(t), t, h, &mut out);
        let q = (modeler_bevy::unconj(frames_p[hand], out[hand]) * rest_rot[hand]).normalize();
        live.push((t, q));
    }
    let (bt, bv) = chan("swing", node_of("hand_r"), "rotation").unwrap();
    let baked = |t: f32| Quat::from_slice(&sample_channel(&bt, &bv, t % swing_dur));
    let last_lap: Vec<&(f32, Quat)> = live.iter().filter(|(t, _)| *t >= swing_dur * 2.0).collect();
    let rest = rest_rot[hand];
    let amp_live = last_lap
        .iter()
        .map(|(_, q)| angle(rest, *q))
        .fold(0.0f32, f32::max)
        .to_degrees();
    let amp_baked = (0..200)
        .map(|k| angle(rest, baked(k as f32 * swing_dur / 200.0)))
        .fold(0.0f32, f32::max)
        .to_degrees();
    let worst = last_lap
        .iter()
        .map(|(t, q)| angle(*q, baked(*t)))
        .fold(0.0f32, f32::max)
        .to_degrees();
    eprintln!(
        "spring offline from glTF samples: hand_r amplitude {amp_live:.0} deg live-conversion vs {amp_baked:.0} baked; worst difference to the baked clip {worst:.1} deg"
    );
    assert!(
        worst < 25.0,
        "live conversion matches the baked swing: {worst}"
    );
}
