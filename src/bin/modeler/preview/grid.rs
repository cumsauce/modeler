//! All documents (G): every open document as a tile, grouped by project,
//! with a filter bar on top and an agent feed down the right, so a human
//! can watch several agents build at once.
//!
//! Cheap by design. Tiles are laid out in a scrolling list (wheel, Page
//! Up / Down, Home / End), and only tiles on screen exist: each is its own
//! camera on its own render layer (the document's parts spawned there),
//! rendering into its own image, and active only for a few frames after
//! its document changes (or the tile is resized or scrolled in); the rest
//! of the time the image keeps its last frame. A tile scrolled out of view
//! is despawned, camera and parts. An idle grid costs no 3D rendering at
//! all, and the window's own camera (TAA + SSR) is paused while the grid
//! is up. Tile cameras are lean: deferred like the window (so materials
//! shade the same) but no SSR, TAA or SSAO, and their light casts no
//! shadows.
//!
//! Click a tile to make that document active and leave the grid; its ×
//! closes the document (an unsaved one asks inside the tile first). The
//! feed lists the latest authored history steps across open documents.

use std::time::Instant;

use bevy::camera::visibility::RenderLayers;
use bevy::feathers::constants::icons;
use bevy::feathers::controls::{
    ButtonVariant, FeathersButton, FeathersTextInput, FeathersTextInputContainer,
    FeathersToolButton,
};
use bevy::feathers::display::{caption, icon};
use bevy::input_focus::InputFocus;
use bevy::picking::events::PointerClick;
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;

use crate::prelude::*;

/// Steady slots kept for the tab strip / top bar ([`Recency::slots`]).
const MAX_TILES: usize = 9;
/// Tiles alive at once (render layers `FIRST_LAYER..`): the ones on screen.
const MAX_LIVE: usize = 24;
const FIRST_LAYER: usize = 10;
/// Frames a tile camera stays active after a change (pipelines and the
/// deferred passes settle within these).
const SETTLE_FRAMES: u32 = 12;
const GAP: f32 = 12.0;
const MIN_TILE_W: f32 = 250.0;
const INFO_H: f32 = 48.0;
const BAR_H: f32 = 44.0;
const SECTION_H: f32 = 30.0;
const FEED_W: f32 = 280.0;
/// A document changed this recently is "agent working".
const WORKING_SECS: u64 = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum GridFilter {
    #[default]
    All,
    Working,
    Unsaved,
}

#[derive(Resource)]
pub(crate) struct Grid {
    pub(crate) on: bool,
    tiles: Vec<Tile>,
    /// An unsaved document whose × was clicked: its tile asks first.
    confirm_close: Option<String>,
    filter: GridFilter,
    query: String,
    scroll: f32,
    ui: Option<GridUi>,
}

struct GridUi {
    root: Entity,
    content: Entity,
    feed: Entity,
    input_box: Entity,
    count: Entity,
    sections: Vec<(String, Entity)>,
    /// What the feed was built from.
    feed_key: u64,
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
            filter: GridFilter::All,
            query: String::new(),
            scroll: 0.0,
            ui: None,
        }
    }
}

struct Tile {
    doc: String,
    layer: usize,
    camera: Entity,
    card: Entity,
    thumb: Entity,
    name: Entity,
    kind: Entity,
    meta: Entity,
    working: Entity,
    confirm: Entity,
    image: Handle<Image>,
    size: UVec2,
    /// (version, history length) the parts were spawned from.
    built: Option<(u64, usize)>,
    parts: Vec<Entity>,
    frames_left: u32,
    parts_n: usize,
    /// Where the tile camera should be (applied once the camera exists:
    /// it is spawned by deferred commands).
    view: Transform,
}

/// A tile's card (clicking it switches to `0`).
#[derive(Component)]
pub(crate) struct GridTileNode(String);

/// A tile's × (closes document `0`, an unsaved one after asking).
#[derive(Component)]
pub(crate) struct GridTileClose(String);

/// The confirm box's buttons: close `0` (true) or keep it (false).
#[derive(Component)]
pub(crate) struct GridConfirm(String, bool);

/// Clicks inside these don't switch to the tile's document.
#[derive(Component)]
pub(crate) struct GridNoSwitch;

/// A filter button.
#[derive(Component)]
pub(crate) struct GridFilterButton(GridFilter);

/// A feed row: clicking opens document `0`.
#[derive(Component)]
pub(crate) struct GridFeedRow(String);

#[derive(Component)]
pub(crate) struct GridTileCamera;

/// How often the app loop runs. While the grid is up nothing animates
/// (tiles render only on change): ~30 fps. Otherwise full rate while
/// anything moves (input, the camera, a model change, playback, agent
/// shots) and for two seconds after, so TAA and SSR settle; then the loop
/// idles at 4 fps. Input and server calls wake it at once.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_mode(
    grid: Res<Grid>,
    orbit: Res<crate::preview::camera::Orbit>,
    playback: Res<crate::preview::playback::Playback>,
    modeler: Res<Modeler>,
    shots: Res<crate::preview::agent_rig::PendingAgentShots>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    scroll: Res<bevy::input::mouse::AccumulatedMouseScroll>,
    mut last: Local<(Option<Instant>, Option<(u64, [u32; 6])>)>,
    winit: Option<ResMut<bevy::winit::WinitSettings>>,
) {
    let Some(mut w) = winit else {
        return;
    };
    let stamp = (
        modeler.session.model.version,
        [
            orbit.yaw,
            orbit.pitch,
            orbit.dist,
            orbit.target.x,
            orbit.target.y,
            orbit.target.z,
        ]
        .map(f32::to_bits),
    );
    let active = last.1 != Some(stamp)
        || !orbit.user
        || playback.playing
        || !shots.0.is_empty()
        || keys.get_pressed().next().is_some()
        || mouse.get_pressed().next().is_some()
        || motion.delta != Vec2::ZERO
        || scroll.delta != Vec2::ZERO;
    last.1 = Some(stamp);
    if active {
        last.0 = Some(Instant::now());
    }
    let busy = last
        .0
        .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2));
    let want = if grid.on {
        bevy::winit::UpdateMode::reactive(std::time::Duration::from_millis(33))
    } else if busy {
        bevy::winit::UpdateMode::Continuous
    } else {
        bevy::winit::UpdateMode::reactive(std::time::Duration::from_millis(250))
    };
    if w.focused_mode != want {
        w.focused_mode = want;
        w.unfocused_mode = want;
    }
}

pub(crate) fn grid_off(grid: Res<Grid>) -> bool {
    !grid.on
}

/// The window's scene is rebuilt when it shows, and while agent shots are
/// waiting (they render it from their own camera, grid or not).
pub(crate) fn scene_wanted(
    grid: Res<Grid>,
    shots: Res<crate::preview::agent_rig::PendingAgentShots>,
) -> bool {
    !grid.on || !shots.0.is_empty()
}

pub(crate) fn toggle_grid_key(
    keys: Res<ButtonInput<KeyCode>>,
    typing: Res<PinTyping>,
    focus: Option<Res<InputFocus>>,
    mut grid: ResMut<Grid>,
) {
    // Typing in a text box (the grid's filter, the document popup) is not a key.
    let typing_text = focus.is_some_and(|f| f.get().is_some());
    if keys.just_pressed(KeyCode::KeyG) && !typing.open && !typing_text {
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

fn hash_of(v: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// A steady colour per agent (the feed's dots).
fn agent_color(name: &str) -> Color {
    ds::agent_color(name)
}

fn project_of(m: &Model) -> String {
    match &m.project {
        None => "shared library".to_string(),
        Some(p) => modeler::catalog::project_name(p),
    }
}

fn kind_of(m: &Model) -> &'static str {
    if m.ops
        .iter()
        .any(|o| matches!(o, Op::Terrain { .. } | Op::Marker { .. }))
    {
        "map"
    } else if m.ops.iter().any(|o| matches!(o, Op::Skeleton { .. })) {
        "rig"
    } else {
        "model"
    }
}

fn author_of(m: &Model) -> Option<&str> {
    m.history
        .current
        .and_then(|c| m.history.nodes.get(c))
        .and_then(|n| n.author.as_deref())
}

fn unsaved(s: &Session, m: &Model) -> bool {
    !(m.ops.is_empty() && m.parts.is_empty()) && !s.is_saved_doc(&m.name)
}

/// When a document last changed: seen by the window, or a call working in it.
fn last_change(s: &Session, recency: &Recency, name: &str) -> Option<Instant> {
    match (recency.changed.get(name).copied(), s.last_touched(name)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn set_node(n: &mut Node, left: f32, top: f32, w: f32, h: f32) {
    let want = (Val::Px(left), Val::Px(top), Val::Px(w), Val::Px(h));
    if (n.left, n.top, n.width, n.height) != want {
        (n.left, n.top, n.width, n.height) = want;
    }
}

fn set_text(t: &mut Text, s: String) {
    if t.0 != s {
        t.0 = s;
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn update_grid(
    mut commands: Commands,
    mut grid: ResMut<Grid>,
    modeler: Res<Modeler>,
    view: Res<View3d>,
    (windows, keys, scroll, focus): (
        Query<&Window, With<PrimaryWindow>>,
        Res<ButtonInput<KeyCode>>,
        Res<AccumulatedMouseScroll>,
        Option<Res<InputFocus>>,
    ),
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
    (mut nodes, mut img_nodes, mut texts, mut vis): (
        Query<&mut Node>,
        Query<&mut ImageNode>,
        Query<&mut Text>,
        Query<&mut Visibility, Without<View3dNode>>,
    ),
    (mut filter_buttons, inputs): (
        Query<(&GridFilterButton, &mut ds::Ui)>,
        Query<(&EditableText, &ChildOf)>,
    ),
    recency: Res<Recency>,
    mut label_tick: Local<Option<Instant>>,
) {
    let Ok((mut main, env)) = main_cam.single_mut() else {
        return;
    };
    let env = env.cloned();
    if grid.ui.is_none() {
        grid.ui = Some(spawn_grid_ui(&mut commands, &assets));
    }
    let Some(root) = grid.ui.as_ref().map(|u| u.root) else {
        return;
    };
    if !grid.on {
        if !grid.tiles.is_empty() {
            for t in grid.tiles.drain(..) {
                despawn_tile(&mut commands, &t);
            }
        }
        if let Ok(mut v) = vis.get_mut(root) {
            v.set_if_neq(Visibility::Hidden);
        }
        if !main.is_active {
            main.is_active = true;
        }
        for mut v in &mut main_node {
            v.set_if_neq(Visibility::Inherited);
        }
        return;
    }
    if let Ok(mut v) = vis.get_mut(root) {
        v.set_if_neq(Visibility::Inherited);
    }
    // The window camera rests while the grid is up (a screenshot then
    // captures the window, tiles and all).
    if main.is_active {
        main.is_active = false;
    }
    for mut v in &mut main_node {
        v.set_if_neq(Visibility::Hidden);
    }
    let sf = windows.single().map(|w| w.scale_factor()).unwrap_or(1.0);
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    let s = &modeler.session;
    let grid = &mut *grid;
    let Some(ui) = grid.ui.as_mut() else {
        return;
    };

    // The filter text and buttons.
    if let Some((t, _)) = inputs.iter().find(|(_, p)| p.parent() == ui.input_box) {
        let q = t.value().to_string();
        if q != grid.query {
            grid.query = q;
            grid.scroll = 0.0;
        }
    }
    for (b, mut ui) in &mut filter_buttons {
        let on = b.0 == grid.filter;
        if ui.on != on {
            ui.on = on;
        }
    }

    // The documents shown, by project, most recently changed first.
    let q = grid.query.to_lowercase();
    let model_of = |name: &str| -> Option<&Model> {
        if s.model.name == name {
            Some(&s.model)
        } else {
            s.others.iter().find(|m| m.name == name)
        }
    };
    let total = recency.order.len();
    let mut sections: Vec<(String, Vec<&Model>)> = Vec::new();
    for name in &recency.order {
        let Some(m) = model_of(name) else {
            continue;
        };
        if !q.is_empty() && !m.name.to_lowercase().contains(&q) {
            continue;
        }
        let keep = match grid.filter {
            GridFilter::All => true,
            GridFilter::Working => last_change(s, &recency, &m.name)
                .is_some_and(|t| t.elapsed().as_secs() < WORKING_SECS),
            GridFilter::Unsaved => unsaved(s, m),
        };
        if !keep {
            continue;
        }
        let p = project_of(m);
        match sections.iter_mut().find(|(n, _)| *n == p) {
            Some((_, v)) => v.push(m),
            None => sections.push((p, vec![m])),
        }
    }
    let shown: usize = sections.iter().map(|(_, v)| v.len()).sum();

    // Layout in content coordinates (under the bar, left of the feed).
    let r = view.rect;
    let area_w = (r.width() - FEED_W).max(MIN_TILE_W * 0.5);
    let area_h = (r.height() - BAR_H).max(40.0);
    let cols = (((area_w - GAP) / (MIN_TILE_W + GAP)).floor() as usize).max(1);
    let cw = ((area_w - GAP * (cols as f32 + 1.0)) / cols as f32).max(60.0);
    let thumb_h = (cw * 10.0 / 16.0).round();
    let ch = thumb_h + INFO_H;
    let mut y = GAP * 0.5;
    let mut places: Vec<(&Model, f32, f32)> = Vec::new();
    let mut heads: Vec<(String, f32)> = Vec::new();
    for (p, docs) in &sections {
        heads.push((format!("{p} · {}", docs.len()), y));
        y += SECTION_H;
        for (i, m) in docs.iter().enumerate() {
            let left = GAP + (i % cols) as f32 * (cw + GAP);
            let top = y + (i / cols) as f32 * (ch + GAP);
            places.push((m, left, top));
        }
        y += docs.len().div_ceil(cols) as f32 * (ch + GAP);
    }
    let content_h = y + GAP;

    // Scrolling: the wheel over the tiles, Page Up / Down, Home / End.
    let over = cursor.is_some_and(|c| {
        c.x >= r.min.x && c.x <= r.min.x + area_w && c.y >= r.min.y + BAR_H && c.y <= r.max.y
    });
    let lines = if over { scroll_lines(&scroll) } else { 0.0 };
    let mut sc = grid.scroll - lines * 60.0;
    let typing_text = focus.is_some_and(|f| f.get().is_some());
    if !typing_text {
        if keys.just_pressed(KeyCode::PageDown) {
            sc += area_h * 0.9;
        }
        if keys.just_pressed(KeyCode::PageUp) {
            sc -= area_h * 0.9;
        }
        if keys.just_pressed(KeyCode::Home) {
            sc = 0.0;
        }
        if keys.just_pressed(KeyCode::End) {
            sc = content_h;
        }
    }
    grid.scroll = sc.clamp(0.0, (content_h - area_h).max(0.0));
    let scroll_y = grid.scroll;

    if let Ok(mut t) = texts.get_mut(ui.count) {
        set_text(&mut t, {
            let mut c = if shown == total {
                format!("{total} documents")
            } else {
                format!("{shown} of {total} documents")
            };
            // Unsaved work a restart parked: found in the popup.
            let parked = modeler.session.parked.len();
            if parked > 0 {
                c += &format!(" · {parked} recovered after restart (Cmd+K)");
            }
            c
        });
    }

    // Section headings.
    let mut keep = Vec::new();
    for (label, top) in &heads {
        let e = match ui.sections.iter().position(|(l, _)| l == label) {
            Some(i) => ui.sections.swap_remove(i).1,
            None => commands
                .spawn((
                    Text::new(label.clone()),
                    ui_text(&assets, 12.0, true),
                    ui_color(true),
                    TextLayout::no_wrap(),
                    Node {
                        position_type: PositionType::Absolute,
                        ..default()
                    },
                    Pickable::IGNORE,
                    ChildOf(ui.content),
                ))
                .id(),
        };
        if let Ok(mut n) = nodes.get_mut(e) {
            let want = (Val::Px(GAP + 2.0), Val::Px(top + 10.0 - scroll_y));
            if (n.left, n.top) != want {
                (n.left, n.top) = want;
            }
        }
        keep.push((label.clone(), e));
    }
    for (_, e) in ui.sections.drain(..) {
        commands.entity(e).despawn();
    }
    ui.sections = keep;

    // The feed, rebuilt when any document's history moves.
    let feed_key = hash_of(
        recency
            .order
            .iter()
            .filter_map(|n| model_of(n))
            .map(|m| (&m.name, m.history.nodes.len(), m.history.current))
            .collect::<Vec<_>>(),
    );
    if ui.feed_key != feed_key {
        ui.feed_key = feed_key;
        build_feed(&mut commands, &assets, ui.feed, s, &recency, &model_of);
    }

    // Tiles on screen, at most MAX_LIVE; the rest are despawned.
    let visible: Vec<(&Model, f32, f32)> = places
        .into_iter()
        .filter(|(_, _, top)| top - scroll_y < area_h && top + ch - scroll_y > 0.0)
        .take(MAX_LIVE)
        .collect();
    let mut kept = Vec::new();
    for t in grid.tiles.drain(..) {
        if visible.iter().any(|(m, _, _)| m.name == t.doc) {
            kept.push(t);
        } else {
            despawn_tile(&mut commands, &t);
        }
    }
    grid.tiles = kept;
    // Tile cameras borrow the window camera's filtered studio light; wait
    // for it (a few frames after startup).
    let Some(env) = env else {
        return;
    };
    let refresh_labels = label_tick.is_none_or(|t| t.elapsed().as_millis() >= 500);
    if refresh_labels {
        *label_tick = Some(Instant::now());
    }
    let content = ui.content;
    for (model, left, top) in visible {
        let name = &model.name;
        let size = UVec2::new(
            (((cw - 2.0) * sf).round() as u32).max(16),
            ((thumb_h * sf).round() as u32).max(16),
        );
        let idx = match grid.tiles.iter().position(|t| &t.doc == name) {
            Some(k) => k,
            None => {
                let used: Vec<usize> = grid.tiles.iter().map(|t| t.layer).collect();
                let Some(layer) = (FIRST_LAYER..FIRST_LAYER + MAX_LIVE).find(|l| !used.contains(l))
                else {
                    continue;
                };
                let image = images.add(view_target(size.x, size.y));
                let tile = spawn_tile(
                    &mut commands,
                    &assets,
                    &env,
                    name,
                    layer,
                    image,
                    size,
                    content,
                );
                grid.tiles.push(tile);
                grid.tiles.len() - 1
            }
        };
        let confirming = grid.confirm_close.as_deref() == Some(name.as_str());
        let tile = &mut grid.tiles[idx];
        let fresh = tile.built.is_none();
        if let Ok(mut n) = nodes.get_mut(tile.card) {
            set_node(&mut n, left, top - scroll_y, cw, ch);
        }
        if let Ok(mut n) = nodes.get_mut(tile.thumb) {
            let want = Val::Px(thumb_h);
            if n.height != want {
                n.height = want;
            }
        }
        if tile.size != size {
            tile.size = size;
            tile.image = images.add(view_target(size.x, size.y));
            if let Ok(mut img) = img_nodes.get_mut(tile.thumb) {
                img.image = tile.image.clone();
            }
            if let Ok((_, _, mut target)) = tile_cams.get_mut(tile.camera) {
                *target = RenderTarget::Image(tile.image.clone().into());
            }
            tile.frames_left = SETTLE_FRAMES;
        }
        if let Ok(mut v) = vis.get_mut(tile.confirm) {
            v.set_if_neq(if confirming {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            });
        }
        // Rebuild the tile's parts when its document changed.
        let key = (model.version, model.history.nodes.len());
        if tile.built != Some(key) {
            tile.built = Some(key);
            for e in tile.parts.drain(..) {
                commands.entity(e).despawn();
            }
            pbr_cache.tick();
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
            let drawn = crate::preview::app::drawn(model);
            let parts: Vec<&modeler::Part> = model
                .parts
                .iter()
                .map(|p| &**p)
                .filter(|p| !p.verts.is_empty() && !p.indices.is_empty() && drawn(p))
                .collect();
            modeler::material::pbr::prebake(parts.iter().copied());
            tile.parts_n = parts.len();
            for part in &parts {
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
            for (at, parts) in crate::preview::app::marker_previews(model) {
                let root = commands
                    .spawn((at, Visibility::default(), RenderLayers::layer(tile.layer)))
                    .id();
                for part in &parts {
                    let child = commands
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
                    commands.entity(root).add_child(child);
                }
                tile.parts.push(root);
            }
            // Frame a three-quarter view on the document's bounds.
            if let Some((lo, hi)) = model.bounds() {
                let c = (lo + hi) * 0.5;
                let radius = ((hi - lo).length() * 0.5).max(0.01);
                let aspect = (cw / thumb_h.max(1.0)).max(0.2);
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
        if refresh_labels || fresh {
            let author = author_of(model);
            let when = last_change(s, &recency, name);
            let is_working = when.is_some_and(|t| t.elapsed().as_secs() < WORKING_SECS);
            if let Ok(mut t) = texts.get_mut(tile.name) {
                let active = if *name == s.model.name {
                    " (active)"
                } else {
                    ""
                };
                set_text(&mut t, format!("{name}{active}"));
            }
            if let Ok(mut t) = texts.get_mut(tile.kind) {
                let n = tile.parts_n;
                set_text(
                    &mut t,
                    format!(
                        "{} · {n} part{}",
                        kind_of(model),
                        if n == 1 { "" } else { "s" }
                    ),
                );
            }
            if let Ok(mut t) = texts.get_mut(tile.meta) {
                let mut m = project_of(model);
                if let Some(a) = author {
                    m.push_str(&format!(" · {a}"));
                }
                if let Some(w) = when {
                    m.push_str(&format!(" · {}", ago(w)));
                }
                if unsaved(s, model) {
                    m.push_str(" · unsaved");
                }
                set_text(&mut t, m);
            }
            if let Ok(mut t) = texts.get_mut(tile.working) {
                set_text(&mut t, format!("{} editing", author.unwrap_or("agent")));
            }
            if let Ok(mut v) = vis.get_mut(tile.working) {
                v.set_if_neq(if is_working {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                });
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

/// The grid's fixed UI: a root over the centre slot with the filter bar,
/// the clipped tile area and the feed column.
fn spawn_grid_ui(commands: &mut Commands, assets: &AssetServer) -> GridUi {
    let line = ds::LINE;
    let bg = ds::BG;
    let root = commands
        .spawn((
            Name::new("all documents"),
            InSlot(PaneSlot::Centre),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(bg),
            BlocksCamera,
            Visibility::Hidden,
        ))
        .id();
    let content = commands
        .spawn((
            Name::new("grid tiles"),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(BAR_H),
                right: Val::Px(FEED_W),
                bottom: Val::Px(0.0),
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    // The sticky bar: filter box, the three filters, a count.
    let bar = commands
        .spawn((
            Name::new("grid bar"),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                right: Val::Px(FEED_W),
                height: Val::Px(BAR_H),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                padding: UiRect::horizontal(Val::Px(GAP)),
                border: UiRect::bottom(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(bg),
            BorderColor::all(line),
            ChildOf(root),
        ))
        .id();
    commands.spawn((
        ds::text_in(assets, ds::Type::Small, "Filter", ds::DIM),
        Pickable::IGNORE,
        ChildOf(bar),
    ));
    let input_box = commands
        .spawn_scene(bsn! {
            @FeathersTextInputContainer
            Children [
                @FeathersTextInput
            ]
        })
        .insert(ChildOf(bar))
        .id();
    let seg = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                padding: UiRect::all(Val::Px(2.0)),
                column_gap: Val::Px(2.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                margin: UiRect::left(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(ds::PANEL),
            BorderColor::all(ds::LINE),
            ChildOf(bar),
        ))
        .id();
    for (f, label) in [
        (GridFilter::All, "All"),
        (GridFilter::Working, "Agent working"),
        (GridFilter::Unsaved, "Unsaved"),
    ] {
        let b = commands
            .spawn((ds::piece(ds::Kind::Tab), GridFilterButton(f), ChildOf(seg)))
            .insert(Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                border_radius: BorderRadius::all(Val::Px(5.0)),
                align_items: AlignItems::Center,
                ..default()
            })
            .observe(on_filter_click)
            .id();
        commands.spawn((
            ds::text(assets, ds::Type::Small, label),
            ds::Ink::Main,
            Pickable::IGNORE,
            ChildOf(b),
        ));
    }
    commands.spawn((
        Node {
            flex_grow: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(bar),
    ));
    let count = commands
        .spawn((
            Text::new(""),
            ui_text(assets, 12.0, false),
            ui_color(true),
            TextLayout::no_wrap(),
            Pickable::IGNORE,
            ChildOf(bar),
        ))
        .id();
    // The feed column.
    let side = commands
        .spawn((
            Name::new("grid feed"),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                width: Val::Px(FEED_W),
                flex_direction: FlexDirection::Column,
                border: UiRect::left(Val::Px(1.0)),
                ..default()
            },
            pane_bg(),
            BorderColor::all(line),
            ChildOf(root),
        ))
        .id();
    commands.spawn((
        ds::text(assets, ds::Type::Caption, "Agent activity"),
        Node {
            margin: UiRect {
                left: Val::Px(12.0),
                top: Val::Px(14.0),
                bottom: Val::Px(8.0),
                ..default()
            },
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(side),
    ));
    let feed = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                overflow: Overflow::scroll_y(),
                padding: UiRect::horizontal(Val::Px(12.0)),
                ..default()
            },
            ScrollPosition::default(),
            ChildOf(side),
        ))
        .id();
    GridUi {
        root,
        content,
        feed,
        input_box,
        count,
        sections: Vec::new(),
        feed_key: 0,
    }
}

/// The latest authored history steps across open documents, the most
/// recently changed document first.
fn build_feed<'a>(
    commands: &mut Commands,
    assets: &AssetServer,
    feed: Entity,
    s: &Session,
    recency: &Recency,
    model_of: &dyn Fn(&str) -> Option<&'a Model>,
) {
    commands.entity(feed).despawn_children();
    let mut rows = 0;
    for name in &recency.order {
        let Some(m) = model_of(name) else {
            continue;
        };
        let when = last_change(s, recency, name).map(ago);
        let steps: Vec<(&str, &str)> = m
            .history
            .nodes
            .iter()
            .rev()
            .filter_map(|n| n.author.as_deref().map(|a| (a, n.label.as_str())))
            .take(6)
            .collect();
        for (i, (author, label)) in steps.into_iter().enumerate() {
            let row = commands
                .spawn((
                    GridFeedRow(name.clone()),
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::FlexStart,
                        column_gap: Val::Px(8.0),
                        padding: UiRect::vertical(Val::Px(5.0)),
                        ..default()
                    },
                    ChildOf(feed),
                ))
                .observe(on_feed_click)
                .id();
            commands.spawn((
                Node {
                    width: Val::Px(8.0),
                    height: Val::Px(8.0),
                    margin: UiRect::top(Val::Px(4.0)),
                    flex_shrink: 0.0,
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(agent_color(author)),
                Pickable::IGNORE,
                ChildOf(row),
            ));
            let col = commands
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        flex_grow: 1.0,
                        flex_shrink: 1.0,
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    Pickable::IGNORE,
                    ChildOf(row),
                ))
                .id();
            // Plex's backtick combines onto the letter before it ("centrè").
            let mut label = label.replace('\n', " ").replace('`', "'");
            if label.chars().count() > 64 {
                label = label.chars().take(63).collect::<String>() + "\u{2026}";
            }
            commands.spawn((
                Text::new(format!("{author}  {label}")),
                ui_text(assets, 12.0, false),
                ui_color(false),
                Pickable::IGNORE,
                ChildOf(col),
            ));
            let meta = match (&when, i) {
                (Some(w), 0) => format!("{name} · {w}"),
                _ => name.clone(),
            };
            commands.spawn((
                Text::new(meta),
                ui_text(assets, 11.0, false),
                ui_color(true),
                TextLayout::no_wrap(),
                Pickable::IGNORE,
                ChildOf(col),
            ));
            rows += 1;
            if rows >= 80 {
                return;
            }
        }
    }
    if rows == 0 {
        commands.spawn((
            Text::new("No agent edits yet."),
            ui_text(assets, 12.0, false),
            ui_color(true),
            Pickable::IGNORE,
            ChildOf(feed),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_tile(
    commands: &mut Commands,
    assets: &AssetServer,
    env: &bevy::light::EnvironmentMapLight,
    name: &str,
    layer: usize,
    image: Handle<Image>,
    size: UVec2,
    content: Entity,
) -> Tile {
    let camera = commands
        .spawn((
            Name::new(format!("grid camera {name}")),
            GridTileCamera,
            Camera3d::default(),
            Camera {
                is_active: true,
                ..default()
            },
            // Deferred lighting needs the depth prepass too: without it
            // tiles shaded flat and washed out.
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
            // The window's lights: the shadowed sun (fixed in the world; the
            // tile view is fixed, so a constant local rotation keeps it
            // there) and the view fill.
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
    let card = commands
        .spawn((
            Name::new(format!("grid tile {name}")),
            GridTileNode(name.to_string()),
            Node {
                position_type: PositionType::Absolute,
                flex_direction: FlexDirection::Column,
                overflow: Overflow::clip(),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(10.0)),
                ..default()
            },
            pane_bg(),
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.1)),
            ChildOf(content),
        ))
        .observe(on_tile_click)
        .id();
    let thumb = commands
        .spawn((
            ImageNode::new(image.clone()),
            Node {
                width: Val::Percent(100.0),
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(card),
        ))
        .id();
    let working = commands
        .spawn((
            Text::new(""),
            ui_text(assets, 11.0, false),
            TextColor(Color::srgb(0.55, 0.9, 0.6)),
            TextLayout::no_wrap(),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                top: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(7.0), Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.08, 0.09, 0.11, 0.85)),
            Visibility::Hidden,
            Pickable::IGNORE,
            ChildOf(thumb),
        ))
        .id();
    // The ×, top right of the thumbnail.
    let x = commands
        .spawn((
            GridNoSwitch,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(6.0),
                top: Val::Px(6.0),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.08, 0.09, 0.11, 0.85)),
            ChildOf(thumb),
        ))
        .id();
    commands
        .spawn_scene(bsn! {
            @FeathersToolButton {
                @variant: ButtonVariant::Plain,
                @caption: bsn! { @icon(icons::X) }
            }
        })
        .insert((GridTileClose(name.to_string()), ChildOf(x)))
        .observe(on_tile_close);
    // The unsaved confirm, inside the tile.
    let confirm = commands
        .spawn((
            GridNoSwitch,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(6.0),
                right: Val::Px(6.0),
                bottom: Val::Px(6.0),
                padding: UiRect::all(Val::Px(8.0)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(7.0)),
                ..default()
            },
            pane_bg(),
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.12)),
            Visibility::Hidden,
            ChildOf(thumb),
        ))
        .id();
    commands.spawn((
        Text::new("Unsaved. Close and lose it?"),
        ui_text(assets, 12.0, false),
        ui_color(false),
        Node {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(confirm),
    ));
    commands
        .spawn_scene(bsn! {
            @FeathersButton {
                @variant: ButtonVariant::Normal,
                @caption: bsn! { @caption("Keep") }
            }
        })
        .insert((GridConfirm(name.to_string(), false), ChildOf(confirm)))
        .observe(on_confirm);
    commands
        .spawn_scene(bsn! {
            @FeathersButton {
                @variant: ButtonVariant::Primary,
                @caption: bsn! { @caption("Close") }
            }
        })
        .insert((GridConfirm(name.to_string(), true), ChildOf(confirm)))
        .observe(on_confirm);
    // Info rows: name and kind, then project · author · when · unsaved.
    let info = |commands: &mut Commands| {
        commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(8.0),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(3.0)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(card),
            ))
            .id()
    };
    let row1 = info(commands);
    let name_e = commands
        .spawn((
            Text::new(name.to_string()),
            ui_text(assets, 13.0, true),
            ui_color(false),
            TextLayout::no_wrap(),
            Node {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                overflow: Overflow::clip(),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(row1),
        ))
        .id();
    let kind = commands
        .spawn((
            Text::new(""),
            ui_text(assets, 11.0, false),
            ui_color(true),
            TextLayout::no_wrap(),
            Pickable::IGNORE,
            ChildOf(row1),
        ))
        .id();
    let row2 = info(commands);
    let meta = commands
        .spawn((
            Text::new(""),
            ui_text(assets, 11.0, false),
            ui_color(true),
            TextLayout::no_wrap(),
            Pickable::IGNORE,
            ChildOf(row2),
        ))
        .id();
    Tile {
        doc: name.to_string(),
        layer,
        camera,
        card,
        thumb,
        name: name_e,
        kind,
        meta,
        working,
        confirm,
        image,
        size,
        built: None,
        parts: Vec::new(),
        frames_left: SETTLE_FRAMES,
        parts_n: 0,
        view: Transform::default(),
    }
}

fn despawn_tile(commands: &mut Commands, t: &Tile) {
    for e in t.parts.iter().chain([&t.camera, &t.card]) {
        commands.entity(*e).despawn();
    }
}

fn switch_to(modeler: &mut Modeler, name: &str) {
    let s = &mut modeler.session;
    if s.model.name != name {
        match s.window_scene("switch", Some(name)) {
            Ok(_) => s.mark_autosave(true, true, false),
            Err(e) => eprintln!("switching document: {e}"),
        }
    }
}

fn on_tile_click(
    click: On<PointerClick>,
    tiles: Query<&GridTileNode>,
    no_switch: Query<(), With<GridNoSwitch>>,
    parents: Query<&ChildOf>,
    mut modeler: ResMut<Modeler>,
    mut grid: ResMut<Grid>,
) {
    if click.button != PointerButton::Primary {
        return;
    }
    if click.button != PointerButton::Primary {
        return;
    }
    let card = click.event_target();
    let Ok(t) = tiles.get(card) else {
        return;
    };
    // Clicks on the × or the confirm box are theirs.
    let mut e = click.original_event_target();
    while e != card {
        if no_switch.contains(e) {
            return;
        }
        match parents.get(e) {
            Ok(p) => e = p.parent(),
            Err(_) => break,
        }
    }
    switch_to(&mut modeler, &t.0);
    grid.confirm_close = None;
    grid.on = false;
}

/// The ×: closes a saved document at once; an unsaved one asks in its tile.
fn on_tile_close(
    activate: On<Activate>,
    buttons: Query<&GridTileClose>,
    mut modeler: ResMut<Modeler>,
    mut grid: ResMut<Grid>,
) {
    let Ok(b) = buttons.get(activate.event_target()) else {
        return;
    };
    let s = &modeler.session;
    let Some(m) = std::iter::once(&s.model)
        .chain(s.others.iter())
        .find(|m| m.name == b.0)
    else {
        return;
    };
    let saved = m.ops.is_empty() && m.parts.is_empty()
        || s.is_saved_doc(&m.name)
        || modeler::catalog::with_project(m.project.clone(), || {
            modeler::catalog::matches_prefab_file(m)
        });
    if !saved {
        grid.confirm_close = Some(b.0.clone());
        return;
    }
    let name = b.0.clone();
    close_doc(&mut modeler, &name);
}

fn on_confirm(
    activate: On<Activate>,
    buttons: Query<&GridConfirm>,
    mut modeler: ResMut<Modeler>,
    mut grid: ResMut<Grid>,
) {
    let Ok(GridConfirm(name, close)) = buttons.get(activate.event_target()) else {
        return;
    };
    grid.confirm_close = None;
    if *close {
        close_doc(&mut modeler, name);
    }
}

fn close_doc(modeler: &mut Modeler, name: &str) {
    let s = &mut modeler.session;
    match s.window_scene("close", Some(name)) {
        Ok(_) => s.mark_autosave(true, true, false),
        Err(e) => eprintln!("closing document: {e}"),
    }
}

fn on_filter_click(
    click: On<PointerClick>,
    buttons: Query<&GridFilterButton>,
    mut grid: ResMut<Grid>,
) {
    if click.button != PointerButton::Primary {
        return;
    }
    if let Ok(b) = buttons.get(click.event_target()) {
        grid.filter = b.0;
        grid.scroll = 0.0;
    }
}

fn on_feed_click(
    click: On<PointerClick>,
    rows: Query<&GridFeedRow>,
    mut modeler: ResMut<Modeler>,
    mut grid: ResMut<Grid>,
) {
    if let Ok(r) = rows.get(click.event_target()) {
        let name = r.0.clone();
        let open = modeler.session.model.name == name
            || modeler.session.others.iter().any(|m| m.name == name);
        if open {
            switch_to(&mut modeler, &name);
            grid.on = false;
        }
    }
}
