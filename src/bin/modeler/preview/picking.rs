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

/// The 3D view's mouse: hovering a part lights it (`Workbench.hover`), a
/// click selects it (empty space deselects), and while comments are open a
/// click (or a right-click any time) pins that spot instead: a badge, and a `PendingPin` for the comment box to fill in.
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
    // A right-click (not a right-drag look) pins a comment on that spot,
    // whether or not comments are open.
    let right = mouse.just_released(MouseButton::Right) && drag.moved <= 6.0;
    let clicked = clicked || right;
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
    if !wb.show_comments && !right {
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
    mut shown: Local<Option<(u64, usize, usize, String)>>,
) {
    if modeler.session.viewer_away() {
        return;
    }
    let model = &modeler.session.model;
    let want = wb.show_comments.then(|| {
        (
            model.version,
            model.annotations.len(),
            model.annotations.iter().filter(|a| !a.is_open()).count(),
            model.name.clone(),
        )
    });
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
    for a in model.annotations.iter().filter(|a| a.material.is_none()) {
        let pin = spawn_pin(&mut commands, a.id, Vec3::from_array(a.position));
        commands.entity(pin).insert(PinCommitted);
        if !a.is_open() {
            commands.entity(pin).insert(PinResolved);
        }
    }
}

/// Spawns pin markers requested by [`pin_on_click`].
pub(crate) fn spawn_pin_markers(mut spawn: ResMut<PendingPinSpawn>, mut commands: Commands) {
    let Some((point, id)) = spawn.point.take() else {
        return;
    };
    spawn_pin(&mut commands, id, point);
}

/// Spawn a pin marker (sphere + number label) at `point`.
pub(crate) fn spawn_pin(commands: &mut Commands, id: u32, point: Vec3) -> Entity {
    // An anchor in the world; `pin_badges` draws it as a flat badge on screen.
    commands
        .spawn((
            Name::new(format!("pin #{id}")),
            AnnotationPin,
            PinId(id),
            Transform::from_translation(point),
        ))
        .id()
}

/// The comment number a pin shows.
#[derive(Component)]
pub(crate) struct PinId(pub(crate) u32);

/// A pin whose comment is resolved: drawn greyed out.
#[derive(Component)]
pub(crate) struct PinResolved;

/// The on-screen badge drawn for a pin.
#[derive(Component)]
pub(crate) struct PinBadge(Entity);

/// Draws every pin as a numbered badge over the 3D view (a circle in the
/// accent colour with a small tail pointing at the spot; a pin still waiting
/// for its comment is a hollow ring), kept on its spot as the camera moves
/// and hidden when the spot is behind the camera or outside the view.
pub(crate) fn pin_badges(
    pins: Query<
        (
            Entity,
            &GlobalTransform,
            &PinId,
            Has<PinCommitted>,
            Has<PinResolved>,
        ),
        With<AnnotationPin>,
    >,
    mut badges: Query<(Entity, &PinBadge, &mut Node, &mut Visibility)>,
    cameras: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    view: Res<View3d>,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    const D: f32 = 22.0;
    let scale = windows.single().map(|w| w.scale_factor()).unwrap_or(1.0);
    let cam = cameras.single().ok();
    let mut have = std::collections::HashSet::new();
    for (e, badge, mut node, mut vis) in &mut badges {
        let Ok((_, at, ..)) = pins.get(badge.0) else {
            commands.entity(e).despawn();
            continue;
        };
        have.insert(badge.0);
        let spot = cam.and_then(|(c, ct)| c.world_to_viewport(ct, at.translation()).ok());
        let spot = spot
            .map(|p| view.rect.min + p / scale)
            .filter(|p| view.rect.contains(*p));
        match spot {
            Some(p) => {
                node.left = Val::Px(p.x - D / 2.0);
                node.top = Val::Px(p.y - D - 6.0);
                if *vis != Visibility::Inherited {
                    *vis = Visibility::Inherited;
                }
            }
            None => {
                if *vis != Visibility::Hidden {
                    *vis = Visibility::Hidden;
                }
            }
        }
    }
    for (pin, _, id, committed, resolved) in &pins {
        if have.contains(&pin) {
            continue;
        }
        let (fill, ink, edge) = match (committed, resolved) {
            (_, true) => (ds::RAISE, ds::DIM, ds::LINE),
            (true, false) => (ds::ACCENT, ds::ON_ACCENT, ds::FG),
            (false, _) => (ds::BG.with_alpha(0.85), ds::ACCENT, ds::ACCENT),
        };
        let badge = commands
            .spawn((
                Name::new("pin badge"),
                PinBadge(pin),
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Px(D),
                    height: Val::Px(D),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(D / 2.0)),
                    ..default()
                },
                BackgroundColor(fill),
                BorderColor::all(edge),
                BoxShadow::new(
                    Color::BLACK.with_alpha(0.45),
                    Val::Px(0.0),
                    Val::Px(2.0),
                    Val::Px(0.0),
                    Val::Px(6.0),
                ),
                Visibility::Hidden,
                GlobalZIndex(5),
                Pickable::IGNORE,
            ))
            .id();
        commands.spawn((
            ds::text_in(
                &assets,
                ds::Type::Small,
                &if committed {
                    id.0.to_string()
                } else {
                    "+".into()
                },
                ink,
            ),
            Pickable::IGNORE,
            ChildOf(badge),
        ));
        // The tail: a short stem from the badge down to the spot.
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(D - 2.0),
                left: Val::Px(D / 2.0 - 1.0),
                width: Val::Px(2.0),
                height: Val::Px(7.0),
                ..default()
            },
            BackgroundColor(edge),
            Pickable::IGNORE,
            ChildOf(badge),
        ));
    }
}
