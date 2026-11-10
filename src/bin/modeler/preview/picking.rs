use crate::prelude::*;

/// A pin sphere marking where the human clicked (or where an annotation sits).
#[derive(Component)]
pub(crate) struct AnnotationPin;

/// A pin whose comment has been typed in and committed.
#[derive(Component)]
pub(crate) struct PinCommitted;

/// The text being typed for the pending pin, shown in the bottom HUD line.
#[derive(Resource, Default)]
pub(crate) struct PinTyping {
    pub(crate) text: String,
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

/// Typing for the pending pin: characters collect into a buffer shown at the bottom
/// of the window; Enter commits the annotation, Escape cancels the text, Backspace
/// deletes a character. Always on (no focus to track), but inert without a pin.
#[allow(clippy::type_complexity)]
pub(crate) fn pin_typing(
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
