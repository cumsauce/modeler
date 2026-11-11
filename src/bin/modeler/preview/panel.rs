use crate::prelude::*;

/// The bottom UI line showing the pin's typed-so-far comment.
#[derive(Component)]
pub(crate) struct TypingHud;

/// The left node-graph panel (toggled with G): one box per op, edges from
/// inputs to readers, laid out by a small spring simulation.
#[derive(Component)]
pub(crate) struct GraphHud;

#[derive(Resource, Default)]
pub(crate) struct GraphHudVisible(pub(crate) bool);

/// The node canvas inside the panel (boxes and edges are rebuilt under it).
#[derive(Component)]
pub(crate) struct GraphCanvas;

#[derive(Component)]
pub(crate) struct GraphFooter;

#[derive(Component)]
pub(crate) struct GraphBox(usize);

#[derive(Component)]
pub(crate) struct GraphEdge(usize, usize);

/// Panel state: the view, its layout, the camera over it and the selection.
#[derive(Resource, Default)]
pub(crate) struct GraphPanel {
    version: Option<u64>,
    view: modeler::graph_view::GraphView,
    layout: modeler::graph_view::Layout,
    zoom: f32,
    pan: Vec2,
    /// The human zoomed or panned: stop auto-fitting.
    user_view: bool,
    drag: Option<Vec2>,
    pub(crate) selected: Option<usize>,
    /// Cursor is over the panel: the 3D camera and pins ignore the mouse.
    pub(crate) hover: bool,
}

const BOX_W: f32 = 170.0;
const BOX_H: f32 = 40.0;

fn kind_color(kind: &str) -> Color {
    match kind {
        "primitive" | "extrude" | "revolve" | "sweep" | "prefab" => Color::srgb(0.16, 0.32, 0.55),
        "boolean" => Color::srgb(0.55, 0.18, 0.18),
        "radial" | "linear" | "mirror" => Color::srgb(0.16, 0.45, 0.25),
        "group" | "hull" | "round" => Color::srgb(0.45, 0.30, 0.55),
        _ => Color::srgb(0.48, 0.40, 0.14),
    }
}

/// G toggles the node panel. Rebuilds the boxes when the model changes, steps
/// the spring layout every frame, auto-fits the view (until the human zooms
/// with the wheel or pans with a right/middle drag), and a left click on a box
/// selects it so its parts glow in the 3D view.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_graph_hud(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut visible: ResMut<GraphHudVisible>,
    mut panel: ResMut<GraphPanel>,
    modeler: Res<Modeler>,
    typing: Res<PinTyping>,
    mut hud: Query<(&mut Visibility, &ComputedNode), With<GraphHud>>,
    canvas: Query<Entity, With<GraphCanvas>>,
    mut footer: Query<&mut Text, With<GraphFooter>>,
    mut boxes: Query<
        (&GraphBox, &mut Node, &mut UiTransform, &mut BorderColor),
        Without<GraphEdge>,
    >,
    mut edges: Query<(&GraphEdge, &mut Node, &mut UiTransform), Without<GraphBox>>,
    mut commands: Commands,
) {
    if keys.just_pressed(KeyCode::KeyG) && typing.text.is_empty() {
        visible.0 = !visible.0;
        panel.user_view = false;
    }
    let Ok((mut vis, computed)) = hud.single_mut() else {
        return;
    };
    *vis = if visible.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if !visible.0 {
        panel.hover = false;
        return;
    }
    let size = computed.size() * computed.inverse_scale_factor();
    let model = &modeler.session.model;
    let panel = &mut *panel;
    if panel.version != Some(model.version) {
        panel.version = Some(model.version);
        panel.view = modeler::graph_view::build(model);
        panel.layout.sync(&panel.view);
        if panel.selected.is_some_and(|s| s >= panel.view.nodes.len()) {
            panel.selected = None;
        }
        let Ok(canvas) = canvas.single() else {
            return;
        };
        commands.entity(canvas).despawn_children();
        commands.entity(canvas).with_children(|c| {
            for &(a, b) in &panel.view.edges {
                c.spawn((
                    GraphEdge(a, b),
                    Node {
                        position_type: PositionType::Absolute,
                        height: Val::Px(2.0),
                        ..default()
                    },
                    UiTransform::IDENTITY,
                    BackgroundColor(Color::srgba(0.8, 0.85, 0.9, 0.45)),
                ));
            }
            for (i, n) in panel.view.nodes.iter().enumerate() {
                let alpha = if n.dimmed { 0.35 } else { 0.95 };
                let sub = if n.badges.is_empty() {
                    n.kind.to_string()
                } else {
                    format!("{}  +{}", n.kind, n.badges.join(" +"))
                };
                c.spawn((
                    GraphBox(i),
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Px(BOX_W),
                        height: Val::Px(BOX_H),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::Center,
                        padding: UiRect::horizontal(Val::Px(6.0)),
                        border: UiRect::all(Val::Px(2.0)),
                        overflow: Overflow::clip(),
                        border_radius: BorderRadius::all(Val::Px(5.0)),
                        ..default()
                    },
                    UiTransform::IDENTITY,
                    BorderColor::all(Color::NONE),
                    BackgroundColor(kind_color(n.kind).with_alpha(alpha)),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new(n.name.clone()),
                        TextFont {
                            font_size: FontSize::Px(13.0),
                            ..default()
                        },
                        TextColor(Color::WHITE.with_alpha(if n.dimmed { 0.6 } else { 1.0 })),
                    ));
                    b.spawn((
                        Text::new(sub),
                        TextFont {
                            font_size: FontSize::Px(10.0),
                            ..default()
                        },
                        TextColor(Color::srgba(0.85, 0.88, 0.92, 0.75)),
                    ));
                });
            }
        });
        if let Ok(mut t) = footer.single_mut() {
            let h = &model.history;
            t.0 = format!(
                "{} ops, {} boxes{} | history {} nodes, at {}\nwheel zoom | right-drag pan | click a box",
                model.ops.len(),
                panel.view.nodes.len(),
                if panel.view.global_moves.is_empty() {
                    String::new()
                } else {
                    format!(
                        " | whole-model move: {}",
                        panel.view.global_moves.join(", ")
                    )
                },
                h.nodes.len(),
                h.current.map_or("-".to_string(), |c| format!("#{c}")),
            );
        }
    }
    panel.layout.step(&panel.view, time.delta_secs());

    // Graph bounds -> auto-fit.
    let mut lo = Vec2::splat(f32::MAX);
    let mut hi = Vec2::splat(f32::MIN);
    for n in &panel.view.nodes {
        if let Some(p) = panel.layout.pos.get(&n.key) {
            let p = Vec2::from_array(*p);
            lo = lo.min(p - Vec2::new(BOX_W, BOX_H) * 0.5);
            hi = hi.max(p + Vec2::new(BOX_W, BOX_H) * 0.5);
        }
    }
    let avail = size - Vec2::new(40.0, 90.0);
    if !panel.user_view && lo.x <= hi.x {
        let ext = (hi - lo).max(Vec2::ONE);
        panel.zoom = (avail.x / ext.x).min(avail.y / ext.y).clamp(0.2, 1.3);
        panel.pan = -(lo + hi) * 0.5;
    }
    if panel.zoom <= 0.0 {
        panel.zoom = 1.0;
    }
    let origin = Vec2::new(size.x * 0.5, (size.y - 50.0) * 0.5);
    let zoom = panel.zoom;
    let pan = panel.pan;
    let to_screen = |p: Vec2| origin + (p + pan) * zoom;

    // Mouse.
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    panel.hover = cursor.is_some_and(|c| c.x < size.x && c.y < size.y);
    if let Some(c) = cursor.filter(|_| panel.hover) {
        if scroll.delta.y != 0.0 {
            let before = (c - origin) / zoom - pan;
            panel.zoom = (zoom * (1.0 + 0.12 * scroll.delta.y)).clamp(0.1, 4.0);
            panel.pan = (c - origin) / panel.zoom - before;
            panel.user_view = true;
        }
        if mouse.just_pressed(MouseButton::Right) || mouse.just_pressed(MouseButton::Middle) {
            panel.drag = Some(c);
        }
        if mouse.just_pressed(MouseButton::Left) {
            let g = (c - origin) / zoom - pan;
            panel.selected = panel.view.nodes.iter().position(|n| {
                panel.layout.pos.get(&n.key).is_some_and(|p| {
                    let d = (g - Vec2::from_array(*p)).abs();
                    d.x <= BOX_W * 0.5 && d.y <= BOX_H * 0.5
                })
            });
        }
    }
    if let (Some(last), Some(c)) = (panel.drag, cursor) {
        if mouse.pressed(MouseButton::Right) || mouse.pressed(MouseButton::Middle) {
            panel.pan += (c - last) / panel.zoom;
            panel.drag = Some(c);
            panel.user_view = true;
        } else {
            panel.drag = None;
        }
    }

    for (b, mut node, mut tf, mut border) in &mut boxes {
        let Some(n) = panel.view.nodes.get(b.0) else {
            continue;
        };
        let p = panel.layout.pos.get(&n.key).copied().unwrap_or([0.0, 0.0]);
        let s = to_screen(Vec2::from_array(p));
        node.left = Val::Px(s.x - BOX_W * 0.5);
        node.top = Val::Px(s.y - BOX_H * 0.5);
        let glow = panel
            .layout
            .glow
            .get(&n.key)
            .copied()
            .unwrap_or(0.0)
            .min(1.0);
        tf.scale = Vec2::splat(zoom * (1.0 + 0.12 * glow));
        *border = BorderColor::all(if panel.selected == Some(b.0) {
            Color::srgb(1.0, 0.85, 0.2)
        } else {
            Color::WHITE.with_alpha(glow)
        });
    }
    for (e, mut node, mut tf) in &mut edges {
        let (Some(a), Some(b)) = (panel.view.nodes.get(e.0), panel.view.nodes.get(e.1)) else {
            continue;
        };
        let pa = Vec2::from_array(panel.layout.pos.get(&a.key).copied().unwrap_or([0.0, 0.0]));
        let pb = Vec2::from_array(panel.layout.pos.get(&b.key).copied().unwrap_or([0.0, 0.0]));
        let sa = to_screen(pa + Vec2::new(BOX_W * 0.5, 0.0));
        let sb = to_screen(pb - Vec2::new(BOX_W * 0.5, 0.0));
        let d = sb - sa;
        let len = d.length();
        let mid = (sa + sb) * 0.5;
        node.width = Val::Px(len);
        node.left = Val::Px(mid.x - len * 0.5);
        node.top = Val::Px(mid.y - 1.0);
        tf.rotation = Rot2::radians(d.y.atan2(d.x));
    }
}

/// The selected box's parts pulse in the 3D view.
pub(crate) fn highlight_selected(
    time: Res<Time>,
    panel: Res<GraphPanel>,
    visible: Res<GraphHudVisible>,
    parts: Query<(&Name, &MeshMaterial3d<StandardMaterial>), With<PreviewPart>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let sel: &[String] = match panel.selected.filter(|_| visible.0) {
        Some(i) => panel.view.nodes.get(i).map_or(&[], |n| &n.outputs),
        None => &[],
    };
    let pulse = 0.35 + 0.25 * (time.elapsed_secs() * 5.0).sin();
    for (name, mat) in &parts {
        let want = if sel.iter().any(|s| s == name.as_str()) {
            LinearRgba::rgb(pulse, pulse * 0.8, 0.1)
        } else {
            LinearRgba::BLACK
        };
        if let Some(m) = materials.get(&mat.0)
            && m.emissive != want
            && let Some(mut m) = materials.get_mut(&mat.0)
        {
            m.emissive = want;
        }
    }
}

/// The bottom HUD line: what pin is being typed for and the text so far.
pub(crate) fn update_typing_hud(
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
