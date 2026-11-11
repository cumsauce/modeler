use crate::prelude::*;

/// A pin sphere marking where the human clicked (or where an annotation sits).
#[derive(Component)]
pub(crate) struct AnnotationPin;

/// A pin whose comment has been typed in and committed.
#[derive(Component)]
pub(crate) struct PinCommitted;

/// A comment box is open: single-letter shortcuts (G, M, O) stay off.
#[derive(Resource, Default)]
pub(crate) struct PinTyping {
    pub(crate) open: bool,
}

/// A click hit to turn into a pin marker, deferred to a spawn system because
/// `MeshRayCast` already borrows `Assets<Mesh>` immutably.
#[derive(Resource, Default)]
pub(crate) struct PendingPinSpawn {
    pub(crate) point: Option<(Vec3, u32)>,
}

/// Pin radius: about 2% of the model bounds diagonal, so it reads at any size.
pub(crate) fn pin_radius(model: &Model) -> f32 {
    model
        .bounds()
        .map(|(min, max)| ((max - min).length() * 0.02).max(0.02))
        .unwrap_or(0.05)
}

/// A click on a part pins a spot: a small bright sphere with the annotation id it
/// would get, and a `PendingPin` on the session for the next comment to fill in.
pub(crate) fn pin_on_click(
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
    panel: Res<GraphPanel>,
    mat_pane: Res<MatPane>,
    outline: Res<OutlinePane>,
    mut comment: ResMut<CommentBox>,
) {
    if panel.hover || mat_pane.hover || outline.hover {
        return;
    }
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
    // The cursor is window-relative; the 3D view may be a pane (graph open).
    let cursor = cursor - camera.logical_viewport_rect().map_or(Vec2::ZERO, |r| r.min);
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
    comment.request_spot(name.to_string());
    modeler.session.pending_pin = Some(PendingPin {
        part: name.to_string(),
        face: hit.triangle_index.map(|i| i as u32),
        vertex: None,
        position: hit.point.to_array(),
    });
}

/// Spawns pin markers requested by [`pin_on_click`].
pub(crate) fn spawn_pin_markers(
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
pub(crate) fn spawn_pin(
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
