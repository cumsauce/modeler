use crate::prelude::*;

/// A comment box is open: single-letter shortcuts (G, M, O) stay off.
#[derive(Resource, Default)]
pub(crate) struct PinTyping {
    pub(crate) open: bool,
}

/// The 3D view's mouse: hovering a part lights it (`Workbench.hover`), a
/// click selects it (empty space deselects), and while comments are open a
/// click (or a right-click any time) pins that spot instead and opens the
/// comment box on it (`CommentTarget::Spot`), drawn by [`place_pins`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn click_in_view(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    mut ray_cast: MeshRayCast,
    parts: Query<(Entity, &Name), With<PreviewPart>>,
    mut modeler: ResMut<Modeler>,
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
        // Empty space closes an empty box; typed text stays until Esc / ×.
        if comment.is_open() && empty_click_cancels(comment.draft()) {
            comment.cancel();
        }
        return;
    };
    let pin = PendingPin {
        part: name,
        face: hit.triangle_index.map(|i| i as u32),
        vertex: None,
        position: hit.point.to_array(),
    };
    // Agents can still consume the spot with an all-optional
    // `add_annotation`; the box keeps its own copy.
    modeler.session.pending_pin = Some(pin.clone());
    comment.request_spot(pin);
}

/// One badge per pin: a small numbered dot on the spot, with a label (the
/// comment's text) shown only while hovered.
struct PinBadge {
    /// `None` for the spot the open comment box is pinned to.
    id: Option<u32>,
    at: Vec3,
    dot: Entity,
    label: Entity,
}

/// The pins the window shows, rebuilt only when what they show changes.
#[derive(Resource, Default)]
pub(crate) struct Pins {
    key: Option<PinKey>,
    badges: Vec<PinBadge>,
    /// The camera last frame, to tell when it moves.
    last_cam: Option<Transform>,
    moving: bool,
    hovered: Option<usize>,
}

#[derive(PartialEq, Clone, Debug)]
struct PinKey {
    doc: String,
    /// Bumped by every annotation edit (add, resolve, delete).
    rev: u64,
    count: usize,
    /// The open box's spot, by its bits.
    spot: Option<[u32; 3]>,
}

/// Dot diameter, logical px.
const DOT: f32 = 16.0;

/// A label's text: the first line, cut to `max` characters.
pub(crate) fn pin_label(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max && !text.trim().contains('\n') {
        return line.to_string();
    }
    let cut: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Rebuilds the pin badges when the comments change (never per frame):
/// every comment on a part while comments are open, plus the open box's
/// spot. A model rebuild leaves them alone (they are UI, not scene).
pub(crate) fn sync_pins(
    modeler: Res<Modeler>,
    wb: Res<Workbench>,
    cb: Res<CommentBox>,
    mut pins: ResMut<Pins>,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    if modeler.session.viewer_away() {
        return;
    }
    let model = &modeler.session.model;
    let spot = cb.spot().map(|p| p.position.map(f32::to_bits));
    let key = (wb.show_comments || spot.is_some()).then(|| PinKey {
        doc: model.name.clone(),
        rev: modeler.session.annotation_rev,
        count: model.annotations.len(),
        spot,
    });
    if pins.key == key {
        return;
    }
    pins.key = key;
    pins.hovered = None;
    for b in pins.badges.drain(..) {
        commands.entity(b.dot).despawn();
    }
    if pins.key.is_none() {
        return;
    }
    let mut want: Vec<(Option<u32>, Vec3, String, bool)> = Vec::new();
    if wb.show_comments {
        for a in model.annotations.iter().filter(|a| a.material.is_none()) {
            want.push((
                Some(a.id),
                Vec3::from_array(a.position),
                pin_label(&a.text, 60),
                !a.is_open(),
            ));
        }
    }
    if let Some(p) = cb.spot() {
        want.push((
            None,
            Vec3::from_array(p.position),
            "new comment".into(),
            false,
        ));
    }
    for (id, at, text, resolved) in want {
        let (fill, ink, edge) = match (id, resolved) {
            (_, true) => (ds::RAISE, ds::DIM, ds::LINE),
            (Some(_), false) => (ds::ACCENT, ds::ON_ACCENT, ds::BG),
            (None, _) => (ds::BG.with_alpha(0.85), ds::ACCENT, ds::ACCENT),
        };
        let dot = commands
            .spawn((
                Name::new("pin"),
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Px(DOT),
                    height: Val::Px(DOT),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border: UiRect::all(Val::Px(1.5)),
                    border_radius: BorderRadius::all(Val::Px(DOT / 2.0)),
                    ..default()
                },
                BackgroundColor(fill),
                BorderColor::all(edge),
                Visibility::Hidden,
                GlobalZIndex(5),
                Pickable::IGNORE,
            ))
            .id();
        commands.spawn((
            ds::text_in(
                &assets,
                ds::Type::Meta,
                id.map_or("+".into(), |i| i.to_string()),
                ink,
            ),
            Pickable::IGNORE,
            ChildOf(dot),
        ));
        // The label: right of the dot, hidden until hovered.
        let label = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(DOT + 4.0),
                    top: Val::Px(-2.0),
                    padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(ds::CHIP_BG),
                Visibility::Hidden,
                Pickable::IGNORE,
                ChildOf(dot),
            ))
            .id();
        commands.spawn((
            ds::text_in(&assets, ds::Type::Small, text, ds::FG),
            Pickable::IGNORE,
            ChildOf(label),
        ));
        pins.badges.push(PinBadge { id, at, dot, label });
    }
}

/// Where a world point lands in the window (logical px), from the camera's
/// own `Transform` (it has no parent), so it is this frame's camera, not
/// last frame's `GlobalTransform`: the dots stay glued to their spots while
/// the camera moves.
fn project(camera: &Camera, cam: &Transform, rect: Rect, scale: f32, at: Vec3) -> Option<Vec2> {
    let p = camera
        .world_to_viewport(&GlobalTransform::from(*cam), at)
        .ok()?;
    Some(rect.min + p / scale).filter(|p| rect.contains(*p))
}

/// Puts every pin dot on its spot each frame, after the camera moved
/// (`orbit_camera`) and the layout settled (`apply_layout`), so the UI
/// layout later this frame draws them where the 3D image is. While the
/// camera moves, the dots dim and labels hide; hovering a dot shows its
/// label.
#[allow(clippy::type_complexity)]
pub(crate) fn place_pins(
    mut pins: ResMut<Pins>,
    cameras: Query<(&Camera, &Transform), With<MainCamera>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    view: Res<View3d>,
    mut nodes: Query<(&mut Node, &mut Visibility, &mut BackgroundColor)>,
) {
    if pins.badges.is_empty() {
        pins.last_cam = None;
        return;
    }
    let Ok((camera, cam)) = cameras.single() else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let scale = window.scale_factor();
    let cursor = window.cursor_position();
    let moving = pins.last_cam.is_some_and(|t| t != *cam);
    pins.last_cam = Some(*cam);
    let moved_state = moving != pins.moving;
    pins.moving = moving;
    // The dot under the cursor (nearest within the dot's radius + slack).
    let mut hovered = None;
    let mut best = (DOT / 2.0 + 3.0).powi(2);
    let mut spots = Vec::with_capacity(pins.badges.len());
    for (i, b) in pins.badges.iter().enumerate() {
        let s = project(camera, cam, view.rect, scale, b.at);
        if let (Some(p), Some(c)) = (s, cursor) {
            let d = p.distance_squared(c);
            if d < best {
                best = d;
                hovered = Some(i);
            }
        }
        spots.push(s);
    }
    if moving {
        hovered = None;
    }
    let hover_changed = hovered != pins.hovered;
    pins.hovered = hovered;
    for (i, (b, s)) in pins.badges.iter().zip(spots).enumerate() {
        if let Ok((mut node, mut vis, mut bg)) = nodes.get_mut(b.dot) {
            match s {
                Some(p) => {
                    let (l, t) = (Val::Px(p.x - DOT / 2.0), Val::Px(p.y - DOT / 2.0));
                    if node.left != l || node.top != t {
                        node.left = l;
                        node.top = t;
                    }
                    vis.set_if_neq(Visibility::Inherited);
                }
                None => {
                    vis.set_if_neq(Visibility::Hidden);
                }
            }
            if moved_state {
                let a = if moving { 0.45 } else { 1.0 };
                let c = bg.0;
                bg.0 = c.with_alpha(if b.id.is_none() { 0.85 * a } else { a });
            }
        }
        if (hover_changed || moved_state)
            && let Ok((_, mut vis, _)) = nodes.get_mut(b.label)
        {
            vis.set_if_neq(if hovered == Some(i) {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pin_label;

    #[test]
    fn labels_are_one_short_line() {
        assert_eq!(pin_label("short", 60), "short");
        assert_eq!(pin_label("first line\nsecond", 60), "first line…");
        let long = "a".repeat(80);
        let l = pin_label(&long, 20);
        assert_eq!(l.chars().count(), 20);
        assert!(l.ends_with('…'));
    }
}
