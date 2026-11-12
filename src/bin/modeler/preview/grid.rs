//! The grid view (G): every open document side by side, one tile each, so
//! a human can watch several agents build at once.
//!
//! Cheap by design. Each tile is its own camera on its own render layer
//! (the document's parts spawned there), rendering into its own image, but
//! a tile camera is active only for a few frames after its document
//! changes (or the tile is resized); the rest of the time the image just
//! keeps its last frame. An idle grid costs no 3D rendering at all, and the
//! window's own camera (TAA + SSR, the expensive one) is paused while the
//! grid is up unless a `screenshot` call needs it. Tile cameras are lean:
//! deferred like the window (so materials shade the same) but no SSR, TAA
//! or SSAO, and their light casts no shadows.
//!
//! Click a tile to make that document active and leave the grid.
//!
//! On by default. The tiles show the most recently changed documents
//! ([`Recency`]), and steadily: a shown document keeps its slot however
//! often it changes; one that isn't shown takes the front slot when it
//! changes, and the stalest tile drops off the end. With parallel agents
//! each keeps its tile instead of the view jumping between their documents.

use std::time::Instant;

use bevy::camera::visibility::RenderLayers;
use bevy::picking::events::PointerClick;

use crate::prelude::*;

/// Most tiles shown (render layers `FIRST_LAYER..`).
const MAX_TILES: usize = 9;
const FIRST_LAYER: usize = 10;
/// Frames a tile camera stays active after a change (pipelines and the
/// deferred passes settle within these).
const SETTLE_FRAMES: u32 = 12;
const GAP: f32 = 6.0;
const LABEL_H: f32 = 22.0;

#[derive(Resource)]
pub(crate) struct Grid {
    pub(crate) on: bool,
    tiles: Vec<Tile>,
    /// An unsaved document whose close button was clicked once: the
    /// second click closes it (its work is lost unless saved).
    confirm_close: Option<String>,
}

/// Every open document, most recently changed first, and the grid's steady
/// slots (shared with the tab strip).
#[derive(Resource, Default)]
pub(crate) struct Recency {
    /// History (node count, cursor) last seen per document: a change in it
    /// is an edit, undo or jump (switching documents doesn't count).
    keys: std::collections::HashMap<String, (usize, Option<usize>)>,
    /// When each document last changed while the window watched.
    pub(crate) changed: std::collections::HashMap<String, Instant>,
    /// All open documents, most recently changed first.
    pub(crate) order: Vec<String>,
    /// At most [`MAX_TILES`], steady (see the module docs).
    pub(crate) slots: Vec<String>,
    started: bool,
}

pub(crate) fn track_recency(modeler: Res<Modeler>, mut r: ResMut<Recency>) {
    let s = &modeler.session;
    // The active document, then the others most recently switched away
    // from first: the starting order before any change is seen.
    let docs: Vec<&modeler::Model> = std::iter::once(&s.model)
        .chain(s.others.iter().rev())
        .collect();
    if r.order.len() != docs.len() || !r.started {
        let names: std::collections::HashSet<&str> = docs.iter().map(|m| m.name.as_str()).collect();
        r.order.retain(|n| names.contains(n.as_str()));
        r.slots.retain(|n| names.contains(n.as_str()));
        r.keys.retain(|n, _| names.contains(n.as_str()));
        r.changed.retain(|n, _| names.contains(n.as_str()));
    }
    let started = r.started;
    for m in docs {
        let key = (m.history.nodes.len(), m.history.current);
        let front = match r.keys.get(&m.name) {
            None => {
                r.keys.insert(m.name.clone(), key);
                // New since startup (an agent opened it): to the front.
                if !started {
                    r.order.push(m.name.clone());
                    false
                } else {
                    true
                }
            }
            Some(k) if *k != key => {
                r.keys.insert(m.name.clone(), key);
                true
            }
            _ => false,
        };
        if front {
            r.changed.insert(m.name.clone(), Instant::now());
            r.order.retain(|n| *n != m.name);
            r.order.insert(0, m.name.clone());
            if !r.slots.contains(&m.name) {
                r.slots.insert(0, m.name.clone());
                r.slots.truncate(MAX_TILES);
            }
        }
    }
    r.started = true;
    if r.slots.len() < MAX_TILES {
        let missing: Vec<String> = r
            .order
            .iter()
            .filter(|n| !r.slots.contains(n))
            .take(MAX_TILES - r.slots.len())
            .cloned()
            .collect();
        r.slots.extend(missing);
    }
}

impl Grid {
    /// On at start unless MODELER_GRID=0.
    pub(crate) fn open(on: bool) -> Self {
        Grid {
            on,
            tiles: Vec::new(),
            confirm_close: None,
        }
    }
}

struct Tile {
    doc: String,
    layer: usize,
    camera: Entity,
    node: Entity,
    label: Entity,
    close: Entity,
    image: Handle<Image>,
    size: UVec2,
    /// (version, history length) the parts were spawned from.
    built: Option<(u64, usize)>,
    parts: Vec<Entity>,
    frames_left: u32,
    /// When it last changed while the window watched (None: not yet).
    changed_at: Option<Instant>,
    parts_n: usize,
    author: Option<String>,
    /// Where the tile camera should be (applied once the camera exists:
    /// it is spawned by deferred commands).
    view: Transform,
}

/// Marks a tile's image node (clicking it switches to `0`).
#[derive(Component)]
pub(crate) struct GridTileNode(String);

/// A tile's close button (closes document `0`).
#[derive(Component)]
pub(crate) struct GridTileClose(String);

#[derive(Component)]
pub(crate) struct GridTileCamera;

pub(crate) fn grid_off(grid: Res<Grid>) -> bool {
    !grid.on
}

pub(crate) fn toggle_grid_key(
    keys: Res<ButtonInput<KeyCode>>,
    typing: Res<PinTyping>,
    mut grid: ResMut<Grid>,
) {
    if keys.just_pressed(KeyCode::KeyG) && !typing.open {
        grid.on = !grid.on;
    }
}

fn ago(t: Instant) -> String {
    let s = t.elapsed().as_secs();
    match s {
        0..=4 => "just now".into(),
        5..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        _ => format!("{}h ago", s / 3600),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update_grid(
    mut commands: Commands,
    mut grid: ResMut<Grid>,
    mut modeler: ResMut<Modeler>,
    view: Res<View3d>,
    windows: Query<&Window, With<PrimaryWindow>>,
    assets: Res<AssetServer>,
    (mut meshes, mut materials, mut images, mut pbr_cache): (
        ResMut<Assets<Mesh>>,
        ResMut<Assets<StandardMaterial>>,
        ResMut<Assets<Image>>,
        ResMut<PbrCache>,
    ),
    mut main_cam: Query<
        (&mut Camera, Option<&bevy::light::EnvironmentMapLight>),
        (With<MainCamera>, Without<GridTileCamera>),
    >,
    mut main_node: Query<&mut Visibility, With<View3dNode>>,
    mut tile_cams: Query<
        (&mut Camera, &mut Transform, &mut RenderTarget),
        (With<GridTileCamera>, Without<MainCamera>),
    >,
    mut nodes: Query<(&mut Node, &mut ImageNode), With<GridTileNode>>,
    mut texts: Query<(&mut Text, &mut Node), Without<GridTileNode>>,
    mut label_tick: Local<Option<Instant>>,
    recency: Res<Recency>,
    mut more: Local<Option<Entity>>,
    winit: Option<ResMut<bevy::winit::WinitSettings>>,
) {
    // While the grid is up nothing animates (tiles render only on change),
    // so the app loop drops to ~30 fps; input still wakes it at once.
    if let Some(mut w) = winit {
        let want = if grid.on {
            bevy::winit::UpdateMode::reactive(std::time::Duration::from_millis(33))
        } else {
            bevy::winit::UpdateMode::Continuous
        };
        if w.focused_mode != want {
            w.focused_mode = want;
            w.unfocused_mode = want;
        }
    }
    let Ok((mut main, env)) = main_cam.single_mut() else {
        return;
    };
    let env = env.cloned();
    if !grid.on {
        if !grid.tiles.is_empty() {
            for t in grid.tiles.drain(..) {
                despawn_tile(&mut commands, &t);
            }
        }
        if let Some(e) = more.take() {
            commands.entity(e).despawn();
        }
        if !main.is_active {
            main.is_active = true;
        }
        for mut v in &mut main_node {
            if *v != Visibility::Inherited {
                *v = Visibility::Inherited;
            }
        }
        return;
    }
    // The window camera rests while the grid is up (a screenshot then
    // captures the window, tiles and all).
    let want_main = false;
    if main.is_active != want_main {
        main.is_active = want_main;
    }
    for mut v in &mut main_node {
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
    }
    // Tile cameras borrow the window camera's filtered studio light; wait
    // for it (a few frames after startup).
    let Some(env) = env else {
        return;
    };
    let sf = windows.single().map(|w| w.scale_factor()).unwrap_or(1.0);
    let win_w = windows.single().map(|w| w.width()).unwrap_or(0.0);
    let s = &mut modeler.session;
    let names: Vec<String> = recency.slots.clone();
    let total = recency.order.len();
    // Drop tiles of closed documents.
    let mut keep = Vec::new();
    for t in grid.tiles.drain(..) {
        if names.contains(&t.doc) {
            keep.push(t);
        } else {
            despawn_tile(&mut commands, &t);
        }
    }
    grid.tiles = keep;
    // Layout in the 3D area: a near-square grid.
    let n = names.len().max(1);
    let cols = (n as f32).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    // Below the toolbar row.
    let mut r = view.rect;
    r.min.y += 40.0;
    let cw = ((r.width() - GAP * (cols as f32 + 1.0)) / cols as f32).max(16.0);
    let ch = ((r.height() - GAP * (rows as f32 + 1.0)) / rows as f32).max(16.0 + LABEL_H);
    let refresh_labels = label_tick.is_none_or(|t| t.elapsed().as_millis() >= 500);
    if refresh_labels {
        *label_tick = Some(Instant::now());
    }
    // How many are open beyond the tiles, over the grid's top-left corner.
    let note = (total > names.len()).then(|| {
        format!(
            "{} of {total} open documents, most recently changed first (O lists them all)",
            names.len()
        )
    });
    match (&note, *more) {
        (Some(t), None) => {
            *more = Some(
                commands
                    .spawn((
                        Name::new("grid more"),
                        Text::new(t.clone()),
                        ui_text(&assets, 13.0, false),
                        ui_color(false),
                        TextLayout::no_wrap(),
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(r.min.x + GAP + 2.0),
                            top: Val::Px(r.min.y - 28.0),
                            ..default()
                        },
                        Pickable::IGNORE,
                        GlobalZIndex(1),
                    ))
                    .id(),
            );
        }
        (Some(t), Some(e)) if refresh_labels => {
            if let Ok((mut text, mut node)) = texts.get_mut(e) {
                if text.0 != *t {
                    text.0 = t.clone();
                }
                let want = (Val::Px(r.min.x + GAP + 2.0), Val::Px(r.min.y - 28.0));
                if (node.left, node.top) != want {
                    (node.left, node.top) = want;
                }
            }
        }
        (None, Some(e)) => {
            commands.entity(e).despawn();
            *more = None;
        }
        _ => {}
    }
    for (i, name) in names.iter().enumerate() {
        let model: &modeler::Model = if *name == s.model.name {
            &s.model
        } else {
            match s.others.iter().find(|m| &m.name == name) {
                Some(m) => m,
                None => continue,
            }
        };
        let (cx, cy) = (i % cols, i / cols);
        let left = r.min.x + GAP + cx as f32 * (cw + GAP);
        let top = r.min.y + GAP + cy as f32 * (ch + GAP);
        let img_h = ch - LABEL_H;
        let size = UVec2::new(
            ((cw * sf).round() as u32).max(16),
            ((img_h * sf).round() as u32).max(16),
        );
        let idx = match grid.tiles.iter().position(|t| &t.doc == name) {
            Some(k) => k,
            None => {
                let used: Vec<usize> = grid.tiles.iter().map(|t| t.layer).collect();
                let Some(layer) =
                    (FIRST_LAYER..FIRST_LAYER + MAX_TILES).find(|l| !used.contains(l))
                else {
                    continue;
                };
                let image = images.add(view_target(size.x, size.y));
                let camera = commands
                    .spawn((
                        Name::new(format!("grid camera {name}")),
                        GridTileCamera,
                        Camera3d::default(),
                        Camera {
                            is_active: true,
                            ..default()
                        },
                        // Deferred lighting needs the depth prepass too:
                        // without it tiles shaded flat and washed out.
                        bevy::core_pipeline::prepass::DepthPrepass,
                        bevy::core_pipeline::prepass::DeferredPrepass,
                        Msaa::Off,
                        env.clone(),
                        AmbientLight {
                            brightness: 60.0,
                            ..default()
                        },
                        RenderLayers::layer(layer),
                        Transform::default(),
                    ))
                    .insert(RenderTarget::Image(image.clone().into()))
                    .with_children(|c| {
                        // The window's lights: the shadowed sun (fixed in the
                        // world; the tile view is fixed, so a constant local
                        // rotation keeps it there) and the view fill.
                        let view = Transform::default()
                            .looking_to(
                                -(Quat::from_euler(EulerRot::YXZ, 0.62, -0.42, 0.0) * Vec3::Z),
                                Vec3::Y,
                            )
                            .rotation;
                        let sun = Quat::from_euler(EulerRot::XYZ, -0.9, 0.5, 0.0);
                        c.spawn((
                            DirectionalLight {
                                illuminance: 8000.0,
                                shadow_maps_enabled: true,
                                ..default()
                            },
                            RenderLayers::layer(layer),
                            Transform::from_rotation(view.inverse() * sun),
                        ));
                        c.spawn((
                            DirectionalLight {
                                illuminance: 500.0,
                                ..default()
                            },
                            RenderLayers::layer(layer),
                            Transform::default(),
                        ));
                    })
                    .id();
                let node = commands
                    .spawn((
                        Name::new(format!("grid tile {name}")),
                        GridTileNode(name.clone()),
                        ImageNode::new(image.clone()),
                        Node {
                            position_type: PositionType::Absolute,
                            ..default()
                        },
                        GlobalZIndex(0),
                    ))
                    .observe(on_tile_click)
                    .id();
                let label = commands
                    .spawn((
                        Name::new(format!("grid label {name}")),
                        Text::new(name.clone()),
                        ui_text(&assets, 13.0, false),
                        ui_color(false),
                        TextLayout::no_wrap(),
                        Node {
                            position_type: PositionType::Absolute,
                            overflow: Overflow::clip(),
                            ..default()
                        },
                        Pickable::IGNORE,
                        GlobalZIndex(1),
                    ))
                    .id();
                let close = commands
                    .spawn((
                        Name::new(format!("grid close {name}")),
                        GridTileClose(name.clone()),
                        Text::new("\u{d7}"),
                        ui_text(&assets, 15.0, false),
                        ui_color(false),
                        TextLayout::no_wrap(),
                        Node {
                            position_type: PositionType::Absolute,
                            padding: UiRect::horizontal(Val::Px(4.0)),
                            ..default()
                        },
                        GlobalZIndex(2),
                    ))
                    .observe(on_tile_close)
                    .id();
                grid.tiles.push(Tile {
                    doc: name.clone(),
                    layer,
                    camera,
                    node,
                    label,
                    close,
                    image,
                    size,
                    built: None,
                    parts: Vec::new(),
                    frames_left: SETTLE_FRAMES,
                    changed_at: None,
                    parts_n: 0,
                    author: None,
                    view: Transform::default(),
                });
                grid.tiles.len() - 1
            }
        };
        let confirming = grid.confirm_close.as_deref() == Some(name.as_str());
        let tile = &mut grid.tiles[idx];
        if tile.built.is_none()
            && let Some(t) = recency.changed.get(name)
        {
            tile.changed_at = Some(*t);
        }
        // Place the image and its label.
        if let Ok((mut node, mut img)) = nodes.get_mut(tile.node) {
            let want = (
                Val::Px(left),
                Val::Px(top + LABEL_H),
                Val::Px(cw),
                Val::Px(img_h),
            );
            if (node.left, node.top, node.width, node.height) != want {
                (node.left, node.top, node.width, node.height) = want;
            }
            if tile.size != size {
                tile.size = size;
                tile.image = images.add(view_target(size.x, size.y));
                img.image = tile.image.clone();
                if let Ok((_, _, mut target)) = tile_cams.get_mut(tile.camera) {
                    *target = RenderTarget::Image(tile.image.clone().into());
                }
                tile.frames_left = SETTLE_FRAMES;
            }
        }
        // Rebuild the tile's parts when its document changed.
        let key = (model.version, model.history.nodes.len());
        if tile.built != Some(key) {
            let first = tile.built.is_none();
            tile.built = Some(key);
            if !first {
                tile.changed_at = Some(Instant::now());
            }
            if let Some(t) = recency.changed.get(name) {
                tile.changed_at = Some(*t);
            }
            tile.author = model
                .history
                .current
                .and_then(|c| model.history.nodes.get(c))
                .and_then(|n| n.author.clone());
            for e in tile.parts.drain(..) {
                commands.entity(e).despawn();
            }
            // An inactive document restored from the autosave may hold
            // only its ops: derive its parts on a copy.
            let derived;
            let model = if model.parts.is_empty() && !model.ops.is_empty() {
                let mut m = model.clone_without_history();
                let _ = m.refresh();
                derived = m;
                &derived
            } else {
                model
            };
            let visible: Vec<&modeler::Part> = model
                .parts
                .iter()
                .map(|p| &**p)
                .filter(|p| {
                    !p.verts.is_empty()
                        && !p.indices.is_empty()
                        && crate::preview::app::drawn(model, p)
                })
                .collect();
            modeler::material::pbr::prebake(visible.iter().copied());
            tile.parts_n = visible.len();
            for part in &visible {
                let e = commands
                    .spawn((
                        part_bundle(
                            part,
                            &mut meshes,
                            &mut materials,
                            &mut images,
                            &mut pbr_cache,
                        ),
                        RenderLayers::layer(tile.layer),
                    ))
                    .id();
                tile.parts.push(e);
            }
            // Frame a three-quarter view on the document's bounds.
            if let Some((lo, hi)) = model.bounds() {
                let c = (lo + hi) * 0.5;
                let radius = ((hi - lo).length() * 0.5).max(0.01);
                let aspect = (cw / img_h.max(1.0)).max(0.2);
                // Fit the bounding sphere in the narrower field of view.
                let half_v = 0.5 * std::f32::consts::FRAC_PI_4;
                let half = if aspect < 1.0 {
                    (half_v.tan() * aspect).atan()
                } else {
                    half_v
                };
                let dist = radius / half.sin() * 1.05;
                let dir = Quat::from_euler(EulerRot::YXZ, 0.62, -0.42, 0.0) * Vec3::Z;
                tile.view = Transform::from_translation(c + dir * dist).looking_at(c, Vec3::Y);
            }
            tile.frames_left = SETTLE_FRAMES;
        }
        if (refresh_labels || tile.frames_left == SETTLE_FRAMES)
            && let Ok((mut text, mut node)) = texts.get_mut(tile.label)
        {
            {
                let by = tile
                    .author
                    .as_deref()
                    .map(|a| format!(" · by {a}"))
                    .unwrap_or_default();
                let when = tile
                    .changed_at
                    .map(|t| format!(" · {}", ago(t)))
                    .unwrap_or_default();
                let n = tile.parts_n;
                let t = format!(
                    "{name} · {n} part{}{by}{when}",
                    if n == 1 { "" } else { "s" }
                );
                if text.0 != t {
                    text.0 = t;
                }
                let want = (Val::Px(left + 2.0), Val::Px(top + 3.0), Val::Px(cw - 4.0));
                if (node.left, node.top, node.width) != want {
                    (node.left, node.top, node.width) = want;
                }
            }
        }
        if let Ok((mut text, mut node)) = texts.get_mut(tile.close) {
            let t = if confirming {
                "close unsaved? \u{d7}"
            } else {
                "\u{d7}"
            };
            if text.0 != t {
                text.0 = t.into();
            }
            let want = (
                Val::Px(top + 1.0),
                Val::Px((win_w - (left + cw)).max(0.0) + 2.0),
            );
            if (node.top, node.right) != want {
                (node.top, node.right) = want;
            }
        }
        // Render only while settling.
        if let Ok((mut cam, mut tf, _)) = tile_cams.get_mut(tile.camera) {
            if *tf != tile.view {
                *tf = tile.view;
                tile.frames_left = tile.frames_left.max(SETTLE_FRAMES);
            }
            let on = tile.frames_left > 0;
            if cam.is_active != on {
                cam.is_active = on;
            }
        }
        tile.frames_left = tile.frames_left.saturating_sub(1);
    }
}

fn despawn_tile(commands: &mut Commands, t: &Tile) {
    for e in t
        .parts
        .iter()
        .chain([&t.camera, &t.node, &t.label, &t.close])
    {
        commands.entity(*e).despawn();
    }
}

fn on_tile_click(
    click: On<PointerClick>,
    tiles: Query<&GridTileNode>,
    mut modeler: ResMut<Modeler>,
    mut grid: ResMut<Grid>,
) {
    let Ok(t) = tiles.get(click.entity) else {
        return;
    };
    let s = &mut modeler.session;
    if s.model.name != t.0 {
        match s.scene("switch", Some(&t.0)) {
            Ok(_) => s.mark_autosave(true, true, false),
            Err(e) => eprintln!("switching document: {e}"),
        }
    }
    grid.on = false;
}

/// The close button: closes a saved document at once, an unsaved one on a
/// second click (closing drops its autosave).
fn on_tile_close(
    mut click: On<PointerClick>,
    buttons: Query<&GridTileClose>,
    mut modeler: ResMut<Modeler>,
    mut grid: ResMut<Grid>,
) {
    click.propagate(false);
    let Ok(b) = buttons.get(click.entity) else {
        return;
    };
    let s = &mut modeler.session;
    let Some(m) = std::iter::once(&s.model)
        .chain(s.others.iter())
        .find(|m| m.name == b.0)
    else {
        return;
    };
    let saved = m.ops.is_empty() && m.parts.is_empty()
        || modeler::catalog::with_project(m.project.clone(), || {
            modeler::catalog::matches_prefab_file(m)
        });
    if !saved && grid.confirm_close.as_deref() != Some(b.0.as_str()) {
        grid.confirm_close = Some(b.0.clone());
        return;
    }
    grid.confirm_close = None;
    match s.scene("close", Some(&b.0)) {
        Ok(_) => s.mark_autosave(true, true, false),
        Err(e) => eprintln!("closing document: {e}"),
    }
}
