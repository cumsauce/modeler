//! Material mode: the active material's node graph fills the centre slot
//! (one-row cards laid out by dependency level, curved edges, a dotted
//! background; wheel zooms about the cursor, middle-drag pans, click
//! selects, right-click comments), and the right slot shows a preview:
//! a lit sphere of the material (or the selected node's output), four
//! channel thumbnails from the bake, then the selected node's fields.
//! Watch + inspect: agents author through material_ops; this shows what
//! they built, live.

use crate::prelude::*;
use modeler::material::{doc, eval};

/// The centre root (the graph).
#[derive(Component)]
pub(crate) struct MatHud;

/// The graph's content, under one transform (pan + zoom).
#[derive(Component)]
pub(crate) struct MatCanvas;

#[derive(Component)]
pub(crate) struct MatTitle;

/// The right-slot root (the preview).
#[derive(Component)]
pub(crate) struct MatSide;

#[derive(Component)]
pub(crate) struct MatPreview;

#[derive(Component)]
pub(crate) struct MatPreviewText;

#[derive(Component)]
pub(crate) struct MatChannel(usize);

#[derive(Component)]
pub(crate) struct MatFields;

#[derive(Component)]
pub(crate) struct MatBox(usize);

impl MatPane {
    pub(crate) fn open(visible: bool) -> Self {
        MatPane {
            visible,
            zoom: 1.0,
            ..default()
        }
    }
}

#[derive(Resource, Default)]
pub(crate) struct MatPane {
    pub(crate) visible: bool,
    /// Cursor is over the graph: the 3D camera and pins ignore the mouse.
    pub(crate) hover: bool,
    /// The graph shown: (material, version, history cursor).
    shown: Option<(String, u64, Option<usize>)>,
    /// The preview shown: (graph key, selected node).
    side_shown: Option<((String, u64, Option<usize>), Option<String>)>,
    /// Node cards: name, graph position (top-left, px at zoom 1).
    boxes: Vec<(String, Vec2)>,
    /// Screen = pan + zoom * graph (pane-local px).
    zoom: f32,
    pan: Vec2,
    /// Fit the view on the next layout (a new material or pane size).
    fit: bool,
    selected: Option<String>,
    /// Last cursor position while middle-dragging.
    drag: Option<Vec2>,
    laid_out: UVec2,
}

const BOX_W: f32 = 168.0;
const BOX_H: f32 = 36.0;
const THUMB: usize = 28;
const COL_W: f32 = 224.0;
const ROW_H: f32 = 50.0;
const CHANNELS: [(&str, &str); 4] = [
    ("base_color", "base colour"),
    ("roughness", "roughness"),
    ("normal", "normal"),
    ("height", "height"),
];

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

/// One dot per 20 px cell, tiled behind the graph.
fn dot_image() -> Canvas {
    let n = 20;
    let mut c = Canvas::new(n, n, [0, 0, 0, 0]);
    for (x, y) in [(9, 9), (10, 9), (9, 10), (10, 10)] {
        let i = (y * n + x) * 4;
        c.px[i..i + 4].copy_from_slice(&[150, 155, 165, 70]);
    }
    c
}

fn type_color(ty: &str) -> Color {
    match ty {
        "noise" | "worley" => Color::srgb(0.30, 0.75, 0.52),
        "circle" | "rect" | "segment" | "polygon" | "star" | "hexagon" | "arc" | "rings"
        | "sphere" | "box" | "torus" | "capsule" | "cylinder" | "plane" | "sdf_op"
        | "sdf_round" | "onion" | "displace" | "mask" => Color::srgb(0.70, 0.45, 0.85),
        "input" | "transform" | "combine" | "split" | "polar" | "kaleido" | "warp"
        | "tile_grid" | "repeat" | "triplanar" => Color::srgb(0.35, 0.58, 0.92),
        "color" | "ramp" | "hsv" | "blend" | "levels" | "random" => Color::srgb(0.92, 0.65, 0.30),
        "output" => Color::srgb(0.92, 0.35, 0.35),
        _ => Color::srgb(0.6, 0.6, 0.66),
    }
}

/// A short one-line form of a field value.
fn short(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f.fract() == 0.0 {
                format!("{f:.0}")
            } else {
                format!("{}", (f * 10000.0).round() / 10000.0)
            }
        }
        other => other.to_string(),
    };
    if s.chars().count() > 34 {
        format!("{}…", s.chars().take(33).collect::<String>())
    } else {
        s
    }
}

/// A curved edge from `a` to `b` (graph px) as short rotated segments.
fn spawn_edge(c: &mut ChildSpawnerCommands, a: Vec2, b: Vec2) {
    let dx = ((b.x - a.x).abs() * 0.5).max(30.0);
    let (p1, p2) = (a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0));
    let at = |t: f32| {
        let u = 1.0 - t;
        a * u * u * u + p1 * 3.0 * u * u * t + p2 * 3.0 * u * t * t + b * t * t * t
    };
    let steps = 12;
    let mut prev = a;
    for k in 1..=steps {
        let q = at(k as f32 / steps as f32);
        let d = q - prev;
        let len = d.length() + 0.6;
        let mid = (q + prev) * 0.5;
        c.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(mid.x - len * 0.5),
                top: Val::Px(mid.y - 0.75),
                width: Val::Px(len),
                height: Val::Px(1.5),
                ..default()
            },
            UiTransform {
                rotation: Rot2::radians(d.y.atan2(d.x)),
                ..UiTransform::IDENTITY
            },
            BackgroundColor(Color::srgba(0.75, 0.8, 0.88, 0.45)),
            Pickable::IGNORE,
        ));
        prev = q;
    }
}

/// Shows the graph in Material mode, handles the mouse over it, and keeps
/// the right-slot preview in step with the selection.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_material_pane(
    (mouse, scroll, windows): (
        Res<ButtonInput<MouseButton>>,
        Res<AccumulatedMouseScroll>,
        Query<&Window, With<PrimaryWindow>>,
    ),
    modeler: Res<Modeler>,
    mut pane: ResMut<MatPane>,
    (wb, grid): (Res<Workbench>, Res<Grid>),
    mut hud: Query<(&mut Visibility, &ComputedNode), (With<MatHud>, Without<MatSide>)>,
    mut side: Query<&mut Visibility, (With<MatSide>, Without<MatHud>)>,
    mut canvas: Query<(Entity, &mut UiTransform), With<MatCanvas>>,
    mut texts: ParamSet<(
        Query<&mut Text, With<MatTitle>>,
        Query<&mut Text, With<MatPreviewText>>,
    )>,
    (mut preview, mut chans, fields): (
        Query<(&mut ImageNode, &mut Node), (With<MatPreview>, Without<MatChannel>)>,
        Query<(&MatChannel, &mut ImageNode), Without<MatPreview>>,
        Query<Entity, With<MatFields>>,
    ),
    mut boxes: Query<(&MatBox, &mut BorderColor)>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
    (assets, mut comment): (Res<AssetServer>, ResMut<CommentBox>),
) {
    pane.visible = wb.mode == Mode::Material && !grid.on;
    let want_vis = if pane.visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut side {
        v.set_if_neq(want_vis);
    }
    let Ok((mut vis, computed)) = hud.single_mut() else {
        return;
    };
    vis.set_if_neq(want_vis);
    if !pane.visible {
        pane.hover = false;
        pane.shown = None;
        pane.side_shown = None;
        pane.drag = None;
        return;
    }
    let size = computed.size() * computed.inverse_scale_factor();
    if size.x < 50.0 || size.y < 50.0 {
        return;
    }
    if size.round().as_uvec2() != pane.laid_out {
        pane.laid_out = size.round().as_uvec2();
        pane.fit = true;
    }
    let cursor = windows
        .single()
        .ok()
        .and_then(|w| w.cursor_position())
        .map(|c| c - wb.rects.centre.min);
    pane.hover = cursor.is_some_and(|c| c.x >= 0.0 && c.y >= 0.0 && c.x < size.x && c.y < size.y);
    if pane.zoom <= 0.0 {
        pane.zoom = 1.0;
    }

    // Pan (middle-drag) and zoom about the cursor (wheel).
    if let Some(c) = cursor {
        if mouse.pressed(MouseButton::Middle) && (pane.hover || pane.drag.is_some()) {
            if let Some(last) = pane.drag {
                pane.pan += c - last;
            }
            pane.drag = Some(c);
        } else {
            pane.drag = None;
        }
        if pane.hover && c.y > HEADER_H && scroll.delta.y != 0.0 {
            let lines = match scroll.unit {
                bevy::input::mouse::MouseScrollUnit::Line => scroll.delta.y,
                bevy::input::mouse::MouseScrollUnit::Pixel => scroll.delta.y / 20.0,
            };
            let z = pane.zoom;
            let nz = (z * 1.15f32.powf(lines)).clamp(0.2, 3.0);
            pane.pan = c - (c - pane.pan) * (nz / z);
            pane.zoom = nz;
        }
    }

    // Click a card: select it (again: back to the material). Right-click:
    // comment on that node, or on the material over empty graph.
    let pressed_l = mouse.just_pressed(MouseButton::Left);
    let pressed_r = mouse.just_pressed(MouseButton::Right);
    if let Some(c) = cursor.filter(|c| pane.hover && c.y > HEADER_H && (pressed_l || pressed_r)) {
        let g = (c - pane.pan) / pane.zoom;
        let hit = pane
            .boxes
            .iter()
            .find(|(_, p)| g.x >= p.x && g.x <= p.x + BOX_W && g.y >= p.y && g.y <= p.y + BOX_H)
            .map(|b| b.0.clone());
        if pressed_l {
            pane.selected = if hit == pane.selected {
                None
            } else {
                hit.clone()
            };
        }
        if pressed_r && let Ok(d) = modeler.session.materials.get(None) {
            comment.request(modeler::AnnotationTarget::Material {
                material: d.name.clone(),
                node: hit,
            });
        }
    }

    if let Ok((_, mut t)) = canvas.single_mut() {
        let want = UiTransform {
            translation: Val2::px(pane.pan.x, pane.pan.y),
            scale: Vec2::splat(pane.zoom),
            ..UiTransform::IDENTITY
        };
        if *t != want {
            *t = want;
        }
    }

    let mats = &modeler.session.materials;
    let Ok(d) = mats.get(None) else {
        if let Some(mut t) = texts.p0().iter_mut().next() {
            t.0 = "No material yet: material_ops creates one".into();
        }
        return;
    };
    let key = (d.name.clone(), d.version, d.history.current);
    if pane.shown.as_ref().is_none_or(|s| s.0 != key.0) {
        pane.fit = true;
    }

    // Cards and edges (columns = dependency levels), rebuilt per version.
    if pane.shown.as_ref() != Some(&key) || pane.fit {
        let rebuild = pane.shown.as_ref() != Some(&key);
        pane.shown = Some(key.clone());
        if rebuild {
            let s = d.samples(64, 0.25);
            let ev = match eval::evaluate(&d.nodes, &d.params, &s, true) {
                Ok(ev) => ev,
                Err(e) => {
                    if let Some(mut t) = texts.p0().iter_mut().next() {
                        t.0 = format!("material `{}`: {e}", d.name);
                    }
                    return;
                }
            };
            let levels = eval::levels(&ev.nodes).unwrap_or_default();
            let tallest = levels.iter().map(Vec::len).max().unwrap_or(1) as f32;
            let mut pos = vec![Vec2::ZERO; ev.nodes.len()];
            for (col, lv) in levels.iter().enumerate() {
                let off = (tallest - lv.len() as f32) * ROW_H * 0.5;
                for (row, &i) in lv.iter().enumerate() {
                    pos[i] = Vec2::new(col as f32 * COL_W, off + row as f32 * ROW_H);
                }
            }
            pane.boxes = ev
                .nodes
                .iter()
                .zip(&pos)
                .map(|(p, q)| (p.name.clone(), *q))
                .collect();
            if pane
                .selected
                .as_ref()
                .is_some_and(|n| !pane.boxes.iter().any(|b| &b.0 == n))
            {
                pane.selected = None;
            }
            let Ok((canvas, _)) = canvas.single() else {
                return;
            };
            commands.entity(canvas).despawn_children();
            let index: std::collections::HashMap<&str, usize> = ev
                .nodes
                .iter()
                .enumerate()
                .map(|(i, p)| (p.name.as_str(), i))
                .collect();
            let mono = TextFont {
                font: assets.load(bevy::feathers::constants::fonts::MONO).into(),
                font_size: FontSize::Px(10.0),
                ..default()
            };
            commands.entity(canvas).with_children(|c| {
                for (i, p) in ev.nodes.iter().enumerate() {
                    for r in &p.reads {
                        if let Some(&j) = index.get(r.as_str()) {
                            spawn_edge(
                                c,
                                pos[j] + Vec2::new(BOX_W, BOX_H * 0.5),
                                pos[i] + Vec2::new(0.0, BOX_H * 0.5),
                            );
                        }
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
                    c.spawn((
                        MatBox(i),
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(pos[i].x),
                            top: Val::Px(pos[i].y),
                            width: Val::Px(BOX_W),
                            height: Val::Px(BOX_H),
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(7.0),
                            padding: UiRect::axes(Val::Px(3.0), Val::Px(2.0)),
                            border: UiRect::all(Val::Px(1.5)),
                            overflow: Overflow::clip(),
                            border_radius: BorderRadius::all(Val::Px(6.0)),
                            ..default()
                        },
                        BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.08)),
                        BackgroundColor(Color::srgb(0.13, 0.135, 0.15)),
                    ))
                    .with_children(|b| {
                        // The type's colour as a strip on the left edge.
                        b.spawn((
                            Node {
                                width: Val::Px(3.0),
                                height: Val::Percent(100.0),
                                border_radius: BorderRadius::all(Val::Px(2.0)),
                                flex_shrink: 0.0,
                                ..default()
                            },
                            BackgroundColor(type_color(&p.ty)),
                        ));
                        let tn = Node {
                            width: Val::Px(THUMB as f32),
                            height: Val::Px(THUMB as f32),
                            flex_shrink: 0.0,
                            border_radius: BorderRadius::all(Val::Px(3.0)),
                            ..default()
                        };
                        match thumb {
                            Some(h) => b.spawn((ImageNode::new(h), tn)),
                            None => b.spawn((tn, BackgroundColor(Color::BLACK))),
                        };
                        b.spawn(Node {
                            flex_direction: FlexDirection::Column,
                            overflow: Overflow::clip(),
                            ..default()
                        })
                        .with_children(|t| {
                            t.spawn((
                                Text::new(p.name.clone()),
                                ui_text(&assets, 12.0, false),
                                TextColor(Color::srgb(0.92, 0.93, 0.95)),
                            ));
                            t.spawn((
                                Text::new(p.ty.clone()),
                                mono.clone(),
                                TextColor(type_color(&p.ty).with_alpha(0.85)),
                            ));
                        });
                    });
                }
            });
        }
        if pane.fit {
            pane.fit = false;
            let ext = pane
                .boxes
                .iter()
                .fold(Vec2::ZERO, |a, (_, p)| a.max(*p + Vec2::new(BOX_W, BOX_H)));
            let avail = Vec2::new(size.x - 48.0, size.y - HEADER_H - 48.0);
            let z = (avail.x / ext.x.max(1.0))
                .min(avail.y / ext.y.max(1.0))
                .clamp(0.3, 1.2);
            pane.zoom = z;
            pane.pan = Vec2::new(
                (size.x - ext.x * z) * 0.5,
                HEADER_H + (size.y - HEADER_H - ext.y * z) * 0.5,
            );
        }
        if let Some(mut t) = texts.p0().iter_mut().next() {
            t.0 = format!(
                "Material `{}` v{}  ·  {} nodes  ·  click to inspect · right-click to comment · wheel zoom · middle-drag pan",
                d.name,
                d.version,
                d.nodes.len()
            );
        }
    }

    let sel = pane.selected.clone();
    for (b, mut border) in &mut boxes {
        let on = pane
            .boxes
            .get(b.0)
            .is_some_and(|x| Some(&x.0) == sel.as_ref());
        let want = BorderColor::all(if on {
            Color::srgb(1.0, 0.82, 0.3)
        } else {
            Color::srgba(1.0, 1.0, 1.0, 0.08)
        });
        if *border != want {
            *border = want;
        }
    }

    // The right-slot preview.
    let side_key = (key, sel.clone());
    if pane.side_shown.as_ref() == Some(&side_key) {
        return;
    }
    pane.side_shown = Some(side_key);
    let n = (d.resolution as usize).clamp(64, 256);
    let ch = d.bake(n, true);
    let (label, big) = match &sel {
        Some(node) => match doc::render(d, Some(node), None, Some(128), None) {
            Ok((t, img)) => (t, img),
            Err(e) => (e, Canvas::new(8, 8, [0, 0, 0, 255])),
        },
        None => match &ch {
            Ok(ch) => {
                let dia = if d.domain == "tile" {
                    d.tile * 2.0
                } else {
                    0.5
                };
                (
                    format!("`{}` on a {dia:.2} m ball", d.name),
                    doc::sphere_preview(d, ch, 256, dia),
                )
            }
            Err(e) => (e.clone(), Canvas::new(8, 8, [0, 0, 0, 255])),
        },
    };
    let w = (wb.rects.right.width() - 24.0).max(40.0);
    if let Ok((mut im, mut node)) = preview.single_mut() {
        let h = w * big.h as f32 / big.w.max(1) as f32;
        node.width = Val::Px(w);
        node.height = Val::Px(h);
        im.image = canvas_image(&big, &mut images);
    }
    if let Some(mut t) = texts.p1().iter_mut().next() {
        t.0 = label.lines().next().unwrap_or_default().to_string();
    }
    for (c, mut im) in &mut chans {
        let img = ch
            .as_ref()
            .ok()
            .and_then(|ch| ch.image(CHANNELS[c.0].0).ok())
            .unwrap_or_else(|| Canvas::new(4, 4, [20, 20, 24, 255]));
        im.image = canvas_image(&resample(&img, 64, 64), &mut images);
    }
    if let Ok(f) = fields.single() {
        commands.entity(f).despawn_children();
        let raw = sel.as_ref().and_then(|s| {
            d.nodes
                .iter()
                .find(|v| v.get("name").and_then(|n| n.as_str()) == Some(s.as_str()))
        });
        let mut rows: Vec<(String, String)> = match raw.and_then(|v| v.as_object()) {
            Some(o) => {
                let mut r = vec![(
                    "type".to_string(),
                    o.get("type").map(short).unwrap_or_default(),
                )];
                r.extend(
                    o.iter()
                        .filter(|(k, _)| *k != "name" && *k != "type")
                        .map(|(k, v)| (k.clone(), short(v))),
                );
                r
            }
            None => vec![
                ("domain".into(), d.domain.clone()),
                ("tile".into(), format!("{} m", d.tile)),
                ("resolution".into(), d.resolution.to_string()),
                ("nodes".into(), d.nodes.len().to_string()),
            ],
        };
        if sel.is_none() {
            rows.extend(
                d.params
                    .iter()
                    .map(|(k, v)| (format!("${k}"), short(&serde_json::json!(v)))),
            );
        }
        let mono = TextFont {
            font: assets.load(bevy::feathers::constants::fonts::MONO).into(),
            font_size: FontSize::Px(11.0),
            ..default()
        };
        commands.entity(f).with_children(|c| {
            c.spawn((
                Text::new(match &sel {
                    Some(s) => format!("node {s}"),
                    None => "material".to_string(),
                }),
                ui_text(&assets, 13.0, true),
                ui_color(false),
                Node {
                    margin: UiRect::bottom(Val::Px(4.0)),
                    ..default()
                },
            ));
            for (k, v) in rows {
                c.spawn(Node {
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: Val::Px(8.0),
                    min_height: Val::Px(20.0),
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|r| {
                    r.spawn((Text::new(k), ui_text(&assets, 12.0, false), ui_color(true)));
                    r.spawn((Text::new(v), mono.clone(), ui_color(false)));
                });
            }
        });
    }
}

/// The graph (centre slot) and the preview (right slot), hidden until
/// Material mode.
pub(crate) fn setup_material_pane(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
) {
    let dots = canvas_image(&dot_image(), &mut images);
    let pane = commands
        .spawn((
            Name::new("material graph"),
            MatHud,
            InSlot(PaneSlot::Centre),
            BlocksCamera,
            Visibility::Hidden,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgb(0.075, 0.078, 0.088)),
        ))
        .with_children(|p| {
            p.spawn((
                ImageNode::new(dots).with_mode(NodeImageMode::Tiled {
                    tile_x: true,
                    tile_y: true,
                    stretch_value: 1.0,
                }),
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                Pickable::IGNORE,
            ));
            // Zero-size at the pane's origin, so its transform's scale
            // pivots there: screen = pan + zoom * graph.
            p.spawn((
                MatCanvas,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    width: Val::Px(0.0),
                    height: Val::Px(0.0),
                    ..default()
                },
                UiTransform::IDENTITY,
            ));
        })
        .id();
    spawn_pane_header(
        &mut commands,
        &assets,
        pane,
        PaneToggle::Material,
        (MatTitle, Text::new(String::new())),
    );

    commands
        .spawn((
            Name::new("material preview"),
            MatSide,
            InSlot(PaneSlot::Right),
            Visibility::Hidden,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(12.0)),
                row_gap: Val::Px(8.0),
                overflow: Overflow::clip(),
                ..default()
            },
            pane_bg(),
            GlobalZIndex(3),
        ))
        .with_children(|p| {
            p.spawn((
                Text::new("Preview"),
                ui_text(&assets, 13.0, true),
                ui_color(false),
            ));
            p.spawn((
                MatPreview,
                ImageNode::default(),
                Node {
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    ..default()
                },
            ));
            p.spawn((
                MatPreviewText,
                Text::new(String::new()),
                ui_text(&assets, 11.0, false),
                ui_color(true),
            ));
            p.spawn(Node {
                column_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|row| {
                for (i, (_, label)) in CHANNELS.iter().enumerate() {
                    row.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        flex_grow: 1.0,
                        flex_basis: Val::Px(0.0),
                        row_gap: Val::Px(2.0),
                        ..default()
                    })
                    .with_children(|c| {
                        c.spawn((
                            MatChannel(i),
                            ImageNode::default(),
                            Node {
                                width: Val::Percent(100.0),
                                aspect_ratio: Some(1.0),
                                border_radius: BorderRadius::all(Val::Px(4.0)),
                                ..default()
                            },
                        ));
                        c.spawn((
                            Text::new(*label),
                            ui_text(&assets, 10.0, false),
                            ui_color(true),
                        ));
                    });
                }
            });
            p.spawn((
                MatFields,
                Node {
                    flex_direction: FlexDirection::Column,
                    margin: UiRect::top(Val::Px(6.0)),
                    ..default()
                },
            ));
        });
}
