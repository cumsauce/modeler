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
}

impl Grid {
    /// Off at start unless MODELER_GRID=1.
    pub(crate) fn open(on: bool) -> Self {
        Grid {
            on,
            tiles: Vec::new(),
        }
    }
}

struct Tile {
    doc: String,
    layer: usize,
    camera: Entity,
    node: Entity,
    label: Entity,
    image: Handle<Image>,
    size: UVec2,
    /// (version, history length) the parts were spawned from.
    built: Option<(u64, usize)>,
    parts: Vec<Entity>,
    frames_left: u32,
    changed_at: Instant,
    parts_n: usize,
    author: Option<String>,
    /// Where the tile camera should be (applied once the camera exists:
    /// it is spawned by deferred commands).
    view: Transform,
}

/// Marks a tile's image node (clicking it switches to `0`).
#[derive(Component)]
pub(crate) struct GridTileNode(String);

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
    let s = &mut modeler.session;
    let names: Vec<String> = {
        let mut n = s.doc_names();
        n.truncate(MAX_TILES);
        n
    };
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
                        c.spawn((
                            DirectionalLight {
                                illuminance: 6000.0,
                                shadow_maps_enabled: false,
                                ..default()
                            },
                            RenderLayers::layer(layer),
                            Transform::from_rotation(Quat::from_euler(
                                EulerRot::YXZ,
                                0.5,
                                -0.6,
                                0.0,
                            )),
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
                grid.tiles.push(Tile {
                    doc: name.clone(),
                    layer,
                    camera,
                    node,
                    label,
                    image,
                    size,
                    built: None,
                    parts: Vec::new(),
                    frames_left: SETTLE_FRAMES,
                    changed_at: Instant::now(),
                    parts_n: 0,
                    author: None,
                    view: Transform::default(),
                });
                grid.tiles.len() - 1
            }
        };
        let tile = &mut grid.tiles[idx];
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
                tile.changed_at = Instant::now();
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
                let mut m = model.clone();
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
                .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty())
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
                let active = *name == s.model.name;
                let by = tile
                    .author
                    .as_deref()
                    .map(|a| format!(" · by {a}"))
                    .unwrap_or_default();
                let t = format!(
                    "{name}{} · {} parts{by} · {}",
                    if active { " (active)" } else { "" },
                    tile.parts_n,
                    ago(tile.changed_at)
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
    for e in t.parts.iter().chain([&t.camera, &t.node, &t.label]) {
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
