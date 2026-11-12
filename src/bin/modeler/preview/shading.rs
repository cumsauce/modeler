//! The HUD row at the 3D view's top right ([`HudRow`], in the Centre slot)
//! and what this file puts in it: the View menu (`Workbench.shading`: Lit
//! as exported, Clay one grey, By part flat colours) and, in Map mode, the
//! Fly / Orbit segment (F toggles; `camera.rs` reads [`CameraNav`]). Other
//! panes add their chips to the row after these (the comments chip sits at
//! the far right).
//!
//! Shading swaps `MeshMaterial3d` on `PreviewPart` entities. Each part's lit
//! handle is recorded in [`LitMaterial`] when it spawns (before the
//! selection pulse in panel.rs copies it), so Lit always restores it.

use std::hash::{Hash, Hasher};

use crate::prelude::*;

/// The row of HUD chips over the 3D view's top right. Add chips as
/// children; they line up right to left in spawn order reversed (the last
/// child is rightmost).
#[derive(Component)]
pub(crate) struct HudRow;

/// A part's material as built (Lit).
#[derive(Component)]
pub(crate) struct LitMaterial(pub(crate) Handle<StandardMaterial>);

/// The shading a part currently wears.
#[derive(Component)]
struct ShadedAs(Shading);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum HudButton {
    View,
    Pick(Shading),
    Nav(bool),
}

#[derive(Component)]
struct ViewMenu;

#[derive(Component)]
struct ViewLabel;

#[derive(Component)]
struct NavSegment;

/// The View menu is open.
#[derive(Resource, Default)]
struct MenuOpen(bool);

/// Shared Clay / By part materials.
#[derive(Resource, Default)]
struct ShadeMats {
    clay: Option<Handle<StandardMaterial>>,
    parts: Vec<Handle<StandardMaterial>>,
}

const PART_COLOURS: usize = 12;

impl Shading {
    fn label(self) -> &'static str {
        match self {
            Shading::Lit => "Lit",
            Shading::Clay => "Clay",
            Shading::Parts => "By part",
        }
    }
    fn about(self) -> &'static str {
        match self {
            Shading::Lit => "materials and lights as exported",
            Shading::Clay => "one neutral grey: the shape alone",
            Shading::Parts => "each part its own flat colour",
        }
    }
}

pub(crate) fn shading_plugin(app: &mut App) {
    app.init_resource::<MenuOpen>()
        .init_resource::<ShadeMats>()
        .init_resource::<CameraNav>()
        .add_systems(Startup, setup_hud)
        .add_systems(
            Update,
            (
                (record_lit, apply_shading)
                    .chain()
                    .after(rebuild_scene)
                    .before(highlight_selected),
                (nav_key, sync_hud).chain(),
            )
                .run_if(windowed),
        );
}

fn setup_hud(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut icons: ResMut<ds::Icons>,
    mut images: ResMut<Assets<Image>>,
) {
    let row = commands
        .spawn((
            Name::new("hud row"),
            HudRow,
            InSlot(PaneSlot::Centre),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(10.0),
                right: Val::Px(10.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    // Fly / Orbit (Map mode only): a chip-styled segment of two tabs.
    let seg = commands
        .spawn((
            NavSegment,
            Node {
                flex_direction: FlexDirection::Row,
                padding: UiRect::all(Val::Px(2.0)),
                column_gap: Val::Px(2.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(ds::RADIUS)),
                display: Display::None,
                ..default()
            },
            BackgroundColor(ds::CHIP_BG),
            BorderColor::all(ds::LINE),
            BlocksCamera,
            ChildOf(row),
        ))
        .id();
    for (fly, label) in [(true, "Fly"), (false, "Orbit")] {
        let b = commands
            .spawn((ds::piece(ds::Kind::Tab), HudButton::Nav(fly), ChildOf(seg)))
            .insert(Node {
                padding: UiRect::axes(Val::Px(9.0), Val::Px(3.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            })
            .observe(on_hud_click)
            .id();
        commands.spawn((
            ds::text(&assets, ds::Type::Small, label),
            ds::Ink::Main,
            Pickable::IGNORE,
            ChildOf(b),
        ));
    }
    // The View chip and its menu.
    let view = commands
        .spawn((
            ds::piece(ds::Kind::Chip),
            HudButton::View,
            BlocksCamera,
            ChildOf(row),
        ))
        .observe(on_hud_click)
        .id();
    commands.spawn((
        ds::icon(&mut icons, &mut images, ds::Icon::View, 13.0, ds::DIM),
        ds::Ink::Icon,
        Pickable::IGNORE,
        ChildOf(view),
    ));
    commands.spawn((
        ViewLabel,
        ds::text(&assets, ds::Type::Small, "Lit"),
        ds::Ink::Main,
        Pickable::IGNORE,
        ChildOf(view),
    ));
    commands.spawn((
        ds::icon(&mut icons, &mut images, ds::Icon::Chevron, 11.0, ds::DIM),
        ds::Ink::Meta,
        Pickable::IGNORE,
        ChildOf(view),
    ));
    let menu = commands
        .spawn((
            ViewMenu,
            InSlot(PaneSlot::Centre),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(44.0),
                right: Val::Px(10.0),
                width: Val::Px(250.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                padding: UiRect::all(Val::Px(5.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(ds::PANEL),
            BorderColor::all(ds::LINE),
            BlocksCamera,
            GlobalZIndex(20),
            Visibility::Hidden,
        ))
        .id();
    for s in [Shading::Lit, Shading::Clay, Shading::Parts] {
        let item = commands
            .spawn((ds::piece(ds::Kind::Row), HudButton::Pick(s), ChildOf(menu)))
            .insert(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexStart,
                row_gap: Val::Px(2.0),
                padding: UiRect::axes(Val::Px(9.0), Val::Px(6.0)),
                border_radius: BorderRadius::all(Val::Px(5.0)),
                ..default()
            })
            .observe(on_hud_click)
            .id();
        commands.spawn((
            ds::text(&assets, ds::Type::Body, s.label()),
            ds::Ink::Main,
            Pickable::IGNORE,
            ChildOf(item),
        ));
        commands.spawn((
            ds::text_wrap(&assets, ds::Type::Meta, s.about()),
            ds::Ink::Meta,
            Pickable::IGNORE,
            ChildOf(item),
        ));
    }
}

fn on_hud_click(
    click: On<PointerClick>,
    which: Query<&HudButton>,
    mut wb: ResMut<Workbench>,
    mut open: ResMut<MenuOpen>,
    mut nav: ResMut<CameraNav>,
) {
    let Ok(b) = which.get(click.entity) else {
        return;
    };
    match *b {
        HudButton::View => open.0 = !open.0,
        HudButton::Pick(s) => {
            wb.shading = s;
            open.0 = false;
        }
        HudButton::Nav(fly) => nav.fly = fly,
    }
}

/// F toggles Fly / Orbit in Map mode.
fn nav_key(
    keys: Res<ButtonInput<KeyCode>>,
    typing: Res<PinTyping>,
    grid: Res<Grid>,
    wb: Res<Workbench>,
    mut nav: ResMut<CameraNav>,
) {
    if !typing.open && !grid.on && wb.mode == Mode::Map && keys.just_pressed(KeyCode::KeyF) {
        nav.fly = !nav.fly;
    }
}

#[allow(clippy::type_complexity)]
fn sync_hud(
    wb: Res<Workbench>,
    nav: Res<CameraNav>,
    mut open: ResMut<MenuOpen>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut label: Query<&mut Text, With<ViewLabel>>,
    mut menu: Query<&mut Visibility, With<ViewMenu>>,
    mut seg: Query<&mut Node, With<NavSegment>>,
    mut items: Query<(&HudButton, &mut ds::Ui)>,
) {
    // A click in the 3D view closes the menu.
    if open.0 && mouse.just_pressed(MouseButton::Left) && !wb.ui_hover {
        open.0 = false;
    }
    let shows_3d = wb.mode != Mode::Material;
    if !shows_3d {
        open.0 = false;
    }
    if let Ok(mut t) = label.single_mut() {
        let want = wb.shading.label().to_string();
        if t.0 != want {
            t.0 = want;
        }
    }
    if let Ok(mut v) = menu.single_mut() {
        v.set_if_neq(if open.0 {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    if let Ok(mut n) = seg.single_mut() {
        let want = if wb.mode == Mode::Map {
            Display::Flex
        } else {
            Display::None
        };
        if n.display != want {
            n.display = want;
        }
    }
    for (b, mut bg) in &mut items {
        let on = match *b {
            HudButton::Pick(s) => s == wb.shading,
            HudButton::Nav(fly) => fly == nav.fly,
            HudButton::View => continue,
        };
        if bg.on != on {
            bg.on = on;
        }
    }
    // The View chip hides with the 3D view (Material shows the graph).
}

/// Remembers every new part's lit material, before anything swaps it.
fn record_lit(
    mut commands: Commands,
    new: Query<
        (Entity, &MeshMaterial3d<StandardMaterial>),
        (Added<PreviewPart>, Without<LitMaterial>),
    >,
    children: Query<&Children>,
    mats: Query<&MeshMaterial3d<StandardMaterial>>,
) {
    for (e, m) in &new {
        commands.entity(e).insert(LitMaterial(m.0.clone()));
    }
    // Marker previews: a PreviewPart root with mesh children.
    let _ = (&children, &mats);
}

fn part_colour(i: usize) -> Color {
    let h = (i as f32 / PART_COLOURS as f32) * 360.0;
    Color::hsl(h, 0.55, 0.6)
}

/// Puts every part in the chosen shading.
#[allow(clippy::type_complexity)]
fn apply_shading(
    mut commands: Commands,
    wb: Res<Workbench>,
    mut shade: ResMut<ShadeMats>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut parts: Query<
        (
            Entity,
            &Name,
            &mut MeshMaterial3d<StandardMaterial>,
            &LitMaterial,
            Option<&ShadedAs>,
            Has<Highlight>,
        ),
        With<PreviewPart>,
    >,
) {
    let want = wb.shading;
    for (e, name, mut mat, lit, shaded, highlighted) in &mut parts {
        let now = shaded.map_or(Shading::Lit, |s| s.0);
        if now == want {
            continue;
        }
        if highlighted {
            // The selection pulse wears its own copy: drop it, the pulse
            // copies the new material next.
            materials.remove(&mat.0);
            commands.entity(e).remove::<Highlight>();
        }
        mat.0 = match want {
            Shading::Lit => lit.0.clone(),
            Shading::Clay => shade
                .clay
                .get_or_insert_with(|| {
                    materials.add(StandardMaterial {
                        base_color: Color::srgb(0.62, 0.62, 0.6),
                        perceptual_roughness: 0.85,
                        ..default()
                    })
                })
                .clone(),
            Shading::Parts => {
                if shade.parts.is_empty() {
                    shade.parts = (0..PART_COLOURS)
                        .map(|i| {
                            materials.add(StandardMaterial {
                                base_color: part_colour(i),
                                perceptual_roughness: 0.9,
                                ..default()
                            })
                        })
                        .collect();
                }
                let mut h = std::collections::hash_map::DefaultHasher::new();
                name.as_str().hash(&mut h);
                shade.parts[(h.finish() as usize) % PART_COLOURS].clone()
            }
        };
        commands.entity(e).insert(ShadedAs(want));
    }
}
