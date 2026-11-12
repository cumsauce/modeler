//! The top bar (design/workbench-concept.html): a segmented switch
//! [All documents (N open)] [project / document ▾], the mode buttons
//! Model / Material / Animate / Map (keys 1-4; unavailable ones dimmed with
//! a tooltip saying why), and the agents at the right as pills, with a live
//! dot while the document they work in changed in the last few seconds.
//! The document half opens the document popup (`doc_popup.rs`).
//!
//! The bar is rebuilt only when what it shows changes (a hash per frame).

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use bevy::feathers::constants::icons;

use crate::prelude::*;

/// A client counts as live this long after its document last changed.
pub(crate) const LIVE: Duration = Duration::from_secs(4);

/// What a top-bar item does when clicked.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TopButton {
    AllDocuments,
    Document,
    Mode(Mode),
}

/// Hovering this node shows its text as a tooltip.
#[derive(Component, Clone)]
pub(crate) struct TopTip(pub(crate) String);

#[derive(Component)]
struct TopBarRoot;

#[derive(Component)]
struct TopTooltip;

#[derive(Resource, Default)]
struct TopBarShown(Option<u64>);

pub(crate) fn top_bar_plugin(app: &mut App) {
    app.init_resource::<TopBarShown>()
        .add_systems(Startup, setup_top_bar)
        .add_systems(Update, (sync_top_bar, tooltip).run_if(windowed));
}

fn setup_top_bar(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn((
        Name::new("top bar"),
        TopBarRoot,
        InSlot(PaneSlot::TopBar),
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(14.0),
            padding: UiRect::horizontal(Val::Px(10.0)),
            border: UiRect::bottom(Val::Px(1.0)),
            ..default()
        },
        BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.08)),
    ));
    commands.spawn((
        Name::new("top bar tooltip"),
        TopTooltip,
        Text::new(""),
        ui_text(&assets, 12.0, false),
        TextColor(Color::srgb(0.92, 0.93, 0.95)),
        Node {
            position_type: PositionType::Absolute,
            padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.06, 0.07, 0.08, 0.95)),
        GlobalZIndex(30),
        Visibility::Hidden,
        Pickable::IGNORE,
    ));
}

/// The project label of a document (as the outliner names projects).
pub(crate) fn project_label(m: &Model) -> String {
    match &m.project {
        None => "shared library".to_string(),
        Some(p) => catalog::project_name(p),
    }
}

/// Clients and whether each is live, sorted by name.
fn agents(session: &Session, now: Instant) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = session
        .clients()
        .map(|(c, doc)| {
            let live = session
                .last_touched(doc)
                .is_some_and(|t| now.duration_since(t) < LIVE);
            (c.to_string(), live)
        })
        .collect();
    v.sort();
    v
}

#[allow(clippy::too_many_arguments)]
fn sync_top_bar(
    modeler: Res<Modeler>,
    wb: Res<Workbench>,
    grid: Res<Grid>,
    mut shown: ResMut<TopBarShown>,
    root: Query<Entity, With<TopBarRoot>>,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    let Ok(root) = root.single() else {
        return;
    };
    let s = &modeler.session;
    let model = &s.model;
    let now = Instant::now();
    let agents = agents(s, now);
    let avail: Vec<Result<(), &'static str>> = Mode::ALL.map(|m| mode_available(model, m)).to_vec();
    let open = 1 + s.others.len();
    let project = project_label(model);
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (grid.on, wb.mode, wb.doc_popup, &model.name, &project, open).hash(&mut h);
        avail.hash(&mut h);
        agents.hash(&mut h);
        h.finish()
    };
    if shown.0 == Some(key) {
        return;
    }
    shown.0 = Some(key);
    commands.entity(root).despawn_children();

    // The segmented switch.
    let seg = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                height: Val::Px(30.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.14)),
            ChildOf(root),
        ))
        .id();
    let half = |commands: &mut Commands, which: TopButton, on: bool| {
        commands
            .spawn((
                which,
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    padding: UiRect::horizontal(Val::Px(10.0)),
                    ..default()
                },
                BackgroundColor(if on {
                    Color::srgba(0.35, 0.62, 0.95, 0.28)
                } else {
                    Color::NONE
                }),
                ChildOf(seg),
            ))
            .observe(on_click)
            .id()
    };
    let all = half(&mut commands, TopButton::AllDocuments, grid.on);
    grid_icon(&mut commands, all);
    text(
        &mut commands,
        &assets,
        all,
        "All documents",
        13.0,
        false,
        false,
    );
    text(
        &mut commands,
        &assets,
        all,
        &format!("{open} open"),
        11.0,
        false,
        true,
    );
    commands.spawn((
        Node {
            width: Val::Px(1.0),
            ..default()
        },
        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.14)),
        ChildOf(seg),
    ));
    let doc = half(&mut commands, TopButton::Document, !grid.on || wb.doc_popup);
    commands
        .entity(doc)
        .insert(TopTip("Switch document  ⌘K".into()));
    text(
        &mut commands,
        &assets,
        doc,
        &format!("{project} /"),
        12.0,
        false,
        true,
    );
    text(&mut commands, &assets, doc, &model.name, 14.0, true, false);
    commands.spawn((
        Node {
            width: Val::Px(12.0),
            height: Val::Px(12.0),
            ..default()
        },
        ImageNode::new(assets.load(icons::CHEVRON_DOWN))
            .with_color(Color::srgba(0.85, 0.87, 0.9, 0.7)),
        Pickable::IGNORE,
        ChildOf(doc),
    ));

    // Modes.
    let modes = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(2.0),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    for (i, m) in Mode::ALL.into_iter().enumerate() {
        let ok = avail[i].is_ok();
        let on = ok && !grid.on && wb.mode == m;
        let b = commands
            .spawn((
                TopButton::Mode(m),
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    height: Val::Px(28.0),
                    padding: UiRect::horizontal(Val::Px(10.0)),
                    border_radius: BorderRadius::all(Val::Px(5.0)),
                    ..default()
                },
                BackgroundColor(if on {
                    Color::srgba(0.35, 0.62, 0.95, 0.28)
                } else {
                    Color::NONE
                }),
                ChildOf(modes),
            ))
            .observe(on_click)
            .id();
        if let Err(why) = avail[i] {
            commands.entity(b).insert(TopTip(why.to_string()));
        }
        text(&mut commands, &assets, b, m.label(), 13.0, on, !ok);
        text(
            &mut commands,
            &assets,
            b,
            &format!("{}", i + 1),
            10.0,
            false,
            true,
        );
    }

    // Agents, right-aligned.
    commands.spawn((
        Node {
            flex_grow: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(root),
    ));
    for (name, live) in agents {
        let pill = commands
            .spawn((
                TopTip(if live {
                    format!("{name}: working now")
                } else {
                    name.clone()
                }),
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(5.0),
                    height: Val::Px(22.0),
                    padding: UiRect::horizontal(Val::Px(8.0)),
                    border_radius: BorderRadius::all(Val::Px(11.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.07)),
                ChildOf(root),
            ))
            .id();
        dot(&mut commands, pill, live_color(live));
        let short: String = name.chars().take(18).collect();
        text(&mut commands, &assets, pill, &short, 11.0, false, !live);
    }
}

pub(crate) fn live_color(live: bool) -> Color {
    if live {
        Color::srgb(0.3, 0.85, 0.45)
    } else {
        Color::srgb(0.45, 0.48, 0.52)
    }
}

pub(crate) fn dot(commands: &mut Commands, parent: Entity, color: Color) {
    commands.spawn((
        Node {
            width: Val::Px(7.0),
            height: Val::Px(7.0),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(color),
        Pickable::IGNORE,
        ChildOf(parent),
    ));
}

pub(crate) fn text(
    commands: &mut Commands,
    assets: &AssetServer,
    parent: Entity,
    s: &str,
    px: f32,
    bold: bool,
    dim: bool,
) {
    commands.spawn((
        Text::new(s.to_string()),
        ui_text(assets, px, bold),
        ui_color(dim),
        TextLayout::no_wrap(),
        Pickable::IGNORE,
        ChildOf(parent),
    ));
}

fn on_click(
    click: On<PointerClick>,
    which: Query<&TopButton>,
    modeler: Res<Modeler>,
    mut wb: ResMut<Workbench>,
    mut grid: ResMut<Grid>,
) {
    if click.button != PointerButton::Primary {
        return;
    }
    match which.get(click.event_target()) {
        Ok(TopButton::AllDocuments) => {
            grid.on = !grid.on;
            wb.doc_popup = false;
        }
        Ok(TopButton::Document) => wb.doc_popup = !wb.doc_popup,
        Ok(TopButton::Mode(m)) if mode_available(&modeler.session.model, *m).is_ok() => {
            wb.mode = *m;
            grid.on = false;
        }
        _ => {}
    }
}

/// Shows the hovered item's [`TopTip`] under it.
fn tooltip(
    windows: Query<&Window, With<PrimaryWindow>>,
    tips: Query<(
        &TopTip,
        &ComputedNode,
        &UiGlobalTransform,
        &InheritedVisibility,
    )>,
    mut tip: Query<(&mut Text, &mut Node, &mut Visibility), With<TopTooltip>>,
) {
    let Ok((mut t, mut n, mut vis)) = tip.single_mut() else {
        return;
    };
    let Some((c, sf)) = windows
        .single()
        .ok()
        .and_then(|w| w.cursor_position().map(|c| (c, w.scale_factor())))
    else {
        vis.set_if_neq(Visibility::Hidden);
        return;
    };
    let hit = tips.iter().find(|(_, node, tr, v)| {
        v.get() && {
            let half = node.size() * 0.5;
            let local = c * sf - tr.translation;
            local.x.abs() <= half.x && local.y.abs() <= half.y
        }
    });
    match hit {
        Some((TopTip(s), node, tr, _)) => {
            if t.0 != *s {
                t.0 = s.clone();
            }
            let left = (tr.translation.x - node.size().x * 0.5) / sf;
            let top = (tr.translation.y + node.size().y * 0.5) / sf + 6.0;
            n.left = Val::Px(left);
            n.top = Val::Px(top);
            vis.set_if_neq(Visibility::Inherited);
        }
        None => {
            vis.set_if_neq(Visibility::Hidden);
        }
    }
}

/// Four small squares: the All documents icon.
fn grid_icon(commands: &mut Commands, parent: Entity) {
    let g = commands
        .spawn((
            Node {
                width: Val::Px(11.0),
                height: Val::Px(11.0),
                flex_wrap: FlexWrap::Wrap,
                row_gap: Val::Px(1.0),
                column_gap: Val::Px(1.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(parent),
        ))
        .id();
    for _ in 0..4 {
        commands.spawn((
            Node {
                width: Val::Px(5.0),
                height: Val::Px(5.0),
                border_radius: BorderRadius::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.85, 0.87, 0.9, 0.8)),
            Pickable::IGNORE,
            ChildOf(g),
        ));
    }
}
