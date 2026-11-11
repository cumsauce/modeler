use crate::prelude::*;

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
                    // Same convention as agent custom cameras: yaw 0 = on +z looking -z.
                    orbit.yaw = (90.0 - yaw).to_radians();
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
                style,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no renderer, no agent view".into()));
                } else {
                    agent_shots
                        .0
                        .push((view, width, height, focus, style, reply));
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
        orbit.scene_radius = radius.max(0.01);
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

/// Camera and lights (no floor: the camera may orbit below the model).
pub(crate) fn setup_scene(mut commands: Commands) {
    // AmbientLight is a per-camera component in Bevy 0.20 (a standalone
    // entity does nothing), so faces turned from the sun went black. A dim
    // camera-aligned fill keeps every side readable as the human orbits.
    // UI gets its own full-window camera, so the 3D camera can be a pane.
    commands.spawn((
        Name::new("ui camera"),
        Camera2d,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        IsDefaultUiCamera,
    ));
    commands
        .spawn((
            Name::new("camera"),
            MainCamera,
            Camera3d::default(),
            AmbientLight {
                brightness: 900.0,
                ..default()
            },
            Transform::from_translation(Vec3::new(4.0, 3.0, 6.0)).looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .with_children(|c| {
            c.spawn((
                Name::new("view fill"),
                DirectionalLight {
                    illuminance: 1500.0,
                    ..default()
                },
                Transform::default(),
            ));
        });
    commands.spawn((
        Name::new("sun"),
        DirectionalLight {
            illuminance: 8000.0,
            shadow_maps_enabled: true,
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
    commands
        .spawn((
            Name::new("graph hud"),
            GraphHud,
            Visibility::Hidden,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(50.0),
                height: Val::Percent(100.0),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.06, 0.08)),
        ))
        .with_children(|p| {
            p.spawn((
                GraphCanvas,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
            ));
            p.spawn((
                GraphFooter,
                Text::new(String::new()),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(Color::srgba(0.8, 0.83, 0.88, 0.8)),
                Node {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(0.0),
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.03, 0.035, 0.05, 0.97)),
                GlobalZIndex(1),
            ));
        });
}
