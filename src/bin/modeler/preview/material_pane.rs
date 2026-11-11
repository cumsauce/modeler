//! The material pane (toggled with M): the active material's node graph, a
//! thumbnail of every node's output, and a large preview (lit sphere plus
//! channels, or the clicked node). Watch + inspect: agents author through
//! material_ops; this shows what they built, live.

use crate::prelude::*;
use modeler::material::{doc, eval};

#[derive(Component)]
pub(crate) struct MatHud;

#[derive(Component)]
pub(crate) struct MatCanvas;

#[derive(Component)]
pub(crate) struct MatTitle;

#[derive(Component)]
pub(crate) struct MatPreview;

#[derive(Component)]
pub(crate) struct MatBox(usize);

impl MatPane {
    /// Open at start (MODELER_MATERIALS=1), like MODELER_GRAPH for G.
    pub(crate) fn open(visible: bool) -> Self {
        MatPane {
            visible,
            ..default()
        }
    }
}

#[derive(Resource, Default)]
pub(crate) struct MatPane {
    pub(crate) visible: bool,
    /// Cursor is over the pane: the 3D camera and pins ignore the mouse.
    pub(crate) hover: bool,
    /// What the pane shows: (material, version, history cursor, selection).
    shown: Option<(String, u64, Option<usize>, Option<usize>)>,
    /// Node boxes: name, graph position (top-left, px at zoom 1).
    boxes: Vec<(String, Vec2)>,
    zoom: f32,
    selected: Option<usize>,
}

const BOX_W: f32 = 120.0;
const BOX_H: f32 = 96.0;
const THUMB: usize = 64;
const COL_W: f32 = 150.0;
const ROW_H: f32 = 110.0;

fn canvas_image(c: &Canvas, images: &mut Assets<Image>) -> Handle<Image> {
    use bevy::render::render_resource::{Extent3d, TextureDimension};
    images.add(Image::new(
        Extent3d {
            width: c.w as u32,
            height: c.h as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        c.px.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ))
}

fn resample(c: &Canvas, w: usize, h: usize) -> Canvas {
    let mut out = Canvas::new(w, h, [0, 0, 0, 255]);
    for y in 0..h {
        for x in 0..w {
            let si = ((y * c.h / h) * c.w + x * c.w / w) * 4;
            let di = (y * w + x) * 4;
            out.px[di..di + 4].copy_from_slice(&c.px[si..si + 4]);
        }
    }
    out
}

fn type_color(ty: &str) -> Color {
    match ty {
        "noise" | "worley" => Color::srgb(0.16, 0.42, 0.30),
        "circle" | "rect" | "segment" | "polygon" | "star" | "hexagon" | "arc" | "rings"
        | "sphere" | "box" | "torus" | "capsule" | "cylinder" | "plane" | "sdf_op"
        | "sdf_round" | "onion" | "displace" | "mask" => Color::srgb(0.45, 0.25, 0.55),
        "input" | "transform" | "combine" | "split" | "polar" | "kaleido" | "warp"
        | "tile_grid" | "repeat" | "triplanar" => Color::srgb(0.16, 0.32, 0.55),
        "color" | "ramp" | "hsv" | "blend" | "levels" | "random" => Color::srgb(0.55, 0.36, 0.14),
        "output" => Color::srgb(0.6, 0.15, 0.15),
        _ => Color::srgb(0.3, 0.3, 0.34),
    }
}

/// M toggles the pane (G's graph pane closes; they share the left half).
/// Rebuilds when the active material changes or a box is clicked.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_material_pane(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    typing: Res<PinTyping>,
    modeler: Res<Modeler>,
    mut pane: ResMut<MatPane>,
    mut graph: ResMut<GraphHudVisible>,
    mut hud: Query<(&mut Visibility, &ComputedNode), With<MatHud>>,
    canvas: Query<Entity, With<MatCanvas>>,
    mut title: Query<&mut Text, With<MatTitle>>,
    mut preview: Query<(&mut ImageNode, &mut Node), (With<MatPreview>, Without<MatBox>)>,
    mut boxes: Query<(&MatBox, &mut BorderColor)>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
) {
    if keys.just_pressed(KeyCode::KeyM) && typing.text.is_empty() {
        pane.visible = !pane.visible;
        if pane.visible {
            graph.0 = false;
        }
    }
    if graph.0 && pane.visible {
        pane.visible = false;
    }
    let Ok((mut vis, computed)) = hud.single_mut() else {
        return;
    };
    *vis = if pane.visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if !pane.visible {
        pane.hover = false;
        pane.shown = None;
        return;
    }
    let size = computed.size() * computed.inverse_scale_factor();
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    pane.hover = cursor.is_some_and(|c| c.x < size.x && c.y < size.y);

    // Click a box: inspect that node (click again: back to the material).
    let top = size.y * 0.45;
    if let Some(c) = cursor.filter(|_| pane.hover)
        && mouse.just_pressed(MouseButton::Left)
        && c.y > top
    {
        let z = pane.zoom.max(0.1);
        let g = (c - Vec2::new(10.0, top + 10.0)) / z;
        let hit = pane.boxes.iter().position(|(_, p)| {
            g.x >= p.x && g.x <= p.x + BOX_W && g.y >= p.y && g.y <= p.y + BOX_H
        });
        pane.selected = if hit == pane.selected { None } else { hit };
    }

    let mats = &modeler.session.materials;
    let Ok(d) = mats.get(None) else {
        if let Ok(mut t) = title.single_mut() {
            t.0 = "no material yet: material_ops creates one (M closes)".into();
        }
        return;
    };
    let want = (d.name.clone(), d.version, d.history.current, pane.selected);
    if pane.shown.as_ref() == Some(&want) {
        return;
    }
    let rebuild_boxes = pane
        .shown
        .as_ref()
        .is_none_or(|s| (&s.0, s.1, s.2) != (&want.0, want.1, want.2));
    pane.shown = Some(want);

    // Thumbnails and layout (columns = dependency levels).
    if rebuild_boxes {
        let s = d.samples(96, 0.25);
        let ev = match eval::evaluate(&d.nodes, &d.params, &s, true) {
            Ok(ev) => ev,
            Err(e) => {
                if let Ok(mut t) = title.single_mut() {
                    t.0 = format!("material `{}`: {e}", d.name);
                }
                return;
            }
        };
        let levels = eval::levels(&ev.nodes).unwrap_or_default();
        let mut pos = vec![Vec2::ZERO; ev.nodes.len()];
        for (col, lv) in levels.iter().enumerate() {
            for (row, &i) in lv.iter().enumerate() {
                pos[i] = Vec2::new(col as f32 * COL_W, row as f32 * ROW_H);
            }
        }
        let ext = pos
            .iter()
            .fold(Vec2::ZERO, |a, p| a.max(*p + Vec2::new(BOX_W, BOX_H)));
        let avail = Vec2::new(size.x - 20.0, size.y - top - 20.0);
        pane.zoom = (avail.x / ext.x.max(1.0))
            .min(avail.y / ext.y.max(1.0))
            .clamp(0.3, 1.0);
        pane.boxes = ev
            .nodes
            .iter()
            .zip(&pos)
            .map(|(p, q)| (p.name.clone(), *q))
            .collect();
        if pane.selected.is_some_and(|i| i >= pane.boxes.len()) {
            pane.selected = None;
        }
        let z = pane.zoom;
        let Ok(canvas) = canvas.single() else {
            return;
        };
        commands.entity(canvas).despawn_children();
        let index: std::collections::HashMap<&str, usize> = ev
            .nodes
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.as_str(), i))
            .collect();
        let origin = Vec2::new(10.0, top + 10.0);
        commands.entity(canvas).with_children(|c| {
            for (i, p) in ev.nodes.iter().enumerate() {
                for r in &p.reads {
                    let Some(&j) = index.get(r.as_str()) else {
                        continue;
                    };
                    let a = origin + (pos[j] + Vec2::new(BOX_W, BOX_H * 0.5)) * z;
                    let b = origin + (pos[i] + Vec2::new(0.0, BOX_H * 0.5)) * z;
                    let dlt = b - a;
                    let len = dlt.length();
                    let mid = (a + b) * 0.5;
                    c.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(mid.x - len * 0.5),
                            top: Val::Px(mid.y - 1.0),
                            width: Val::Px(len),
                            height: Val::Px(2.0),
                            ..default()
                        },
                        UiTransform {
                            rotation: Rot2::radians(dlt.y.atan2(dlt.x)),
                            ..UiTransform::IDENTITY
                        },
                        BackgroundColor(Color::srgba(0.8, 0.85, 0.9, 0.4)),
                    ));
                }
            }
            for (i, p) in ev.nodes.iter().enumerate() {
                let thumb = match ev.ports.get(&p.name).and_then(|ports| ports.first()) {
                    Some((_, b)) => {
                        let (img, _) = doc::node_image(b, s.w, s.h);
                        Some(canvas_image(&resample(&img, THUMB, THUMB), &mut images))
                    }
                    // The output node shows the base colour it produces.
                    None => modeler::material::bake::channels(&ev, &s)
                        .and_then(|ch| ch.image("base_color"))
                        .ok()
                        .map(|img| canvas_image(&resample(&img, THUMB, THUMB), &mut images)),
                };
                let at = origin + pos[i] * z;
                c.spawn((
                    MatBox(i),
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(at.x),
                        top: Val::Px(at.y),
                        width: Val::Px(BOX_W * z),
                        height: Val::Px(BOX_H * z),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        padding: UiRect::all(Val::Px(3.0 * z)),
                        border: UiRect::all(Val::Px(2.0)),
                        overflow: Overflow::clip(),
                        border_radius: BorderRadius::all(Val::Px(5.0)),
                        ..default()
                    },
                    BorderColor::all(Color::NONE),
                    BackgroundColor(type_color(&p.ty).with_alpha(0.95)),
                ))
                .with_children(|b| {
                    if let Some(h) = thumb {
                        b.spawn((
                            ImageNode::new(h),
                            Node {
                                width: Val::Px(THUMB as f32 * z),
                                height: Val::Px(THUMB as f32 * z),
                                ..default()
                            },
                        ));
                    }
                    b.spawn((
                        Text::new(format!("{} ({})", p.name, p.ty)),
                        TextFont {
                            font_size: FontSize::Px((11.0 * z).max(7.0)),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            }
        });
    }

    // The big preview: the material, or the selected node.
    let node = pane
        .selected
        .and_then(|i| pane.boxes.get(i))
        .map(|b| b.0.clone());
    let (text, img) = match doc::render(d, node.as_deref(), None, Some(256), None) {
        Ok(r) => r,
        Err(e) => (e, Canvas::new(8, 8, [0, 0, 0, 255])),
    };
    if let Ok((mut im, mut n)) = preview.single_mut() {
        let scale = ((size.x - 20.0) / img.w as f32)
            .min((top - 56.0) / img.h as f32)
            .min(1.5);
        n.width = Val::Px(img.w as f32 * scale);
        n.height = Val::Px(img.h as f32 * scale);
        im.image = canvas_image(&img, &mut images);
    }
    if let Ok(mut t) = title.single_mut() {
        let first = text.lines().next().unwrap_or_default().to_string();
        t.0 = format!(
            "{first}\n{} node(s) | history {} nodes | click a node to inspect it, again for the material | M closes",
            d.nodes.len(),
            d.history.nodes.len()
        );
    }
    let sel = pane.selected;
    for (b, mut border) in &mut boxes {
        *border = BorderColor::all(if Some(b.0) == sel {
            Color::srgb(1.0, 0.85, 0.2)
        } else {
            Color::NONE
        });
    }
}

/// The pane's UI tree (hidden until M).
pub(crate) fn setup_material_pane(mut commands: Commands) {
    commands
        .spawn((
            Name::new("material pane"),
            MatHud,
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
                MatTitle,
                Text::new(String::new()),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(Color::srgba(0.85, 0.88, 0.92, 0.9)),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(6.0),
                    left: Val::Px(10.0),
                    right: Val::Px(10.0),
                    ..default()
                },
            ));
            p.spawn((
                MatPreview,
                ImageNode::default(),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(52.0),
                    left: Val::Px(10.0),
                    ..default()
                },
            ));
            p.spawn((
                MatCanvas,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
            ));
        });
}
