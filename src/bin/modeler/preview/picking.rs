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

/// The 3D view's mouse: hovering a part lights it (`Workbench.hover`), a
/// click selects it (empty space deselects), and while comments are open a
/// click pins that spot instead: a small bright sphere with the annotation
/// id it would get, and a `PendingPin` for the comment box to fill in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn click_in_view(
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
    mut wb: ResMut<Workbench>,
    mut comment: ResMut<CommentBox>,
    view: Res<View3d>,
    mut last: Local<Option<Vec2>>,
) {
    let cursor = windows.single().ok().and_then(|w| {
        w.cursor_position()
            .filter(|c| view.rect.contains(*c))
            .map(|c| (c, (c - view.rect.min) * w.scale_factor()))
    });
    let clicked = mouse.just_released(MouseButton::Left) && drag.moved <= 6.0;
    let Some((at, cursor)) = cursor.filter(|_| !wb.ui_hover && drag.button.is_none() || clicked)
    else {
        if wb.hover.is_some() {
            wb.hover = None;
        }
        *last = None;
        return;
    };
    // Hover only re-casts when the cursor moved (or something was clicked).
    if !clicked && *last == Some(at) {
        return;
    }
    *last = Some(at);
    let Ok((camera, cam_transform)) = cameras.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_transform, cursor) else {
        return;
    };
    let filter = |e| parts.contains(e);
    let settings = MeshRayCastSettings::default().with_filter(&filter);
    let hit = ray_cast
        .cast_ray(ray, &settings)
        .first()
        .and_then(|(e, hit)| {
            parts
                .get(*e)
                .ok()
                .map(|(_, n)| (n.to_string(), hit.clone()))
        });
    let name = hit.as_ref().map(|(n, _)| n.clone());
    if wb.hover != name {
        wb.hover = name.clone();
    }
    if !clicked || wb.ui_hover {
        return;
    }
    if !wb.show_comments {
        if wb.selected != name {
            wb.selected = name;
        }
        return;
    }
    let Some((name, hit)) = hit else {
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
    comment.request_spot(name.clone());
    modeler.session.pending_pin = Some(PendingPin {
        part: name,
        face: hit.triangle_index.map(|i| i as u32),
        vertex: None,
        position: hit.point.to_array(),
    });
}

/// While comments are open, every comment on a part shows as a numbered
/// pin where it was left (rebuilt after each model change, which clears
/// the scene's pins); closed, only an uncommitted pin stays.
pub(crate) fn sync_pins(
    modeler: Res<Modeler>,
    wb: Res<Workbench>,
    pins: Query<(Entity, Option<&PinCommitted>), With<AnnotationPin>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut shown: Local<Option<(u64, usize, String)>>,
) {
    let model = &modeler.session.model;
    let want = wb
        .show_comments
        .then(|| (model.version, model.annotations.len(), model.name.clone()));
    let have = pins.iter().any(|(_, c)| c.is_some());
    if *shown == want && (have || want.is_none() || model.annotations.is_empty()) {
        return;
    }
    *shown = want.clone();
    for (e, committed) in &pins {
        if committed.is_some() {
            commands.entity(e).despawn();
        }
    }
    if want.is_none() {
        return;
    }
    let radius = pin_radius(model);
    for a in model.annotations.iter().filter(|a| a.material.is_none()) {
        let pin = spawn_pin(
            &mut commands,
            &mut meshes,
            &mut materials,
            radius,
            a.id,
            Vec3::from_array(a.position),
        );
        commands.entity(pin).insert(PinCommitted);
    }
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
