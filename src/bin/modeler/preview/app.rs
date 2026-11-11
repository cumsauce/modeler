use crate::prelude::*;
use bevy::camera::visibility::RenderLayers;

#[derive(Resource)]
pub(crate) struct Modeler {
    pub(crate) session: Session,
    pub(crate) bridge: Arc<Bridge>,
}

#[derive(Resource)]
pub(crate) struct Headless(pub(crate) bool);

pub(crate) fn windowed(headless: Option<Res<Headless>>) -> bool {
    !headless.map(|h| h.0).unwrap_or(false)
}

#[derive(Component)]
pub(crate) struct PreviewPart;

/// Which model version the scene was last built from.
#[derive(Resource, Default)]
pub(crate) struct SceneBuilt {
    pub(crate) version: Option<u64>,
}

/// Exits headless mode once the MCP server has hung up; windowed mode stays open so
/// the human can keep looking at the model.
pub(crate) fn quit_when_server_done(
    headless: bool,
) -> impl FnMut(Res<Modeler>, MessageWriter<AppExit>) {
    move |modeler, mut exit| {
        if headless && modeler.bridge.quit() {
            exit.write(AppExit::Success);
        }
    }
}

/// Drains MCP commands every frame and executes them against the session.
pub(crate) fn drain_commands(
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
            Cmd::ProbeRay {
                origin,
                dir,
                all_hits,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no scene to ray-cast".into()));
                } else {
                    probes.0.push(Some((origin, dir, all_hits, reply)));
                }
            }
            Cmd::AgentShot {
                view,
                width,
                height,
                focus,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no renderer, no agent view".into()));
                } else {
                    agent_shots.0.push((view, width, height, focus, reply));
                }
            }
        }
    }
}

/// Rebuilds one entity per part whenever the model version moves, and re-frames the
/// orbit camera around the model's bounds.
pub(crate) fn rebuild_scene(
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

pub(crate) fn part_bundle(
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

/// Camera, lights and the grey ground slab the model sits on.
pub(crate) fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Name::new("camera"),
        MainCamera,
        Camera3d::default(),
        // Layer 1 holds the grey floor: only the human window renders it.
        RenderLayers::from_layers(&[0, 1]),
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
        RenderLayers::from_layers(&[0, 1]),
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
        RenderLayers::layer(1),
    ));
}
