//! The left pane: what is in the active document, for the current mode.
//!
//! - Model: the part tree (`modeler::outline`): prefab instances and array
//!   runs fold into groups, hidden parts sink into a dimmed group. Every
//!   row has a type badge, so indentation only means nesting.
//! - Material: the materials this document uses (swatch, the parts each
//!   colours), then the library (working materials and the catalog). A
//!   click makes one the active material (what the graph shows).
//! - Animate: the clips (a click plays one), then each skeleton's bones.
//! - Map: layers with eye toggles ([`LayerVisibility`], view only), then
//!   the prefab instances and markers (a click selects and frames one).
//!
//! The list is rebuilt only when what it shows changes (the model, the
//! mode, an open / closed group, a layer); selection restyles rows in place.
//! Double-click frames a row; right-click comments on it.

use std::collections::HashSet;

use bevy::feathers::constants::icons;
use modeler::AnnotationTarget;
use modeler::outline::{self, Row};

use crate::prelude::*;

/// Which kinds of things the 3D view draws (Map mode's eye toggles). View
/// only: nothing here changes the document or its exports.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct LayerVisibility {
    pub(crate) terrain: bool,
    pub(crate) prefabs: bool,
    /// Markers' `preview` prefabs (what game code builds there).
    pub(crate) game_built: bool,
    pub(crate) markers: bool,
    pub(crate) colliders: bool,
    pub(crate) lights: bool,
}

impl Default for LayerVisibility {
    fn default() -> Self {
        LayerVisibility {
            terrain: true,
            prefabs: true,
            game_built: true,
            markers: true,
            colliders: true,
            lights: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Layer {
    Terrain,
    Prefabs,
    GameBuilt,
    Markers,
    Colliders,
    Lights,
}

impl LayerVisibility {
    pub(crate) fn get(&self, l: Layer) -> bool {
        match l {
            Layer::Terrain => self.terrain,
            Layer::Prefabs => self.prefabs,
            Layer::GameBuilt => self.game_built,
            Layer::Markers => self.markers,
            Layer::Colliders => self.colliders,
            Layer::Lights => self.lights,
        }
    }

    fn flip(&mut self, l: Layer) {
        let v = match l {
            Layer::Terrain => &mut self.terrain,
            Layer::Prefabs => &mut self.prefabs,
            Layer::GameBuilt => &mut self.game_built,
            Layer::Markers => &mut self.markers,
            Layer::Colliders => &mut self.colliders,
            Layer::Lights => &mut self.lights,
        };
        *v = !*v;
    }

    /// Whether the view draws `part` of `model`: a terrain's part, a prefab
    /// instance's parts (`inst.part`).
    pub(crate) fn shows_part(&self, model: &Model, part: &str) -> bool {
        if self.terrain && self.prefabs {
            return true;
        }
        for o in &model.ops {
            match o {
                Op::Terrain { name, .. } if !self.terrain && name == part => return false,
                Op::Prefab { name, .. }
                    if !self.prefabs
                        && part
                            .strip_prefix(name.as_str())
                            .is_some_and(|r| r.starts_with('.')) =>
                {
                    return false;
                }
                _ => {}
            }
        }
        true
    }
}

/// The kind of thing a row is (its badge).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Icon {
    Part,
    Instance,
    Array,
    Hidden,
    Material,
    Library,
    Clip,
    Skeleton,
    Bone,
    Terrain,
    GameBuilt,
    Marker,
    Collider,
    Light,
}

impl Icon {
    fn ds(self) -> ds::Icon {
        match self {
            Icon::Part => ds::Icon::Part,
            Icon::Instance => ds::Icon::House,
            Icon::Array => ds::Icon::Block,
            Icon::Hidden => ds::Icon::Hide,
            Icon::Material | Icon::Library => ds::Icon::Swatch,
            Icon::Clip => ds::Icon::Play,
            Icon::Skeleton | Icon::Bone => ds::Icon::Bone,
            Icon::Terrain => ds::Icon::Mount,
            Icon::GameBuilt => ds::Icon::Cube,
            Icon::Marker => ds::Icon::Pin,
            Icon::Collider => ds::Icon::Zone,
            Icon::Light => ds::Icon::Light,
        }
    }
}

/// What a click on a row does.
#[derive(Clone, Debug, PartialEq)]
enum Act {
    /// Select these parts (Model rows, material rows' parts).
    Parts,
    /// Make this material active.
    Material(String),
    /// Play this clip.
    Clip(String),
    /// A bone: highlight; double-click frames its head.
    Bone(Vec3),
    /// Flip this layer.
    Layer(Layer),
    /// A marker: select, frame its spot.
    Spot(Vec3, f32),
    /// A section heading.
    Heading,
}

#[derive(Clone, Debug)]
struct Item {
    key: String,
    label: String,
    detail: String,
    depth: usize,
    icon: Option<Icon>,
    swatch: Option<[f32; 3]>,
    dim: bool,
    /// Has children (shows a chevron); `open` when they are listed.
    group: bool,
    open: bool,
    parts: Vec<String>,
    act: Act,
}

impl Item {
    fn new(key: String, label: impl Into<String>, act: Act) -> Self {
        Item {
            key,
            label: label.into(),
            detail: String::new(),
            depth: 0,
            icon: None,
            swatch: None,
            dim: false,
            group: false,
            open: false,
            parts: Vec::new(),
            act,
        }
    }

    fn heading(label: &str) -> Self {
        Item::new(format!("h:{label}"), label, Act::Heading)
    }
}

#[derive(Resource, Default)]
pub(crate) struct OutlinePane {
    pub(crate) visible: bool,
    /// Cursor is over the pane: the 3D camera and pins ignore the mouse.
    pub(crate) hover: bool,
    /// Open groups, by row key.
    pub(crate) expanded: HashSet<String>,
    /// The selected row's key and the parts it covers (they pulse in 3D).
    pub(crate) selected: Option<String>,
    pub(crate) selected_parts: Vec<String>,
    /// What the list was built for (see `want_key`).
    shown: Option<u64>,
    /// Bumped by clicks that change the list's shape (groups, layers).
    generation: u64,
    /// `Workbench.selected` as last seen (a change made elsewhere, a 3D
    /// click or Esc, moves the row selection with it).
    last_wb: Option<String>,
    items: Vec<Item>,
}

impl OutlinePane {
    /// Open at start unless MODELER_OUTLINE=0.
    pub(crate) fn open(visible: bool) -> Self {
        OutlinePane {
            visible,
            ..default()
        }
    }

    fn item(&self, key: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.key == key)
    }

    fn toggle(&mut self, key: &str) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.to_string());
        }
        self.generation += 1;
    }
}

#[derive(Component)]
pub(crate) struct OutlineHud;

#[derive(Component)]
pub(crate) struct OutlineList;

#[derive(Component)]
pub(crate) struct OutlineTitle;

#[derive(Component)]
pub(crate) struct OutlineRow {
    key: String,
}

/// The chevron of a group row (a click there only opens / closes).
#[derive(Component)]
pub(crate) struct OutlineChevron;

const ROW_H: f32 = 26.0;
const INDENT: f32 = 14.0;

fn hash_of(v: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// Keeps the list in step with the model and the mode; wheel scrolls;
/// restyles the selection in place.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_outline_pane(
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    modeler: Res<Modeler>,
    mut pane: ResMut<OutlinePane>,
    mut wb: ResMut<Workbench>,
    (layers, playback): (Option<Res<LayerVisibility>>, Res<Playback>),
    mut hud: Query<&mut Visibility, With<OutlineHud>>,
    mut list: Query<(Entity, &mut ScrollPosition), With<OutlineList>>,
    mut title: Query<&mut Text, With<OutlineTitle>>,
    mut rows: Query<(&OutlineRow, &mut ds::Ui)>,
    assets: Res<AssetServer>,
    mut commands: Commands,
    (mut icons, mut images): (ResMut<ds::Icons>, ResMut<Assets<Image>>),
) {
    if modeler.session.viewer_away() {
        return;
    }
    let Ok(mut vis) = hud.single_mut() else {
        return;
    };
    let shown = pane.visible;
    vis.set_if_neq(if shown {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
    if !shown {
        pane.hover = false;
        return;
    }
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    pane.hover = cursor.is_some_and(|c| wb.rects.left.contains(c));
    let Ok((list_e, mut pos)) = list.single_mut() else {
        return;
    };
    if pane.hover {
        let lines = scroll_lines(&scroll);
        if lines != 0.0 {
            pos.y = (pos.y - lines * ROW_H * 1.5).max(0.0);
        }
    }
    let layers = layers.map(|l| *l).unwrap_or_default();
    let session = &modeler.session;
    let model = &session.model;

    // A selection made elsewhere (a 3D click, Esc) moves the row with it.
    if wb.selected != pane.last_wb {
        pane.last_wb = wb.selected.clone();
        match &wb.selected {
            Some(p) => {
                pane.selected = Some(format!("part:{p}"));
                pane.selected_parts = vec![p.clone()];
            }
            None => {
                pane.selected = None;
                pane.selected_parts.clear();
            }
        }
    }

    let want = hash_of((
        wb.mode,
        &model.name,
        model.version,
        pane.generation,
        layers,
        (wb.mode == Mode::Material).then(|| session.materials.fingerprint()),
    ));
    if pane.shown != Some(want) {
        pane.shown = Some(want);
        if let Ok(mut t) = title.single_mut() {
            let s = match wb.mode {
                Mode::Model => "Parts",
                Mode::Material => "Materials",
                Mode::Animate => "Animation",
                Mode::Map => "Map",
            };
            if t.0 != s {
                t.0 = s.into();
            }
        }
        let items = match wb.mode {
            Mode::Model => model_items(model, &pane.expanded),
            Mode::Material => material_items(session, &pane.expanded),
            Mode::Animate => animate_items(model, &pane.expanded),
            Mode::Map => map_items(model, &layers),
        };
        pane.items = items;
        // Keep the selection if its row still exists.
        if let Some(k) = pane.selected.clone()
            && !k.starts_with("part:")
            && pane.item(&k).is_none()
        {
            pane.selected = None;
            pane.selected_parts.clear();
        }
        spawn_rows(
            &mut commands,
            list_e,
            &pane.items,
            &layers,
            &assets,
            (&mut icons, &mut images),
        );
        return;
    }

    // Selection styling, in place.
    let active_mat = session.materials.active.as_deref();
    let clip = playback.clip.as_deref();
    let sel = pane.selected.as_deref();
    for (row, mut bg) in &mut rows {
        let k = row.key.as_str();
        let on = sel == Some(k)
            || k.strip_prefix("mat:")
                .is_some_and(|m| Some(m) == active_mat)
            || k.strip_prefix("lib:")
                .is_some_and(|m| Some(m) == active_mat)
            || k.strip_prefix("clip:").is_some_and(|c| Some(c) == clip);
        if bg.on != on {
            bg.on = on;
        }
    }
    let _ = &mut wb;
}

fn icon_for(r: &Row) -> Icon {
    if r.key.starts_with("prefab:") {
        Icon::Instance
    } else if r.key.starts_with("array:") {
        Icon::Array
    } else if r.key == "hidden" {
        Icon::Hidden
    } else {
        Icon::Part
    }
}

fn model_items(model: &Model, expanded: &HashSet<String>) -> Vec<Item> {
    let tree = outline::build(model);
    outline::flatten(&tree, &|k| expanded.contains(k))
        .into_iter()
        .map(|(depth, r)| {
            let mut it = Item::new(r.key.clone(), r.label.clone(), Act::Parts);
            it.detail = r.detail.clone();
            it.depth = depth;
            it.icon = Some(icon_for(r));
            it.dim = r.dim;
            it.group = !r.children.is_empty();
            it.open = expanded.contains(&r.key);
            it.parts = r.parts.clone();
            it
        })
        .collect()
}

/// A pbr part's mean baked colour (sRGB), from the bake cache the preview
/// fills anyway; its base colour factor alone is white.
fn baked_swatch(p: &Part) -> Option<[f32; 4]> {
    let ch = modeler::material::pbr::bake_part(p).ok()??;
    let n = ch.base_color.len().max(1) as f32;
    let m = ch
        .base_color
        .iter()
        .fold([0.0f32; 3], |a, c| [a[0] + c[0], a[1] + c[1], a[2] + c[2]]);
    let c = Color::linear_rgb(m[0] / n, m[1] / n, m[2] / n).to_srgba();
    Some([c.red, c.green, c.blue, 1.0])
}

fn material_items(session: &Session, expanded: &HashSet<String>) -> Vec<Item> {
    let model = &session.model;
    // Materials in use, in part order, with the parts each colours.
    let mut used: Vec<(String, [f32; 3], Vec<String>)> = Vec::new();
    for p in model.parts.iter().filter(|p| p.visible) {
        let name = match &p.material.pbr {
            Some(r) => r.material.clone(),
            None => {
                let c = p.material.base_color;
                format!(
                    "flat #{:02x}{:02x}{:02x}",
                    (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0) as u8
                )
            }
        };
        let c = baked_swatch(p).unwrap_or(p.material.base_color);
        match used.iter_mut().find(|(n, _, _)| *n == name) {
            Some((_, _, parts)) => parts.push(p.name.clone()),
            None => used.push((name, [c[0], c[1], c[2]], vec![p.name.clone()])),
        }
    }
    let mut out = vec![Item::heading("In this document")];
    for (name, swatch, parts) in &used {
        let key = format!("mat:{name}");
        let open = expanded.contains(&key);
        let flat = name.starts_with("flat ");
        let mut it = Item::new(
            key.clone(),
            name.clone(),
            if flat {
                Act::Parts
            } else {
                Act::Material(name.clone())
            },
        );
        it.icon = Some(Icon::Material);
        it.swatch = Some(*swatch);
        it.detail = format!(
            "{} part{}",
            parts.len(),
            if parts.len() == 1 { "" } else { "s" }
        );
        it.group = true;
        it.open = open;
        it.parts = parts.clone();
        out.push(it);
        if open {
            for p in parts {
                let mut c = Item::new(format!("part:{p}"), p.clone(), Act::Parts);
                c.depth = 1;
                c.icon = Some(Icon::Part);
                c.parts = vec![p.clone()];
                out.push(c);
            }
        }
    }
    if used.is_empty() {
        let mut e = Item::new("none".into(), "no parts yet", Act::Heading);
        e.dim = true;
        out.push(e);
    }
    // The library: working materials, then the catalog's.
    let mut lib: Vec<(String, &'static str)> = session
        .materials
        .docs
        .keys()
        .map(|k| (k.clone(), "working"))
        .collect();
    let catalog = modeler::catalog::with_project(model.project.clone(), || {
        modeler::catalog::list_kind(modeler::catalog::Kind::Materials)
    })
    .unwrap_or_default();
    for e in catalog {
        if !lib.iter().any(|(n, _)| *n == e.name) {
            lib.push((e.name, "library"));
        }
    }
    lib.retain(|(n, _)| !used.iter().any(|(u, _, _)| u == n));
    lib.sort();
    out.push(Item::heading("Library"));
    for (name, detail) in lib {
        let mut it = Item::new(format!("lib:{name}"), name.clone(), Act::Material(name));
        it.icon = Some(Icon::Library);
        it.detail = detail.into();
        out.push(it);
    }
    out
}

fn animate_items(model: &Model, expanded: &HashSet<String>) -> Vec<Item> {
    let mut out = vec![Item::heading("Clips")];
    for o in &model.ops {
        let (name, detail) = match o {
            Op::Clip { name, def } => (name, format!("{:.2} s", def.duration)),
            Op::Sway { name, .. } => (name, "sway".to_string()),
            _ => continue,
        };
        let mut it = Item::new(
            format!("clip:{name}"),
            name.clone(),
            Act::Clip(name.clone()),
        );
        it.icon = Some(Icon::Clip);
        it.detail = detail;
        out.push(it);
    }
    if out.len() == 1 {
        let mut e = Item::new("noclip".into(), "no clips yet", Act::Heading);
        e.dim = true;
        out.push(e);
    }
    out.push(Item::heading("Bones"));
    let skels = modeler::anim::skeletons(model).unwrap_or_default();
    for s in &skels {
        let key = format!("skel:{}", s.name);
        // Skeletons start open.
        let open = !expanded.contains(&key);
        let mut it = Item::new(key.clone(), s.name.clone(), Act::Heading);
        it.act = Act::Bone(s.bones.first().map_or(Vec3::ZERO, |b| b.head));
        it.icon = Some(Icon::Skeleton);
        it.detail = format!("{} bones", s.bones.len());
        it.group = !s.bones.is_empty();
        it.open = open;
        out.push(it);
        if !open {
            continue;
        }
        fn walk(
            s: &modeler::anim::Skeleton,
            parent: Option<usize>,
            depth: usize,
            expanded: &HashSet<String>,
            out: &mut Vec<Item>,
        ) {
            for (i, b) in s.bones.iter().enumerate() {
                if b.parent != parent {
                    continue;
                }
                let key = format!("bone:{}:{}", s.name, b.name);
                let kids = s.bones.iter().any(|c| c.parent == Some(i));
                // Bones start open, to a few levels.
                let open = kids && (expanded.contains(&key) != (depth < 4));
                let mut it = Item::new(key, b.name.clone(), Act::Bone(b.head));
                it.icon = Some(Icon::Bone);
                it.depth = depth;
                it.group = kids;
                it.open = open;
                out.push(it);
                if open {
                    walk(s, Some(i), depth + 1, expanded, out);
                }
            }
        }
        walk(s, None, 1, expanded, &mut out);
    }
    if skels.is_empty() {
        let mut e = Item::new("noskel".into(), "no skeleton", Act::Heading);
        e.dim = true;
        out.push(e);
    }
    out
}

fn map_items(model: &Model, layers: &LayerVisibility) -> Vec<Item> {
    let markers = modeler::game::markers(model).unwrap_or_default();
    let colliders = modeler::game::colliders(model).map_or(0, |c| c.len());
    let count = |f: fn(&Op) -> bool| model.ops.iter().filter(|o| f(o)).count();
    let built = markers.iter().filter(|m| m.preview.is_some()).count();
    let mut out = vec![Item::heading("Layers")];
    for (layer, label, icon, n) in [
        (
            Layer::Terrain,
            "Terrain",
            Icon::Terrain,
            count(|o| matches!(o, Op::Terrain { .. })),
        ),
        (
            Layer::Prefabs,
            "Prefab instances",
            Icon::Instance,
            count(|o| matches!(o, Op::Prefab { .. })),
        ),
        (Layer::GameBuilt, "Game-built", Icon::GameBuilt, built),
        (Layer::Markers, "Markers", Icon::Marker, markers.len()),
        (Layer::Colliders, "Colliders", Icon::Collider, colliders),
        (
            Layer::Lights,
            "Lights",
            Icon::Light,
            count(|o| matches!(o, Op::Light { .. })),
        ),
    ] {
        let mut it = Item::new(format!("layer:{label}"), label, Act::Layer(layer));
        it.icon = Some(icon);
        let on = layers.get(layer);
        it.detail = format!("{n}  {}", if on { "shown" } else { "hidden" });
        it.dim = !on;
        out.push(it);
    }
    out.push(Item::heading("Instances and markers"));
    for o in &model.ops {
        if let Op::Prefab { name, prefab, .. } = o {
            let head = format!("{name}.");
            let parts: Vec<String> = model
                .parts
                .iter()
                .filter(|p| p.name.starts_with(&head))
                .map(|p| p.name.clone())
                .collect();
            let mut it = Item::new(format!("inst:{name}"), name.clone(), Act::Parts);
            it.icon = Some(Icon::Instance);
            it.detail = prefab.clone();
            it.parts = parts;
            out.push(it);
        }
    }
    for m in &markers {
        let r = m
            .size
            .map_or(3.0, |s| Vec3::from(s).length().max(1.0) * 1.5);
        let mut it = Item::new(
            format!("marker:{}", m.name),
            m.name.clone(),
            Act::Spot(m.position, r),
        );
        match &m.preview {
            Some(p) => {
                it.icon = Some(Icon::GameBuilt);
                it.detail = p.clone();
            }
            None => {
                it.icon = Some(Icon::Marker);
                it.detail = m.kind.clone();
            }
        }
        out.push(it);
    }
    out
}

fn spawn_rows(
    commands: &mut Commands,
    list_e: Entity,
    items: &[Item],
    layers: &LayerVisibility,
    assets: &AssetServer,
    (icons, images): (&mut ds::Icons, &mut Assets<Image>),
) {
    let _ = layers;
    commands.entity(list_e).despawn_children();
    let chevron_right: Handle<Image> = assets.load(icons::CHEVRON_RIGHT);
    let chevron_down: Handle<Image> = assets.load(icons::CHEVRON_DOWN);
    for it in items {
        if it.act == Act::Heading && it.key.starts_with("h:") {
            commands.spawn((
                ds::text(assets, ds::Type::Caption, it.label.clone()),
                Node {
                    margin: UiRect {
                        top: Val::Px(14.0),
                        bottom: Val::Px(4.0),
                        left: Val::Px(12.0),
                        ..default()
                    },
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(list_e),
            ));
            continue;
        }
        let row = commands
            .spawn((
                OutlineRow {
                    key: it.key.clone(),
                },
                ds::piece(ds::Kind::Row),
                ChildOf(list_e),
            ))
            .insert((Node {
                height: Val::Px(ROW_H),
                min_height: Val::Px(ROW_H),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(7.0),
                padding: UiRect {
                    left: Val::Px(4.0 + it.depth as f32 * INDENT),
                    right: Val::Px(8.0),
                    ..default()
                },
                border_radius: BorderRadius::all(Val::Px(5.0)),
                ..default()
            },))
            .observe(on_row_click)
            .id();
        let mut chev = commands.spawn((
            Node {
                width: Val::Px(12.0),
                height: Val::Px(12.0),
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(row),
        ));
        if it.group {
            chev.insert((
                ImageNode::new(if it.open {
                    chevron_down.clone()
                } else {
                    chevron_right.clone()
                })
                .with_color(ds::FAINT),
                OutlineChevron,
            ));
        }
        if let Some(icon) = it.icon
            && it.swatch.is_none()
        {
            commands.spawn((
                ds::icon(icons, images, icon.ds(), 14.0, ds::DIM),
                if it.dim {
                    ds::Ink::Fixed
                } else {
                    ds::Ink::Meta
                },
                Pickable::IGNORE,
                ChildOf(row),
            ));
        }
        if let Some([cr, cg, cb]) = it.swatch {
            commands.spawn((
                Node {
                    width: Val::Px(14.0),
                    height: Val::Px(14.0),
                    flex_shrink: 0.0,
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(cr, cg, cb).with_alpha(if it.dim { 0.4 } else { 1.0 })),
                Pickable::IGNORE,
                ChildOf(row),
            ));
        }
        commands.spawn((
            ds::text_in(
                assets,
                ds::Type::Body,
                it.label.clone(),
                if it.dim { ds::FAINT } else { ds::FG },
            ),
            if it.dim {
                ds::Ink::Fixed
            } else {
                ds::Ink::Main
            },
            Node {
                flex_shrink: 1.0,
                overflow: Overflow::clip(),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(row),
        ));
        commands.spawn((
            Node {
                flex_grow: 1.0,
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(row),
        ));
        commands.spawn((
            ds::text(assets, ds::Type::Meta, it.detail.clone()),
            ds::Ink::Meta,
            Pickable::IGNORE,
            ChildOf(row),
        ));
    }
}

fn frame(orbit: &mut Orbit, c: Vec3, radius: f32) {
    orbit.target = c;
    orbit.dist = (radius * 2.4).max(0.3);
    orbit.user = true;
}

/// Row clicks: the chevron opens / closes a group; elsewhere the row's
/// action (select, make active, play, flip a layer); a second click on a
/// selected group opens it; a double click frames the row.
#[allow(clippy::too_many_arguments)]
fn on_row_click(
    click: On<PointerClick>,
    rows: Query<&OutlineRow>,
    chevrons: Query<&OutlineChevron>,
    mut modeler: ResMut<Modeler>,
    mut pane: ResMut<OutlinePane>,
    mut wb: ResMut<Workbench>,
    mut orbit: ResMut<Orbit>,
    mut comment: ResMut<CommentBox>,
    (mut layers, mut playback): (Option<ResMut<LayerVisibility>>, ResMut<Playback>),
) {
    let target = click.original_event_target();
    let Ok(row) = rows.get(click.event_target()) else {
        return;
    };
    let Some(item) = pane.item(&row.key).cloned() else {
        return;
    };
    if click.button == PointerButton::Secondary {
        if item.parts.is_empty() {
            return;
        }
        comment.request(if item.group || item.parts.len() > 1 {
            AnnotationTarget::Group {
                label: item.label.clone(),
                parts: item.parts.clone(),
            }
        } else {
            AnnotationTarget::Part(item.parts[0].clone())
        });
        return;
    }
    if click.button != PointerButton::Primary {
        return;
    }
    if chevrons.get(target).is_ok() {
        pane.toggle(&item.key);
        return;
    }
    let double = click.count >= 2;
    match &item.act {
        Act::Heading => return,
        Act::Layer(l) => {
            if let Some(layers) = layers.as_mut() {
                layers.flip(*l);
            }
            return;
        }
        Act::Material(name) => {
            let s = &mut modeler.session;
            if s.materials.docs.contains_key(name) {
                s.materials.active = Some(name.clone());
            } else {
                let p = s.model.project.clone();
                let r = modeler::catalog::with_project(p, || s.materials.load(name));
                if let Err(e) = r {
                    eprintln!("loading material `{name}`: {e}");
                }
            }
            s.mark_autosave(false, false, true);
        }
        Act::Clip(name) => {
            playback.clip = Some(name.clone());
            playback.time = 0.0;
            playback.playing = true;
        }
        Act::Bone(at) if double => frame(&mut orbit, *at, 0.3),
        Act::Spot(at, r) if double => frame(&mut orbit, *at, *r),
        Act::Spot(..) | Act::Bone(_) => {}
        Act::Parts => {
            if double && let Some((min, max)) = modeler.session.model.bounds_of(&item.parts) {
                frame(&mut orbit, (min + max) * 0.5, (max - min).length() * 0.5);
            } else if !double && item.group && pane.selected.as_deref() == Some(item.key.as_str()) {
                pane.toggle(&item.key);
            }
        }
    }
    // Instances and markers frame on a single click too (Map).
    if wb.mode == Mode::Map && !double {
        match &item.act {
            Act::Spot(at, r) => frame(&mut orbit, *at, *r),
            Act::Parts => {
                if let Some((min, max)) = modeler.session.model.bounds_of(&item.parts) {
                    frame(&mut orbit, (min + max) * 0.5, (max - min).length() * 0.5);
                }
            }
            _ => {}
        }
    }
    pane.selected = Some(item.key.clone());
    pane.selected_parts = item.parts.clone();
    // A single part selects it for the right pane.
    let part = (item.parts.len() == 1 && !item.group).then(|| item.parts[0].clone());
    wb.selected = part.clone();
    pane.last_wb = part;
}

/// The pane's UI tree.
pub(crate) fn setup_outline_pane(mut commands: Commands, assets: Res<AssetServer>) {
    commands.init_resource::<LayerVisibility>();
    let pane = commands
        .spawn((
            Name::new("left pane"),
            OutlineHud,
            InSlot(PaneSlot::Left),
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
            pane_bg(),
        ))
        .id();
    commands.spawn((
        OutlineList,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(HEADER_H + 4.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            bottom: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            // Rows inset from the pane edge (padding, not row margins: a
            // margin on a zero-width list makes a negative row, which
            // Bevy's border-radius clamp panics on).
            padding: UiRect::horizontal(Val::Px(6.0)),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        ScrollPosition::default(),
        ChildOf(pane),
    ));
    spawn_pane_header(
        &mut commands,
        &assets,
        pane,
        PaneToggle::Outline,
        (Text::new("Parts"), OutlineTitle),
    );
}
