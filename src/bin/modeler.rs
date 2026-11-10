//! The modeler preview app: a small Bevy window showing the model, driven by an MCP
//! server on stdio. No game code.
//!
//! Run `modeler` for a windowed preview, `modeler --headless` for no window/render,
//! and `modeler --model <path.json>` to start from a saved [`modeler::Model`].
//!
//! Bridge design: the MCP server runs in its own thread on its own tokio runtime
//! (winit wants the main thread, so Bevy keeps it). Tools send a `Cmd` (a closure over
//! the [`Session`] or a screenshot request) plus a oneshot reply channel over a tokio
//! mpsc; a Bevy system drains the channel every frame, executes against the session
//! resource and answers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use bevy::app::AppExit;
use bevy::asset::RenderAssetUsages;
use bevy::camera::{RenderTarget, ScalingMode};
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings};
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::window::{PrimaryWindow, WindowPlugin};
use modeler::catalog::{self, CatalogEntry};
use modeler::commands::{BatchItem, PendingPin, Session};
use modeler::gltf;
use modeler::{Model, Op, Part};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::{ServiceExt, schemars, tool, tool_handler, tool_router};
use tokio::sync::{mpsc, oneshot};

type CmdResult = Result<String, String>;
type ShotReply = oneshot::Sender<Result<Vec<u8>, String>>;
type TextReply = oneshot::Sender<Result<String, String>>;

/// A unit of work for the Bevy main loop: mutate the session, or grab a picture.
enum Cmd {
    Exec(Box<dyn FnOnce(&mut Session) -> CmdResult + Send>),
    Screenshot(ShotReply),
    SetCamera {
        yaw: f32,
        pitch: f32,
        dist: f32,
        reply: ShotReply,
    },
    ProbeRay {
        origin: [f32; 3],
        dir: [f32; 3],
        reply: TextReply,
    },
    AgentShot {
        view: AgentView,
        width: u32,
        height: u32,
        reply: ShotReply,
    },
}

/// Shared between the MCP thread and the Bevy world (the receiver sits behind a mutex,
/// since tokio's receiver is not `Sync` and Bevy resources are).
struct Bridge {
    rx: Mutex<mpsc::Receiver<Cmd>>,
    server_done: AtomicBool,
}

impl Bridge {
    fn quit(&self) -> bool {
        self.server_done.load(Ordering::Relaxed)
    }
}

#[derive(Resource)]
struct Modeler {
    session: Session,
    bridge: Arc<Bridge>,
}

#[derive(Resource)]
struct Headless(bool);

fn windowed(headless: Option<Res<Headless>>) -> bool {
    !headless.map(|h| h.0).unwrap_or(false)
}

/// Screenshot requests waiting for the renderer to capture a frame.
#[derive(Resource, Default)]
struct PendingShots(Vec<Mutex<Option<ShotReply>>>);

/// Ray-cast probes waiting for a frame with the current meshes: origin, dir,
/// reply. Deferred to its own system because `MeshRayCast` borrows
/// `Assets<Mesh>` immutably.
#[derive(Resource, Default)]
struct PendingProbes(Vec<Option<([f32; 3], [f32; 3], TextReply)>>);

/// The agent's offscreen camera: renders to a texture so screenshots don't
/// touch the human's window or steal their orbit camera. Spawned by the
/// windowed startup; absent in headless mode.
#[derive(Resource, Default)]
struct AgentRig {
    camera: Option<Entity>,
    image: Option<Handle<Image>>,
}

/// Marks the offscreen agent camera entity (the main window camera is
/// [`MainCamera`]).
#[derive(Component)]
struct AgentCamera;

/// One offscreen shot waiting for a frame: view, pixel size, reply.
#[derive(Resource, Default)]
struct PendingAgentShots(Vec<(AgentView, u32, u32, ShotReply)>);

/// Named agent camera views. The six cardinal views are orthographic (true
/// proportions, auto-framed to the model bounds); `iso` is a perspective
/// three-quarter view.
#[derive(Clone, Copy, Debug)]
enum AgentView {
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
    Iso,
}

fn parse_agent_view(s: &str) -> Result<AgentView, String> {
    match s.to_ascii_lowercase().as_str() {
        "front" => Ok(AgentView::Front),
        "back" => Ok(AgentView::Back),
        "left" => Ok(AgentView::Left),
        "right" => Ok(AgentView::Right),
        "top" => Ok(AgentView::Top),
        "bottom" => Ok(AgentView::Bottom),
        "iso" => Ok(AgentView::Iso),
        other => Err(format!(
            "unknown view `{other}` (front, back, left, right, top, bottom, iso)"
        )),
    }
}

/// The orbit camera: angles in a resource, mouse drag / arrow keys to steer.
/// `user` is set once the human takes over, which stops the slow auto-drift.
#[derive(Resource)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    dist: f32,
    target: Vec3,
    user: bool,
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            yaw: 0.7,
            pitch: 0.45,
            dist: 6.0,
            target: Vec3::ZERO,
            user: false,
        }
    }
}

/// State of the current mouse drag (orbit with left, pan with middle/right).
#[derive(Resource, Default)]
struct CameraDrag {
    button: Option<MouseButton>,
    last: Vec2,
    moved: f32,
}

#[derive(Component)]
struct PreviewPart;

#[derive(Component)]
struct MainCamera;

/// A pin sphere marking where the human clicked (or where an annotation sits).
#[derive(Component)]
struct AnnotationPin;

/// A pin whose comment has been typed in and committed.
#[derive(Component)]
struct PinCommitted;

/// The text being typed for the pending pin, shown in the bottom HUD line.
#[derive(Resource, Default)]
struct PinTyping {
    text: String,
}

/// The bottom UI line showing the pin's typed-so-far comment.
#[derive(Component)]
struct TypingHud;

/// The left panel showing the procedural op graph and history cursor. Toggled
/// with G; read-only.
#[derive(Component)]
struct GraphHud;

#[derive(Resource, Default)]
struct GraphHudVisible(bool);

/// A click hit to turn into a pin marker, deferred to a spawn system because
/// `MeshRayCast` already borrows `Assets<Mesh>` immutably.
#[derive(Resource, Default)]
struct PendingPinSpawn {
    point: Option<(Vec3, u32)>,
}

/// Which model version the scene was last built from.
#[derive(Resource, Default)]
struct SceneBuilt {
    version: Option<u64>,
}

fn main() -> AppExit {
    let mut headless = false;
    let mut model_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headless" => headless = true,
            "--model" => model_path = args.next().map(PathBuf::from),
            other => eprintln!(
                "unknown argument `{other}` (usage: modeler [--headless] [--model <path.json>])"
            ),
        }
    }

    let mut session = Session::default();
    if let Some(path) = &model_path {
        let loaded = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Model>(&s).map_err(|e| e.to_string()));
        match loaded {
            Ok(model) => {
                eprintln!("loaded model `{}` from {}", model.name, path.display());
                session.model = model;
            }
            Err(e) => eprintln!("could not load {}: {e}", path.display()),
        }
    } else {
        // No explicit model: restore the last session's autosave so unnamed
        // WIP (ops, parts, history tree, annotations) survives a restart.
        if let Some(model) = catalog::load_session_state() {
            eprintln!(
                "restored session model `{}` (v{}) from autosave",
                model.name, model.version
            );
            session.model = model;
        }
    }

    let (tx, rx) = mpsc::channel::<Cmd>(64);
    let bridge = Arc::new(Bridge {
        rx: Mutex::new(rx),
        server_done: AtomicBool::new(false),
    });

    // The MCP server gets a thread of its own; winit/render must own the main thread.
    {
        let done = Arc::clone(&bridge);
        thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("modeler: could not start MCP runtime: {e}");
                    done.server_done.store(true, Ordering::Relaxed);
                    return;
                }
            };
            rt.block_on(async move {
                let server = match Tools::new(tx).serve(rmcp::transport::stdio()).await {
                    Ok(server) => server,
                    Err(e) => {
                        eprintln!("modeler: MCP server failed to start: {e}");
                        done.server_done.store(true, Ordering::Relaxed);
                        return;
                    }
                };
                if let Err(e) = server.waiting().await {
                    eprintln!("modeler: MCP server stopped: {e}");
                }
                done.server_done.store(true, Ordering::Relaxed);
            });
        });
    }

    let mut app = App::new();
    app.insert_resource(Modeler {
        session,
        bridge: Arc::clone(&bridge),
    })
    .insert_resource(Headless(headless))
    .insert_resource(PendingShots::default())
    .insert_resource(PendingProbes::default())
    .insert_resource(PendingAgentShots::default())
    .insert_resource(AgentRig::default())
    .insert_resource(Orbit::default())
    .init_resource::<CameraDrag>()
    .init_resource::<GraphHudVisible>()
    .init_resource::<SceneBuilt>()
    .init_resource::<PinTyping>()
    .init_resource::<PendingPinSpawn>()
    .add_systems(
        Update,
        (
            drain_commands,
            quit_when_server_done(headless),
            take_screenshot.run_if(windowed),
            run_probe.run_if(windowed),
            rebuild_scene.run_if(windowed),
            orbit_camera.run_if(windowed),
            pin_on_click.run_if(windowed),
            spawn_pin_markers.run_if(windowed),
            pin_typing.run_if(windowed),
            update_typing_hud.run_if(windowed),
            update_graph_hud.run_if(windowed),
        ),
    );
    if headless {
        app.add_plugins(MinimalPlugins); // MinimalPlugins already runs the schedule loop
    } else {
        app.add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "modeler".into(),
                ..default()
            }),
            ..default()
        }))
        .add_systems(Startup, setup_scene)
        .add_systems(Startup, setup_agent_camera)
        .add_systems(Update, (process_agent_shot.run_if(windowed),));
    }
    app.run()
}

/// Exits headless mode once the MCP server has hung up; windowed mode stays open so
/// the human can keep looking at the model.
fn quit_when_server_done(headless: bool) -> impl FnMut(Res<Modeler>, MessageWriter<AppExit>) {
    move |modeler, mut exit| {
        if headless && modeler.bridge.quit() {
            exit.write(AppExit::Success);
        }
    }
}

/// Drains MCP commands every frame and executes them against the session.
fn drain_commands(
    headless: Res<Headless>,
    mut modeler: ResMut<Modeler>,
    mut pending: ResMut<PendingShots>,
    mut probes: ResMut<PendingProbes>,
    mut agent_shots: ResMut<PendingAgentShots>,
    mut orbit: ResMut<Orbit>,
) {
    let bridge = Arc::clone(&modeler.bridge);
    let Ok(mut rx) = bridge.rx.lock() else {
        return;
    };
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Cmd::Exec(f) => {
                // The reply goes back to the MCP tool through the closure's channel.
                let _ = f(&mut modeler.session);
            }
            Cmd::Screenshot(reply) => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no renderer, no screenshot".into()));
                } else {
                    pending.0.push(Mutex::new(Some(reply)));
                }
            }
            Cmd::SetCamera {
                yaw,
                pitch,
                dist,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no camera to move".into()));
                } else {
                    orbit.yaw = yaw.to_radians();
                    orbit.pitch = pitch.clamp(-89.0, 89.0).to_radians();
                    orbit.dist = dist.clamp(0.2, 200.0);
                    orbit.user = true;
                    let _ = reply.send(Ok(vec![]));
                }
            }
            Cmd::ProbeRay { origin, dir, reply } => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no scene to ray-cast".into()));
                } else {
                    probes.0.push(Some((origin, dir, reply)));
                }
            }
            Cmd::AgentShot {
                view,
                width,
                height,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no renderer, no agent view".into()));
                } else {
                    agent_shots.0.push((view, width, height, reply));
                }
            }
        }
    }
}

/// Spawns one screenshot request per pending MCP tool call; the observer encodes the
/// captured frame to PNG and answers the waiting tool.
fn take_screenshot(mut commands: Commands, mut pending: ResMut<PendingShots>) {
    let Some(reply) = pending.0.pop() else {
        return;
    };
    commands
        .spawn(Screenshot::primary_window())
        .observe(move |captured: On<ScreenshotCaptured>| {
            if let Ok(mut slot) = reply.lock()
                && let Some(reply) = slot.take()
            {
                let _ = reply.send(encode_png(&captured.image));
            }
        })
        .insert(Name::new("mcp screenshot"));
}

/// Answers a pending `probe_ray` with the first part the ray hits: name,
/// distance along the ray and the world hit point. The deterministic check
/// for openings and occlusion — cast through a window hole to prove it is
/// open, cast at a door fill to prove it is solid.
fn run_probe(
    mut probes: ResMut<PendingProbes>,
    mut ray_cast: MeshRayCast,
    parts: Query<(Entity, &Name), With<PreviewPart>>,
) {
    let Some(Some((origin, dir, reply))) = probes.0.pop() else {
        return;
    };
    let Ok(direction) = Dir3::new(Vec3::from(dir).normalize_or_zero()) else {
        let _ = reply.send(Err("ray direction is zero".into()));
        return;
    };
    let ray = Ray3d {
        origin: Vec3::from(origin),
        direction,
    };
    let filter = |e| parts.contains(e);
    let settings = MeshRayCastSettings::default().with_filter(&filter);
    match ray_cast.cast_ray(ray, &settings).first() {
        Some((entity, hit)) => {
            let name = parts
                .get(*entity)
                .map(|(_, n)| n.as_str().to_string())
                .unwrap_or_else(|_| "<unknown>".into());
            let p = hit.point;
            let _ = reply.send(Ok(format!(
                "hit `{name}` at t={:.4}, point [{:.4}, {:.4}, {:.4}]",
                hit.distance, p.x, p.y, p.z
            )));
        }
        None => {
            let _ = reply.send(Ok("no hit — the ray passes clean through".into()));
        }
    }
}

/// Spawns the agent's offscreen camera rig: a render-to-texture target plus a
/// camera the tools reposition per shot. Independent of the human's orbit
/// camera, so agent screenshots never disturb the window view. The camera
/// carries its own directional light so every view is lit regardless of the
/// scene's light orientation.
fn setup_agent_camera(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = images.add(Image::new_target_texture(
        1280,
        720,
        TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    // Camera3d requires RenderTarget, so it cannot also appear in the spawn
    // bundle; insert the image target after the spawn instead. ClearColor
    // follows the world default, matching the human window's background.
    let camera = commands
        .spawn((
            Name::new("agent camera"),
            AgentCamera,
            Camera3d::default(),
            Projection::from(PerspectiveProjection::default()),
            Transform::from_translation(Vec3::new(4.0, 3.0, 6.0)).looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .with_children(|c| {
            // Key + fill: dimmer than the scene's sun so low-roughness parts
            // don't blow out to white in agent views (a hot highlight reads
            // as solid geometry to an agent that cannot orbit to check).
            c.spawn((
                DirectionalLight {
                    illuminance: 350.0,
                    ..default()
                },
                Transform::default(),
            ));
            c.spawn((
                DirectionalLight {
                    illuminance: 150.0,
                    ..default()
                },
                Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
            ));
        })
        .insert(RenderTarget::Image(image.clone().into()))
        .id();
    commands.insert_resource(AgentRig {
        camera: Some(camera),
        image: Some(image),
    });
}

/// Computes the camera placement for one agent view, auto-framed to the
/// model's world bounds. Returns (eye, target, up, projection).
fn agent_view_projection(
    view: AgentView,
    bounds: Option<(Vec3, Vec3)>,
    aspect: f32,
) -> (Vec3, Vec3, Vec3, Projection) {
    let (min, max) = bounds.unwrap_or((Vec3::splat(-1.0), Vec3::splat(1.0)));
    let center = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let radius = half.length().max(0.5);
    let margin = 1.2;
    match view {
        AgentView::Iso => {
            let dist = (radius * 2.6).max(2.0);
            let dir = Vec3::new(1.0, 0.75, 1.0).normalize();
            (
                center + dir * dist,
                center,
                Vec3::Y,
                Projection::from(PerspectiveProjection {
                    fov: 45.0_f32.to_radians(),
                    aspect_ratio: aspect,
                    near: 0.05,
                    far: (dist * 4.0 + radius * 10.0).max(10.0),
                    ..default()
                }),
            )
        }
        view => {
            let (offset, up, (mut hw, mut hh)): (Vec3, Vec3, (f32, f32)) = match view {
                AgentView::Front => (Vec3::Z, Vec3::Y, (half.x, half.y)),
                AgentView::Back => (-Vec3::Z, Vec3::Y, (half.x, half.y)),
                AgentView::Right => (Vec3::X, Vec3::Y, (half.z, half.y)),
                AgentView::Left => (-Vec3::X, Vec3::Y, (half.z, half.y)),
                AgentView::Top => (Vec3::Y, Vec3::Z, (half.x, half.z)),
                AgentView::Bottom => (-Vec3::Y, Vec3::Z, (half.x, half.z)),
                AgentView::Iso => unreachable!(),
            };
            hw *= margin;
            hh *= margin;
            // Fit the world span into the requested pixel aspect.
            if hw / hh < aspect {
                hw = hh * aspect;
            } else {
                hh = hw / aspect;
            }
            let dist = radius * 3.0 + 5.0;
            (
                center + offset * dist,
                center,
                up,
                Projection::from(OrthographicProjection {
                    near: 0.0,
                    far: dist * 2.0 + radius * 10.0 + 10.0,
                    // Fixed, not WindowSize: the target is an offscreen image,
                    // so the world span must come from our framing, not the
                    // human's window dimensions.
                    scaling_mode: ScalingMode::Fixed {
                        width: hw * 2.0,
                        height: hh * 2.0,
                    },
                    ..OrthographicProjection::default_3d()
                }),
            )
        }
    }
}

/// Takes one pending agent shot per frame: repositions the offscreen camera,
/// resizes the target image, then requests the capture. One per frame so each
/// capture is guaranteed to show its own view.
fn process_agent_shot(
    mut agent_shots: ResMut<PendingAgentShots>,
    mut rig: ResMut<AgentRig>,
    mut images: ResMut<Assets<Image>>,
    mut cameras: Query<(&mut Projection, &mut RenderTarget, &mut Transform), With<AgentCamera>>,
    modeler: Res<Modeler>,
    mut commands: Commands,
) {
    let Some((view, width, height, reply)) = agent_shots.0.pop() else {
        return;
    };
    let (Some(camera_entity), Some(image_handle)) = (rig.camera, rig.image.clone()) else {
        let _ = reply.send(Err("agent camera is not initialized".into()));
        return;
    };
    let mut image_handle = image_handle;
    let aspect = width as f32 / height.max(1) as f32;
    let (eye, target, up, new_projection) =
        agent_view_projection(view, modeler.session.model.bounds(), aspect);
    let Ok((mut projection, mut render_target, mut transform)) = cameras.get_mut(camera_entity)
    else {
        let _ = reply.send(Err("agent camera missing".into()));
        return;
    };
    *projection = new_projection;
    *transform = Transform::from_translation(eye).looking_at(target, up);
    let (cur_w, cur_h) = images
        .get(&image_handle)
        .map(|i| (i.width(), i.height()))
        .unwrap_or((0, 0));
    if cur_w != width || cur_h != height {
        // Recreate the target instead of resizing: the camera's render target
        // handle is updated below, so this is invisible to the GPU graph.
        let fresh = images.add(Image::new_target_texture(
            width,
            height,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
        rig.image = Some(fresh.clone());
        image_handle = fresh;
    }
    *render_target = RenderTarget::Image(image_handle.clone().into());
    let slot = Mutex::new(Some(reply));
    commands
        .spawn(Screenshot::image(image_handle))
        .observe(move |captured: On<ScreenshotCaptured>| {
            if let Ok(mut s) = slot.lock()
                && let Some(reply) = s.take()
            {
                let _ = reply.send(encode_png(&captured.image));
            }
        })
        .insert(Name::new("mcp agent shot"));
}

/// Rebuilds one entity per part whenever the model version moves, and re-frames the
/// orbit camera around the model's bounds.
fn rebuild_scene(
    mut commands: Commands,
    mut modeler: ResMut<Modeler>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut orbit: ResMut<Orbit>,
    mut built: ResMut<SceneBuilt>,
    mut typing: ResMut<PinTyping>,
    existing: Query<Entity, With<PreviewPart>>,
    pins: Query<Entity, With<AnnotationPin>>,
) {
    let version = modeler.session.model.version;
    if built.version == Some(version) {
        return;
    }
    built.version = Some(version);
    for e in existing.iter().chain(&pins) {
        commands.entity(e).despawn();
    }
    // The model changed under any pending pin; drop it so no stale comment lands.
    modeler.session.pending_pin = None;
    typing.text.clear();
    for part in &modeler.session.model.parts {
        if part.verts.is_empty() || part.indices.is_empty() || !part.visible {
            continue;
        }
        commands.spawn((part_bundle(part, &mut meshes, &mut materials), PreviewPart));
    }
    if let Some((min, max)) = modeler.session.model.bounds() {
        orbit.target = (min + max) * 0.5;
        let radius = (max - min).length() * 0.5;
        orbit.dist = (radius * 2.4).max(1.0);
        // A new model re-frames the view and hands control back to the drift.
        orbit.user = false;
    }
}

fn part_bundle(
    part: &Part,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> impl Bundle {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, part.verts.clone())
    .with_inserted_indices(Indices::U32(part.indices.clone()));
    if !part.normals.is_empty() {
        mesh = mesh.with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, part.normals.clone());
    }
    if !part.uvs.is_empty() {
        mesh = mesh.with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, part.uvs.clone());
    }
    let [r, g, b, a] = part.material.base_color;
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(r, g, b, a),
        perceptual_roughness: part.material.roughness,
        metallic: part.material.metalness,
        ..default()
    });
    (
        Name::new(part.name.clone()),
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material),
        Transform {
            translation: Vec3::from_array(part.translation),
            rotation: modeler::quat_of(part.rotation),
            scale: Vec3::from_array(part.scale),
        },
    )
}

/// Mouse and keyboard camera control: left-drag orbits, middle/right-drag pans,
/// scroll wheel zooms, arrows/PageUp/PageDown still work. A slow auto-drift runs
/// until the user first moves the camera themselves.
fn orbit_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut orbit: ResMut<Orbit>,
    mut drag: ResMut<CameraDrag>,
    mut camera: Query<&mut Transform, With<MainCamera>>,
) {
    if scroll.delta.y != 0.0 {
        orbit.dist = (orbit.dist * (1.0 - 0.12 * scroll.delta.y)).clamp(0.2, 200.0);
        orbit.user = true;
    }
    if keys.pressed(KeyCode::PageUp) {
        orbit.dist = (orbit.dist * 0.95).max(0.2);
        orbit.user = true;
    }
    if keys.pressed(KeyCode::PageDown) {
        orbit.dist = (orbit.dist * 1.05).min(200.0);
        orbit.user = true;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let dragging = [MouseButton::Left, MouseButton::Middle, MouseButton::Right]
        .into_iter()
        .find(|b| mouse.pressed(*b));
    if let Some(button) = dragging {
        if let Some(cursor) = window.cursor_position() {
            if drag.button != Some(button) {
                drag.button = Some(button);
                drag.last = cursor;
                drag.moved = 0.0;
            } else {
                let d = cursor - drag.last;
                drag.moved += d.length();
                drag.last = cursor;
                match button {
                    // Left-drag orbits around the target; click-without-drag pins.
                    MouseButton::Left => {
                        orbit.yaw -= d.x * 0.01;
                        orbit.pitch = (orbit.pitch + d.y * 0.01).clamp(-0.2, 1.4);
                        orbit.user = true;
                    }
                    // Middle/right-drag pans the target in the camera's screen plane.
                    _ => {
                        let (sin_y, cos_y) = orbit.yaw.sin_cos();
                        let (sin_p, cos_p) = orbit.pitch.sin_cos();
                        let right = Vec3::new(sin_y, 0.0, -cos_y);
                        let up = right
                            .cross(-Vec3::new(cos_y * cos_p, sin_p, sin_y * cos_p))
                            .normalize();
                        let s = orbit.dist * 0.0016;
                        orbit.target -= right * (d.x * s);
                        orbit.target += up * (d.y * s);
                        orbit.user = true;
                    }
                }
            }
        }
    } else {
        drag.button = None;
    }

    // Gentle auto-orbit only until the user steers the camera themselves.
    if !orbit.user && drag.button.is_none() {
        orbit.yaw += time.delta_secs() * 0.25;
    }
    let spin = 1.5 * time.delta_secs();
    if keys.pressed(KeyCode::ArrowLeft) {
        orbit.yaw -= spin;
        orbit.user = true;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        orbit.yaw += spin;
        orbit.user = true;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        orbit.pitch = (orbit.pitch + spin).min(1.4);
        orbit.user = true;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        orbit.pitch = (orbit.pitch - spin).max(-0.2);
        orbit.user = true;
    }
    let (sin_y, cos_y) = orbit.yaw.sin_cos();
    let (sin_p, cos_p) = orbit.pitch.sin_cos();
    let eye = orbit.target + Vec3::new(cos_y * cos_p, sin_p, sin_y * cos_p) * orbit.dist;
    let Ok(mut cam) = camera.single_mut() else {
        return;
    };
    *cam = Transform::from_translation(eye).looking_at(orbit.target, Vec3::Y);
}

/// Camera, lights and the grey ground slab the model sits on.
fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Name::new("camera"),
        MainCamera,
        Camera3d::default(),
        Transform::from_translation(Vec3::new(4.0, 3.0, 6.0)).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Name::new("ambient light"),
        AmbientLight {
            brightness: 900.0,
            ..default()
        },
    ));
    commands.spawn((
        Name::new("sun"),
        DirectionalLight {
            illuminance: 8000.0,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.5, 0.0)),
    ));
    commands.spawn((
        Name::new("typing hud"),
        TypingHud,
        Text::new(String::new()),
        TextFont {
            font_size: FontSize::Px(20.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(8.0),
            left: Val::Px(8.0),
            ..default()
        },
    ));
    commands.spawn((
        Name::new("graph hud"),
        GraphHud,
        Visibility::Hidden,
        Text::new(String::new()),
        TextFont {
            font_size: FontSize::Px(14.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(8.0),
            ..default()
        },
    ));
    let ground = materials.add(StandardMaterial {
        base_color: Color::srgb(0.35, 0.35, 0.38),
        ..default()
    });
    commands.spawn((
        Name::new("ground"),
        Mesh3d(meshes.add(Plane3d::default().mesh().size(100.0, 100.0))),
        MeshMaterial3d(ground),
        Transform::from_translation(Vec3::new(0.0, -0.01, 0.0)),
    ));
}

/// Pin radius: about 2% of the model bounds diagonal, so it reads at any size.
fn pin_radius(model: &Model) -> f32 {
    model
        .bounds()
        .map(|(min, max)| ((max - min).length() * 0.02).max(0.02))
        .unwrap_or(0.05)
}

/// A click on a part pins a spot: a small bright sphere with the annotation id it
/// would get, and a `PendingPin` on the session for the next comment to fill in.
fn pin_on_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    mut ray_cast: MeshRayCast,
    parts: Query<(Entity, &Name), With<PreviewPart>>,
    pins: Query<(Entity, Option<&PinCommitted>), With<AnnotationPin>>,
    mut modeler: ResMut<Modeler>,
    mut commands: Commands,
    mut spawn: ResMut<PendingPinSpawn>,
    drag: Res<CameraDrag>,
) {
    // Pins land on a click (press + release with barely any movement), never on
    // the end of a left-drag orbit.
    if !mouse.just_released(MouseButton::Left) || drag.moved > 6.0 {
        return;
    }
    let Ok((camera, cam_transform)) = cameras.single() else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_transform, cursor) else {
        return;
    };
    let filter = |e| parts.contains(e);
    let settings = MeshRayCastSettings::default().with_filter(&filter);
    let Some((hit_entity, hit)) = ray_cast.cast_ray(ray, &settings).first() else {
        return;
    };
    let Ok((_, name)) = parts.get(*hit_entity) else {
        return;
    };
    // One pin at a time: replace any pin that is still waiting for its comment.
    for (e, committed) in &pins {
        if committed.is_none() {
            commands.entity(e).despawn();
        }
    }
    let id = modeler.session.model.next_annotation_id;
    // Deferred: MeshRayCast already borrows Assets<Mesh>, so the pin mesh is
    // spawned by the following system.
    spawn.point = Some((hit.point, id));
    modeler.session.pending_pin = Some(PendingPin {
        part: name.to_string(),
        face: hit.triangle_index.map(|i| i as u32),
        vertex: None,
        position: hit.point.to_array(),
    });
}

/// Spawns pin markers requested by [`pin_on_click`].
fn spawn_pin_markers(
    mut spawn: ResMut<PendingPinSpawn>,
    modeler: Res<Modeler>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some((point, id)) = spawn.point.take() else {
        return;
    };
    let radius = pin_radius(&modeler.session.model);
    spawn_pin(
        &mut commands,
        &mut meshes,
        &mut materials,
        radius,
        id,
        point,
    );
}

/// Spawn a pin marker (sphere + number label) at `point`.
fn spawn_pin(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    radius: f32,
    id: u32,
    point: Vec3,
) -> Entity {
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.85, 0.1),
        emissive: LinearRgba::rgb(1.0, 0.85, 0.1),
        ..default()
    });
    let pin = commands
        .spawn((
            Name::new(format!("pin #{id}")),
            AnnotationPin,
            Mesh3d(meshes.add(Sphere::new(radius))),
            MeshMaterial3d(material),
            Transform::from_translation(point),
        ))
        .id();
    commands.entity(pin).with_child((
        Text2d::new(format!("{id}")),
        TextFont {
            font_size: FontSize::Px(24.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Transform::from_xyz(0.0, radius * 3.0, 0.0),
    ));
    pin
}

/// Typing for the pending pin: characters collect into a buffer shown at the bottom
/// of the window; Enter commits the annotation, Escape cancels the text, Backspace
/// deletes a character. Always on (no focus to track), but inert without a pin.
#[allow(clippy::type_complexity)]
fn pin_typing(
    mut key_events: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mut modeler: ResMut<Modeler>,
    mut typing: ResMut<PinTyping>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    uncommitted: Query<
        (Entity, &AnnotationPin, &MeshMaterial3d<StandardMaterial>),
        Without<PinCommitted>,
    >,
) {
    if modeler.session.pending_pin.is_none() {
        typing.text.clear();
        return;
    }
    if keys.just_pressed(KeyCode::Backspace) {
        typing.text.pop();
    }
    if keys.just_pressed(KeyCode::Escape) {
        // Escape cancels annotation mode entirely: drop the pending pin and
        // its on-screen marker, not just the text typed so far.
        typing.text.clear();
        if modeler.session.pending_pin.take().is_some() {
            for (entity, _, _) in &uncommitted {
                commands.entity(entity).despawn();
            }
        }
        return;
    }
    if keys.just_pressed(KeyCode::Enter) {
        let Some(pin) = modeler.session.pending_pin.take() else {
            return;
        };
        let text = std::mem::take(&mut typing.text);
        let position = pin.position;
        let face = pin.face;
        let vertex = pin.vertex;
        let part = pin.part.clone();
        let result = modeler
            .session
            .add_annotation(&part, &text, face, vertex, Some(position));
        if result.is_ok()
            && let Some((entity, _, material)) = uncommitted.iter().next()
        {
            commands.entity(entity).insert(PinCommitted);
            if let Some(mut mat) = materials.get_mut(&material.0) {
                mat.base_color = Color::srgb(0.2, 0.9, 0.3);
                mat.emissive = LinearRgba::rgb(0.2, 0.9, 0.3);
            }
        } else {
            // Keep the pin alive for another try if the commit failed.
            modeler.session.pending_pin = Some(pin);
        }
        return;
    }
    for ev in key_events.read() {
        if ev.state != ButtonState::Pressed {
            continue;
        }
        // Space arrives as `Key::Space`, not a character, on winit.
        match &ev.logical_key {
            Key::Character(c) => {
                if let Some(ch) = c.chars().next()
                    && !ch.is_control()
                {
                    typing.text.push(ch);
                }
            }
            Key::Space => typing.text.push(' '),
            _ => {}
        }
    }
}

/// The bottom HUD line: what pin is being typed for and the text so far.
fn update_typing_hud(
    modeler: Res<Modeler>,
    typing: Res<PinTyping>,
    mut hud: Query<&mut Text, With<TypingHud>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };
    text.0 = match &modeler.session.pending_pin {
        Some(pin) => format!(
            "pin #{} on `{}`: {}_  (Enter to comment, Esc to clear)",
            modeler.session.model.next_annotation_id, pin.part, typing.text
        ),
        None => String::new(),
    };
}

/// G toggles the read-only graph panel: the op chain the parts derive from,
/// plus the history tree with its cursor. Hidden until asked for.
fn update_graph_hud(
    keys: Res<ButtonInput<KeyCode>>,
    mut visible: ResMut<GraphHudVisible>,
    modeler: Res<Modeler>,
    mut hud: Query<(&mut Text, &mut Visibility), With<GraphHud>>,
) {
    if keys.just_pressed(KeyCode::KeyG) {
        visible.0 = !visible.0;
    }
    let Ok((mut text, mut vis)) = hud.single_mut() else {
        return;
    };
    *vis = if visible.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if !visible.0 {
        text.0.clear();
        return;
    }
    let mut s = format!(
        "{}\n---\n",
        modeler::graph::describe(&modeler.session.model.ops)
    );
    s += &modeler.session.model.history.describe();
    text.0 = s;
}

/// Encode a captured frame to PNG bytes (BGRA/RGBA in, RGBA PNG out).
fn encode_png(image: &Image) -> Result<Vec<u8>, String> {
    let (w, h) = (image.width(), image.height());
    let Some(data) = image.data.as_deref() else {
        return Err(format!("screenshot {w}x{h} has no pixel data"));
    };
    if w == 0 || h == 0 || data.len() != (w as usize) * (h as usize) * 4 {
        return Err(format!("unexpected screenshot {w}x{h}"));
    }
    let mut rgba = data.to_vec();
    match image.texture_descriptor.format {
        TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm => {
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        TextureFormat::Rgba8UnormSrgb | TextureFormat::Rgba8Unorm => {}
        other => return Err(format!("unsupported screenshot format {other:?}")),
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| format!("png body: {e}"))?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// MCP server
// ---------------------------------------------------------------------------

fn ok_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

fn err_result(message: String) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message)])
}

/// The MCP tool surface. Every tool ships a command to the Bevy loop and awaits its
/// oneshot reply, so state changes always happen on the main thread.
struct Tools {
    tx: mpsc::Sender<Cmd>,
}

impl Tools {
    fn new(tx: mpsc::Sender<Cmd>) -> Self {
        Self { tx }
    }

    async fn run(
        &self,
        f: impl FnOnce(&mut Session) -> CmdResult + Send + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = Cmd::Exec(Box::new(move |session: &mut Session| {
            let reply = f(session);
            // Autosave after every mutation so unnamed WIP survives a restart.
            if reply.is_ok()
                && let Err(e) = catalog::save_session_state(&session.model)
            {
                eprintln!("session autosave failed: {e}");
            }
            let _ = reply_tx.send(reply.clone());
            reply
        }));
        self.tx
            .send(cmd)
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        match reply_rx.await {
            Ok(Ok(text)) => Ok(ok_result(text)),
            Ok(Err(message)) => Ok(err_result(message)),
            Err(_) => Err(ErrorData::internal_error(
                "preview app dropped the reply",
                None,
            )),
        }
    }

    /// Waits for a Bevy-side reply with a hard timeout: a stuck frame (or any
    /// hang in the render loop) surfaces as an error to the calling tool
    /// instead of wedging the MCP server forever.
    async fn await_reply(
        reply_rx: tokio::sync::oneshot::Receiver<Result<Vec<u8>, String>>,
    ) -> Result<Vec<u8>, String> {
        match tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx).await {
            Ok(Ok(inner)) => inner,
            Ok(Err(_)) => Err("preview app dropped the request".into()),
            Err(_) => Err(
                "timed out after 15s waiting for the preview app - the render may be stuck; retry or restart the daemon".into(),
            ),
        }
    }

    async fn grab_screenshot(&self) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::Screenshot(reply_tx))
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let started = std::time::Instant::now();
        match Self::await_reply(reply_rx).await {
            Ok(png) => {
                log_call("screenshot", true, started.elapsed().as_millis());
                Ok(CallToolResult::success(vec![
                    Content::image(B64.encode(png), "image/png"),
                    Content::text("current preview render"),
                ]))
            }
            Err(message) => {
                log_call("screenshot", false, started.elapsed().as_millis());
                Ok(err_result(message))
            }
        }
    }

    /// One offscreen agent shot: repositions the rig camera, waits for the
    /// capture, returns (png bytes, human label).
    async fn agent_render(
        &self,
        view: AgentView,
        width: u32,
        height: u32,
    ) -> Result<(Vec<u8>, String), ErrorData> {
        let started = std::time::Instant::now();
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::AgentShot {
                view,
                width,
                height,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let png = Self::await_reply(reply_rx)
            .await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        log_call("agent_shot", true, started.elapsed().as_millis());
        Ok((png, format!("{view:?} {width}x{height}")))
    }
}

/// Per-call session log: (tool, ok, milliseconds). Capped; the `session_log`
/// tool surfaces it so an agent can self-audit a build (failures, latency).
static CALL_LOG: Mutex<Vec<(String, bool, u128)>> = Mutex::new(Vec::new());

fn log_call(tool: &str, ok: bool, ms: u128) {
    if let Ok(mut log) = CALL_LOG.lock() {
        log.push((tool.to_string(), ok, ms));
        if log.len() > 500 {
            log.remove(0);
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AddPrimitiveArgs {
    /// Kind of primitive, e.g. "box", "sphere", "cylinder" (see the primitives module).
    kind: String,
    /// Size in metres: [width, height, depth].
    size: [f32; 3],
    /// Where to put it: [x, y, z].
    translation: [f32; 3],
    /// Unique part name to give it.
    name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AddTriMeshArgs {
    /// Unique part name.
    name: String,
    /// Vertex positions [x, y, z], in metres.
    verts: Vec<[f32; 3]>,
    /// Triangle indices into `verts`, exactly three per triangle.
    indices: Vec<u32>,
    /// Optional per-vertex normals; computed automatically when omitted.
    normals: Option<Vec<[f32; 3]>>,
    /// Optional UVs [u, v]; box projection when omitted.
    uvs: Option<Vec<[f32; 2]>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct BooleanArgs {
    /// Boolean operation: "union", "difference" or "intersection".
    op: String,
    /// Name of the first part (the base).
    a: String,
    /// Name of the second part (the tool).
    b: String,
    /// Optional name for the result part.
    name: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TransformPartArgs {
    /// The part to move.
    part: String,
    /// New translation [x, y, z]; unchanged when omitted.
    translation: Option<[f32; 3]>,
    /// New rotation as quaternion [x, y, z, w]; unchanged when omitted.
    rotation: Option<[f32; 4]>,
    /// New scale [x, y, z]; unchanged when omitted.
    scale: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct PartArgs {
    /// The part name.
    part: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RenamePartArgs {
    /// The part to rename.
    part: String,
    /// The new name.
    to: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SetMaterialArgs {
    /// The part to paint.
    part: String,
    /// Base colour [r, g, b, a], 0..1; unchanged when omitted.
    base_color: Option<[f32; 4]>,
    /// Roughness 0..1 (0 shiny, 1 matte); unchanged when omitted.
    roughness: Option<f32>,
    /// Metalness 0..1; unchanged when omitted.
    metalness: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SetUvsArgs {
    /// The part to re-project.
    part: String,
    /// UV projection to apply, e.g. "box", "planar", "spherical".
    projection: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ExportModelArgs {
    /// File path to write, e.g. "chair.glb".
    path: String,
    /// "gltf" for a JSON .gltf or "glb" for a binary .glb.
    format: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SavePrefabArgs {
    /// Human description of what the model is.
    description: String,
    /// Short search tags, e.g. ["furniture", "wood"].
    tags: Vec<String>,
    /// The prompt/idea that produced this model, for future agents.
    prompt: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct NameArgs {
    /// The new name.
    name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AddAnnotationArgs {
    /// The part the comment is about.
    part: String,
    /// The comment text.
    text: String,
    /// Optional triangle index (from add_tri_mesh/boolean output) to pin the comment to.
    face: Option<u32>,
    /// Optional vertex index to pin the comment to.
    vertex: Option<u32>,
    /// Optional world-space point [x, y, z] the comment sits at; defaults to the part's
    /// centroid, or the spot the human clicked in the live view when face/vertex/position
    /// are all omitted.
    position: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AnnotationIdArgs {
    /// The annotation id, as shown by list_annotations.
    id: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct HistoryIdArgs {
    /// The history node id, as shown by history.
    id: usize,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpPrimitiveArgs {
    /// Unique output part name; re-using a name replaces that op (parameter tweak).
    name: String,
    /// Primitive kind: box, sphere, cylinder, cone, torus, capsule, plane.
    kind: String,
    /// Full extents for box/plane; radius for sphere; [radius, height, _] for
    /// cylinder/cone/capsule; [ring, tube, _] for torus.
    size: [f32; 3],
    translation: [f32; 3],
    /// Optional rotation, quaternion [x, y, z, w].
    #[serde(default)]
    rotation: Option<[f32; 4]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpTransformArgs {
    /// Unique output part name; re-using a name replaces that op.
    name: String,
    /// An earlier op's output part.
    input: String,
    #[serde(default)]
    translation: Option<[f32; 3]>,
    /// Quaternion [x, y, z, w].
    #[serde(default)]
    rotation: Option<[f32; 4]>,
    #[serde(default)]
    scale: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpBooleanArgs {
    /// Unique output part name; re-using a name replaces that op.
    name: String,
    /// union, difference or intersection.
    op: String,
    a: String,
    b: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpRadialArrayArgs {
    /// Output name prefix: copies become name_0 .. name_{count-1}.
    name: String,
    /// An earlier op's output part to copy.
    input: String,
    count: u32,
    /// The circle's center; copies rotate about +Y through this point.
    #[serde(default)]
    center: [f32; 3],
    /// Arc to spread the copies over, in degrees. Default 360 (full ring).
    #[serde(default = "default_tool_degrees")]
    degrees: f32,
    /// Angle of the first copy. Use step (= degrees / count) plus count-1
    /// copies when the source part itself should fill the 0° slot evenly.
    #[serde(default)]
    start_degrees: f32,
    /// Rotation axis: "y" (default), "x" or "z". "x"/"z" make horizontal
    /// rings (windmill blades, fan spokes).
    #[serde(default = "default_axis")]
    axis: String,
}

fn default_axis() -> String {
    "y".to_string()
}

fn default_tool_degrees() -> f32 {
    360.0
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpMaterialArgs {
    /// Unique output part name; re-using a name replaces that op.
    name: String,
    /// An earlier op's output part.
    input: String,
    #[serde(default)]
    base_color: Option<[f32; 4]>,
    #[serde(default)]
    roughness: Option<f32>,
    #[serde(default)]
    metalness: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpLinearArrayArgs {
    /// Output name prefix: copies become name_1 .. name_{count-1}.
    name: String,
    /// An earlier op's output part to copy.
    input: String,
    count: u32,
    /// Per-copy step vector; copy i sits at input + i * offset.
    #[serde(default)]
    offset: [f32; 3],
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpMirrorArgs {
    /// Unique output part name; re-using a name replaces that op.
    name: String,
    /// An earlier op's output part to mirror.
    input: String,
    /// The plane's axis: "x", "y" or "z".
    axis: String,
    /// Where the mirror plane sits on that axis, e.g. [0.5, 0, 0] mirrors
    /// across x = 0.5.
    #[serde(default)]
    offset: [f32; 3],
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OpNameArgs {
    /// The op's output part name, as shown by scene_summary's ops list.
    name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct CatalogNameArgs {
    /// The prefab/catalog entry name.
    name: String,
}

/// One op in an `apply_ops` batch, tagged by "type" (e.g.
/// {"type": "primitive", "name": "leg", "kind": "cylinder", ...}). Same
/// parameters as the individual op tools.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpSpec {
    Primitive {
        /// Unique output part name; re-using a name replaces that op.
        name: String,
        /// Primitive kind: box, sphere, cylinder, cone, capsule, torus, plane, wedge, disc, washer.
        kind: String,
        /// Full extents for box/plane/wedge; radius for sphere; [radius, height, _] for cylinder/cone/capsule.
        size: [f32; 3],
        translation: [f32; 3],
        /// Optional rotation, quaternion [x, y, z, w].
        #[serde(default)]
        rotation: Option<[f32; 4]>,
    },
    Transform {
        name: String,
        input: String,
        #[serde(default)]
        translation: Option<[f32; 3]>,
        /// Quaternion [x, y, z, w].
        #[serde(default)]
        rotation: Option<[f32; 4]>,
        #[serde(default)]
        scale: Option<[f32; 3]>,
    },
    Boolean {
        name: String,
        /// union, difference or intersection.
        op: String,
        a: String,
        b: String,
    },
    RadialArray {
        name: String,
        input: String,
        count: u32,
        #[serde(default)]
        center: [f32; 3],
        #[serde(default = "default_tool_degrees")]
        degrees: f32,
        #[serde(default)]
        start_degrees: f32,
        #[serde(default = "default_axis")]
        axis: String,
    },
    LinearArray {
        name: String,
        input: String,
        count: u32,
        #[serde(default)]
        offset: [f32; 3],
    },
    Mirror {
        name: String,
        input: String,
        /// "x", "y" or "z".
        axis: String,
        #[serde(default)]
        offset: [f32; 3],
    },
    Material {
        name: String,
        input: String,
        #[serde(default)]
        base_color: Option<[f32; 4]>,
        #[serde(default)]
        roughness: Option<f32>,
        #[serde(default)]
        metalness: Option<f32>,
    },
    /// Baked edit (not an op): hide a part. Runs in batch position.
    HidePart { part: String },
    /// Baked edit (not an op): un-hide a part. Runs in batch position.
    ShowPart { part: String },
    /// Baked edit (not an op): rename a part. Runs in batch position.
    Rename {
        part: String,
        /// The new name.
        to: String,
    },
}

impl From<OpSpec> for BatchItem {
    fn from(spec: OpSpec) -> Self {
        match spec {
            OpSpec::Primitive {
                name,
                kind,
                size,
                translation,
                rotation,
            } => BatchItem::Op(Op::Primitive {
                name,
                kind,
                size,
                translation,
                rotation,
            }),
            OpSpec::Transform {
                name,
                input,
                translation,
                rotation,
                scale,
            } => BatchItem::Op(Op::Transform {
                name,
                input,
                translation,
                rotation,
                scale,
            }),
            OpSpec::Boolean { name, op, a, b } => BatchItem::Op(Op::Boolean { name, op, a, b }),
            OpSpec::RadialArray {
                name,
                input,
                count,
                center,
                degrees,
                start_degrees,
                axis,
            } => BatchItem::Op(Op::RadialArray {
                name,
                input,
                count,
                center,
                degrees,
                start_degrees,
                axis,
            }),
            OpSpec::LinearArray {
                name,
                input,
                count,
                offset,
            } => BatchItem::Op(Op::LinearArray {
                name,
                input,
                count,
                offset,
            }),
            OpSpec::Mirror {
                name,
                input,
                axis,
                offset,
            } => BatchItem::Op(Op::Mirror {
                name,
                input,
                axis,
                offset,
            }),
            OpSpec::Material {
                name,
                input,
                base_color,
                roughness,
                metalness,
            } => BatchItem::Op(Op::Material {
                name,
                input,
                base_color,
                roughness,
                metalness,
            }),
            OpSpec::HidePart { part } => BatchItem::Hide { part },
            OpSpec::ShowPart { part } => BatchItem::Show { part },
            OpSpec::Rename { part, to } => BatchItem::Rename { part, to },
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ApplyOpsArgs {
    /// Ops to apply in order, each tagged with its "type".
    ops: Vec<OpSpec>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SetCameraArgs {
    /// Horizontal angle in degrees (0 looks down -z).
    yaw: f32,
    /// Vertical angle in degrees, -89..89.
    pitch: f32,
    /// Distance from the orbit target.
    dist: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ExpectPartsArgs {
    /// The exact part names the model should have.
    parts: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ExpectBoundsArgs {
    /// Expected world-bounds minimum per axis; `null` skips that axis.
    min: [Option<f32>; 3],
    /// Expected world-bounds maximum per axis; `null` skips that axis.
    max: [Option<f32>; 3],
    /// Allowed deviation per checked axis. Default 0.01.
    #[serde(default = "default_tol")]
    tol: f32,
}

fn default_tol() -> f32 {
    0.01
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AgentShotArgs {
    /// Which view: "front", "back", "left", "right", "top", "bottom"
    /// (orthographic, auto-framed) or "iso" (perspective three-quarter).
    view: String,
    /// Pixel width. Default 1280.
    #[serde(default = "default_shot_width")]
    width: u32,
    /// Pixel height. Default 720.
    #[serde(default = "default_shot_height")]
    height: u32,
}

fn default_shot_width() -> u32 {
    1280
}

fn default_shot_height() -> u32 {
    720
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AgentViewsArgs {
    /// Views to render, e.g. ["front", "right", "top"] for a three-pane check
    /// or ["iso"] for the looks-right shot. Each returns its own image.
    views: Vec<String>,
    /// Pixel width per view. Default 1280.
    #[serde(default = "default_shot_width")]
    width: u32,
    /// Pixel height per view. Default 720.
    #[serde(default = "default_shot_height")]
    height: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SessionLogArgs {
    /// How many recent calls to show. Default 30, max 500.
    limit: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ProbeRayArgs {
    /// Ray origin, world space [x, y, z].
    origin: [f32; 3],
    /// Ray direction (normalized internally).
    dir: [f32; 3],
}

#[tool_router]
impl Tools {
    /// Show the whole model: name, version, parts, triangle counts, bounds and materials.
    /// Call this first (and after any confusing result) to ground yourself in the model.
    #[tool]
    async fn scene_summary(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.summary())).await
    }

    /// Add a named primitive part (box, sphere, cylinder, ...) with a size and a
    /// translation. The fastest way to block out a shape: add a few primitives, then
    /// refine with boolean ops.
    #[tool]
    async fn add_primitive(
        &self,
        Parameters(AddPrimitiveArgs {
            kind,
            size,
            translation,
            name,
        }): Parameters<AddPrimitiveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.add_primitive(&kind, size, translation, &name))
            .await
    }

    /// Add a part from raw triangle data: vertex positions and triangle indices, with
    /// optional normals and UVs (both are computed for you when omitted: auto normals
    /// and box projection). Use for shapes primitives and booleans cannot make.
    #[tool]
    async fn add_tri_mesh(
        &self,
        Parameters(AddTriMeshArgs {
            name,
            verts,
            indices,
            normals,
            uvs,
        }): Parameters<AddTriMeshArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.add_tri_mesh(&name, verts, indices, normals, uvs))
            .await
    }

    /// Boolean-combine two parts: "union" glues a and b, "difference" subtracts b from a
    /// (holes, cutouts), "intersection" keeps only the overlap. Both input parts are
    /// consumed and replaced by the result, optionally named.
    #[tool]
    async fn boolean(
        &self,
        Parameters(BooleanArgs { op, a, b, name }): Parameters<BooleanArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.boolean(&op, &a, &b, name.as_deref()))
            .await
    }

    /// Move, rotate (quaternion [x, y, z, w]) or scale one part. Fields left out stay
    /// as they are, so you can nudge just the translation.
    #[tool]
    async fn transform_part(
        &self,
        Parameters(TransformPartArgs {
            part,
            translation,
            rotation,
            scale,
        }): Parameters<TransformPartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.transform_part(&part, translation, rotation, scale))
            .await
    }

    /// Delete a part by name.
    #[tool]
    async fn delete_part(
        &self,
        Parameters(PartArgs { part }): Parameters<PartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.delete_part(&part)).await
    }

    /// Rename a part. Pick descriptive names; parts are addressed by name everywhere.
    #[tool]
    async fn rename_part(
        &self,
        Parameters(RenamePartArgs { part, to }): Parameters<RenamePartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.rename_part(&part, &to)).await
    }

    /// Set a part's material: base colour [r, g, b, a], roughness and metalness. Any
    /// field left out keeps its current value.
    #[tool]
    async fn set_material(
        &self,
        Parameters(SetMaterialArgs {
            part,
            base_color,
            roughness,
            metalness,
        }): Parameters<SetMaterialArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_material(&part, base_color, roughness, metalness))
            .await
    }

    /// Re-project a part's UVs with a named projection (e.g. "box", "planar").
    /// Use after reshaping a part when textures or exports need clean UVs.
    #[tool]
    async fn set_uvs(
        &self,
        Parameters(SetUvsArgs { part, projection }): Parameters<SetUvsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_uvs(&part, &projection)).await
    }

    /// Export the current model as glTF. Pass format "gltf" for a JSON .gltf or "glb"
    /// for a self-contained binary .glb, plus the output file path.
    #[tool]
    async fn export_model(
        &self,
        Parameters(ExportModelArgs { path, format }): Parameters<ExportModelArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = PathBuf::from(path);
        self.run(move |s| match format.as_str() {
            "glb" => gltf::export_glb(&s.model, &path)
                .map(|_| format!("exported `{}` to {}", s.model.name, path.display()))
                .map_err(|e| e.to_string()),
            "gltf" => gltf::export_gltf(&s.model, &path)
                .map(|_| format!("exported `{}` to {}", s.model.name, path.display()))
                .map_err(|e| e.to_string()),
            other => Err(format!(
                "unknown format `{other}` (use \"gltf\" or \"glb\")"
            )),
        })
        .await
    }

    /// Save the current model into the shared prefab catalog as JSON, recording a
    /// description, tags and the prompt that produced it. The model's own name is the
    /// catalog entry name (set it with set_model_name first if needed).
    #[tool]
    async fn save_prefab(
        &self,
        Parameters(SavePrefabArgs {
            description,
            tags,
            prompt,
        }): Parameters<SavePrefabArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
            catalog::save_prefab(&s.model, &description, &tag_refs, &prompt).map(
                |(name, overwrote)| {
                    if overwrote {
                        format!("saved `{name}` — overwrote an existing catalog entry")
                    } else {
                        format!("saved `{name}`")
                    }
                },
            )
        })
        .await
    }

    /// Load a prefab from the catalog by name, replacing the current model in the
    /// session (undoable). Use catalog_list to see what exists.
    #[tool]
    async fn load_prefab(
        &self,
        Parameters(CatalogNameArgs { name }): Parameters<CatalogNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let model = catalog::load_prefab(&name)?;
            let mut model = model;
            model.version = s.model.version + 1;
            let snapshot = s.model.clone();
            s.model.history.record(snapshot, format!("load `{name}`"));
            s.model = model;
            Ok(format!("loaded prefab `{name}`\n{}", s.summary()))
        })
        .await
    }

    /// Name the whole model (used in summaries, exports and as the prefab name).
    #[tool]
    async fn set_model_name(
        &self,
        Parameters(NameArgs { name }): Parameters<NameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            s.model.name = name.clone();
            let snapshot = s.model.clone();
            s.model.history.record(snapshot, format!("name `{name}`"));
            Ok(format!("model renamed to `{name}`"))
        })
        .await
    }

    /// List every entry in the shared prefab catalog: names, tags and descriptions.
    #[tool]
    async fn catalog_list(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|_s| catalog::list().map(|e| format_catalog(&e)))
            .await
    }

    /// Describe one catalog entry in full: description, tags and the prompt that made it.
    #[tool]
    async fn catalog_describe(
        &self,
        Parameters(CatalogNameArgs { name }): Parameters<CatalogNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |_s| catalog::describe(&name)).await
    }

    /// Grab the current preview render as a PNG image so you can see the model.
    /// Needs the windowed preview app; fails in --headless mode.
    #[tool]
    async fn screenshot(&self) -> Result<CallToolResult, ErrorData> {
        self.grab_screenshot().await
    }

    /// Undo the last mutation (parts, booleans, transforms, materials, renames...).
    #[tool]
    async fn undo(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| s.undo()).await
    }

    /// Redo: re-apply the most recent undone mutation (the newest child in the
    /// history tree). If you jumped to an older state and edited, redo follows
    /// the branch you are on.
    #[tool]
    async fn redo(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| s.redo()).await
    }

    /// Show the history tree: one line per snapshot with its id, parent, label
    /// and model version. The cursor marks where you are.
    #[tool]
    async fn history(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.history())).await
    }

    /// Jump to any snapshot in the history tree by id (from history). Useful to
    /// try an alternative from an earlier state: after jumping, the next edit
    /// branches — the old path stays reachable by jumping back.
    #[tool]
    async fn jump(
        &self,
        Parameters(HistoryIdArgs { id }): Parameters<HistoryIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.jump(id)).await
    }

    /// Add a primitive as a procedural graph step (upsert by output name —
    /// re-issuing an op with the same `name` replaces it, which is how you
    /// tweak a parameter). Prefer ops over baked parts: the graph re-derives.
    #[tool]
    async fn op_primitive(
        &self,
        Parameters(OpPrimitiveArgs {
            name,
            kind,
            size,
            translation,
            rotation,
        }): Parameters<OpPrimitiveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_primitive(&name, &kind, size, translation, rotation))
            .await
    }

    /// Place/rotate/scale an earlier part into a new output as a graph step.
    #[tool]
    async fn op_transform(
        &self,
        Parameters(OpTransformArgs {
            name,
            input,
            translation,
            rotation,
            scale,
        }): Parameters<OpTransformArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_transform(&name, &input, translation, rotation, scale))
            .await
    }

    /// Boolean-combine two earlier parts as a graph step.
    #[tool]
    async fn op_boolean(
        &self,
        Parameters(OpBooleanArgs { name, op, a, b }): Parameters<OpBooleanArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_boolean(&name, &op, &a, &b)).await
    }

    /// Copy an earlier part around a circle: `count` copies over `degrees`.
    /// Outputs are named name_0 .. name_{count-1}.
    #[tool]
    async fn op_radial_array(
        &self,
        Parameters(OpRadialArrayArgs {
            name,
            input,
            count,
            center,
            degrees,
            start_degrees,
            axis,
        }): Parameters<OpRadialArrayArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            s.op_radial_array(&name, &input, count, center, degrees, start_degrees, &axis)
        })
        .await
    }

    /// Re-material an earlier part as a graph step.
    #[tool]
    async fn op_material(
        &self,
        Parameters(OpMaterialArgs {
            name,
            input,
            base_color,
            roughness,
            metalness,
        }): Parameters<OpMaterialArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_material(&name, &input, base_color, roughness, metalness))
            .await
    }

    /// Copy an earlier part in a straight line as a graph step. The source
    /// counts as copy 0; outputs are name_1 .. name_{count-1}.
    #[tool]
    async fn op_linear_array(
        &self,
        Parameters(OpLinearArrayArgs {
            name,
            input,
            count,
            offset,
        }): Parameters<OpLinearArrayArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_linear_array(&name, &input, count, offset))
            .await
    }

    /// Mirror an earlier part across a plane as a graph step. The mesh is
    /// baked (verts reflected, winding flipped), so it renders correctly.
    #[tool]
    async fn op_mirror(
        &self,
        Parameters(OpMirrorArgs {
            name,
            input,
            axis,
            offset,
        }): Parameters<OpMirrorArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_mirror(&name, &input, &axis, offset))
            .await
    }

    /// Remove one graph step by its output name and re-derive the model.
    #[tool]
    async fn op_delete(
        &self,
        Parameters(OpNameArgs { name }): Parameters<OpNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_delete(&name)).await
    }

    /// List the procedural graph: one line per op with its inputs.
    #[tool]
    async fn ops(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.describe_ops())).await
    }

    /// The full parameters of one op (size, translation, colors, counts...),
    /// looked up by output part name. Use before an upsert tweak to see what
    /// the op currently looks like.
    #[tool]
    async fn op_get(
        &self,
        Parameters(OpNameArgs { name }): Parameters<OpNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.describe_op(&name)).await
    }

    /// Replace the session with a fresh empty model. The old model stays one
    /// undo away (recorded in the history tree), so this is the clean way to
    /// start the next model without restarting the server.
    #[tool]
    async fn new_model(
        &self,
        Parameters(NameArgs { name }): Parameters<NameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.new_model(&name)).await
    }

    /// Apply a whole batch in one call: an array of objects, each tagged
    /// with a "type" field — the graph ops ("primitive", "transform",
    /// "boolean", "radial_array", "linear_array", "mirror", "material")
    /// carrying the same parameters as the individual tools, plus the baked
    /// edits "hide_part" {"part"}, "show_part" {"part"} and "rename"
    /// {"part", "to"}. Applied in order with the usual upsert/rollback
    /// semantics; stops at the first error with per-item context. The result
    /// ends with the full scene summary.
    #[tool]
    async fn apply_ops(
        &self,
        Parameters(ApplyOpsArgs { ops }): Parameters<ApplyOpsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let items: Vec<BatchItem> = ops.into_iter().map(BatchItem::from).collect();
        self.run(move |s| s.apply_batch(items)).await
    }

    /// Move the orbit camera: yaw/pitch in degrees and the distance from the
    /// model center. Marks the camera user-steered (auto-drift stops). Windowed
    /// preview only.
    #[tool]
    async fn set_camera(
        &self,
        Parameters(SetCameraArgs { yaw, pitch, dist }): Parameters<SetCameraArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::SetCamera {
                yaw,
                pitch,
                dist,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        match Self::await_reply(reply_rx).await {
            Ok(_) => {}
            Err(e) => return Err(ErrorData::internal_error(e, None)),
        }
        Ok(CallToolResult::success(vec![Content::text(format!(
            "camera set to yaw {yaw}°, pitch {pitch}°, dist {dist}"
        ))]))
    }

    /// Hide a part: it stays in the model and keeps deriving from ops, but the
    /// preview and the glTF export skip it. The clean way to get boolean
    /// intermediates out of an export without breaking the graph.
    #[tool]
    async fn hide_part(
        &self,
        Parameters(PartArgs { part }): Parameters<PartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_visible(&part, false)).await
    }

    /// Un-hide a previously hidden part.
    #[tool]
    async fn show_part(
        &self,
        Parameters(PartArgs { part }): Parameters<PartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_visible(&part, true)).await
    }

    /// Mesh-level validation, no renderer needed: per-part edge manifoldness
    /// and winding (signed volume must be positive). Catches inverted normals
    /// and torn geometry that a screenshot only hints at.
    #[tool]
    async fn validate(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.validate())).await
    }

    /// Re-derives the op graph into a scratch copy and compares the geometry
    /// against the cached parts. Catches a stale parts cache or a
    /// non-deterministic op. Baked edits (set_material, visibility) are not
    /// compared — a refresh legitimately resets those.
    #[tool]
    async fn check_rebuild(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| s.check_rebuild()).await
    }

    /// Assert the model's part names exactly (order-insensitive). Fails with
    /// the missing and unexpected names, so you can deterministically verify
    /// "my gate is 14 parts named X..." instead of eyeballing a render.
    #[tool]
    async fn expect_parts(
        &self,
        Parameters(ExpectPartsArgs { parts }): Parameters<ExpectPartsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.expect_parts(&parts)).await
    }

    /// Assert the model's world bounds numerically — the deterministic form of
    /// "it sits on the ground", "it is centered", "the roof peaks at ~2.2".
    /// Per-axis expectations; `null` skips an axis. Over visible parts only.
    #[tool]
    async fn expect_bounds(
        &self,
        Parameters(ExpectBoundsArgs { min, max, tol }): Parameters<ExpectBoundsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.expect_bounds(min, max, tol)).await
    }

    /// Cast a ray through the scene and report the first part it hits (name,
    /// distance, point) or that it passes clean through. The numeric check for
    /// openings and occlusion: cast through a window hole to prove it is open,
    /// cast at a door fill to prove it is solid. Windowed preview only.
    #[tool]
    async fn probe_ray(
        &self,
        Parameters(ProbeRayArgs { origin, dir }): Parameters<ProbeRayArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::ProbeRay {
                origin,
                dir,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let text = match tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx).await {
            Ok(Ok(Ok(text))) => text,
            Ok(Ok(Err(e))) => return Err(ErrorData::internal_error(e, None)),
            Ok(Err(_)) => {
                return Err(ErrorData::internal_error(
                    "preview app dropped the probe",
                    None,
                ));
            }
            Err(_) => {
                return Err(ErrorData::internal_error(
                    "timed out after 15s waiting for the preview app - the render may be stuck; retry or restart the daemon",
                    None,
                ));
            }
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Render the model from the agent's own offscreen camera — independent of
    /// the human's window view, orthographic for the six cardinal views (true
    /// proportions, auto-framed to the bounds), perspective for "iso". Returns
    /// a PNG. Windowed preview only.
    #[tool]
    async fn agent_shot(
        &self,
        Parameters(AgentShotArgs {
            view,
            width,
            height,
        }): Parameters<AgentShotArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let view = parse_agent_view(&view).map_err(|e| ErrorData::internal_error(e, None))?;
        let png = self.agent_render(view, width, height).await?;
        Ok(CallToolResult::success(vec![
            Content::image(B64.encode(png.0), "image/png"),
            Content::text(png.1),
        ]))
    }

    /// Render several agent views in one call — e.g. ["front", "right", "top"]
    /// as a three-pane check, or ["front", "iso"] for proportions plus the
    /// looks-right shot. Returns one image per view, in order. Windowed
    /// preview only.
    #[tool]
    async fn agent_views(
        &self,
        Parameters(AgentViewsArgs {
            views,
            width,
            height,
        }): Parameters<AgentViewsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if views.is_empty() || views.len() > 6 {
            return Err(ErrorData::internal_error(
                "give 1..6 views, e.g. [\"front\", \"right\", \"top\"]",
                None,
            ));
        }
        let mut parsed = Vec::with_capacity(views.len());
        for v in &views {
            parsed.push(parse_agent_view(v).map_err(|e| ErrorData::internal_error(e, None))?);
        }
        let mut content = Vec::with_capacity(parsed.len() * 2);
        for (i, view) in parsed.iter().enumerate() {
            let (png, _) = self.agent_render(*view, width, height).await?;
            content.push(Content::text(format!("view {}: {view:?}", i + 1)));
            content.push(Content::image(B64.encode(png), "image/png"));
        }
        Ok(CallToolResult::success(content))
    }

    /// Add a human-readable comment pinned to the model. Face, vertex and position are
    /// all optional; when all three are omitted and the human has clicked a spot in the
    /// live view, that click is used automatically (so prefer omitting them when a pin
    /// is pending — the summary tells you when one is). Annotations persist in prefabs
    /// and show up in every scene_summary.
    #[tool]
    async fn add_annotation(
        &self,
        Parameters(AddAnnotationArgs {
            part,
            text,
            face,
            vertex,
            position,
        }): Parameters<AddAnnotationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.add_annotation(&part, &text, face, vertex, position))
            .await
    }

    /// Read every annotation on the model: one line per comment with its id, part,
    /// text and the face/vertex it is pinned to.
    #[tool]
    async fn list_annotations(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.list_annotations())).await
    }

    /// Delete one annotation by its id (from list_annotations).
    #[tool]
    async fn delete_annotation(
        &self,
        Parameters(AnnotationIdArgs { id }): Parameters<AnnotationIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.delete_annotation(id)).await
    }

    /// The recent call log: tool name, ok/error, duration in ms — so you can
    /// self-audit a build (what failed, what was slow) without leaving the
    /// server. Covers the last ~500 calls in this server process.
    #[tool]
    async fn session_log(
        &self,
        Parameters(SessionLogArgs { limit }): Parameters<SessionLogArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = limit.unwrap_or(30).min(500) as usize;
        let log = CALL_LOG.lock().unwrap_or_else(|e| e.into_inner());
        let start = log.len().saturating_sub(limit);
        let lines: Vec<String> = log[start..]
            .iter()
            .enumerate()
            .map(|(i, (tool, ok, ms))| {
                format!(
                    "#{} {} {} ({}ms)",
                    start + i + 1,
                    tool,
                    if *ok { "ok" } else { "ERROR" },
                    ms
                )
            })
            .collect();
        if lines.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "session log is empty",
            )]));
        }
        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }
}

/// The server-handler wiring: tool calls go through the generated router, and
/// `get_info` serves operating guidance in the MCP `initialize` response — the
/// standard slot for "how to drive this server", so every client lands it in
/// the model's context without anyone having to read AGENTS.md first.
#[tool_handler]
impl rmcp::ServerHandler for Tools {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        const INSTRUCTIONS: &str = "Operating guidance for this modeling server:
- Call scene_summary first (and after anything confusing): it shows parts, bounds, ops, annotations, plus warnings for known traps. An empty warnings list means none fired.
- Build with apply_ops (a whole model in one call), not one op per call. Ops are upserts: re-issuing the same name and kind tweaks parameters and re-derives. Evaluation is dependency-ordered, so op positions don't matter.
- Wire shapes are strict: size is ALWAYS [f32;3] (sphere radius = size[0], cylinder/cone/capsule radius = size[0] height = size[1]); base_color is ALWAYS RGBA [f32;4]. Wrong shapes fail deserialization with no field name.
- op_transform values are ABSOLUTE, not additive: a part at y=0.375 transformed with translation y=0 lands at y=0. Restate the full position in every copy. op_primitive also takes an optional rotation quat [x,y,z,w].
- hide_part/show_part take an argument named part; expect_parts takes parts; save_prefab requires prompt and warns when it overwrites an existing entry.
- apply_ops batches can also include baked edits: type hide_part/show_part with part, or rename with part and to - they run in batch position.
- radial_array takes an optional axis (y default, x or z for horizontal rings like windmill blades - no manual rotation needed).
- session_log shows the recent per-call record (tool, ok/error, ms) for self-auditing.
- Color parts with op_material BEFORE ops that copy them (arrays/mirror/transform clone the input, so later materials leave copies default-gray - the warnings section catches this).
- Full-ring radial arrays duplicate the source unless you pass count-1 copies with start_degrees = degrees/count.
- undo/redo/jump move a history tree cursor; jump restores that snapshot's op list exactly, so re-issue every op created after the jump point. Editing after a jump branches.
- Verify deterministically before screenshots: validate (manifold/winding), expect_parts (visible parts only), expect_bounds (visible bounds), check_rebuild (stale-cache diff), probe_ray (openings/occlusion; windowed preview only). Then screenshot or agent_shot/agent_views (independent offscreen camera; front/right/top/iso need no camera setup).
- Every mutation autosaves the session (parts, ops, history) and it is restored on restart; save_prefab only names and indexes it. Prefab names come from set_model_name.
- export_model skips hidden parts; hide boolean intermediates instead of deleting them (later ops may still derive from them).
- Render waits time out after 15s with an error; a stuck frame can't wedge the server.
Full details live in AGENTS.md next to the source.";
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new("modeler", "0.1.0"))
        .with_instructions(INSTRUCTIONS)
    }
}

fn format_catalog(entries: &[CatalogEntry]) -> String {
    if entries.is_empty() {
        return "catalog is empty".into();
    }
    let mut out = format!("{} catalog entries\n", entries.len());
    for e in entries {
        out += &format!(
            "- `{}`: {} [{}]\n",
            e.name,
            e.description,
            e.tags.join(", ")
        );
    }
    out
}
