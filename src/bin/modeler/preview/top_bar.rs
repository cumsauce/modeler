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
            column_gap: Val::Px(12.0),
            padding: UiRect::horizontal(Val::Px(12.0)),
            border: UiRect::bottom(Val::Px(1.0)),
            ..default()
        },
        BackgroundColor(ds::PANEL),
        BorderColor::all(ds::LINE),
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
    icons_res: ResMut<ds::Icons>,
    mut images: ResMut<Assets<Image>>,
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

    let mut icons = icons_res;
    let images = &mut *images;
    // The segmented switch: one bordered group of two tabs.
    let seg = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(2.0)),
                column_gap: Val::Px(2.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(ds::BG),
            BorderColor::all(ds::LINE),
            ChildOf(root),
        ))
        .id();
    let tab = |commands: &mut Commands, parent: Entity, which: TopButton, on: bool, ok: bool| {
        let mut ui = ds::Ui::new(ds::Kind::Tab).on(on);
        ui.disabled = !ok;
        commands
            .spawn((ds::piece(ds::Kind::Tab), which, ChildOf(parent)))
            .insert(ui)
            .observe(on_click)
            .id()
    };
    let all = tab(&mut commands, seg, TopButton::AllDocuments, grid.on, true);
    commands.spawn((
        ds::icon(&mut icons, images, ds::Icon::Grid, 14.0, ds::DIM),
        ds::Ink::Icon,
        Pickable::IGNORE,
        ChildOf(all),
    ));
    commands.spawn((
        ds::text(&assets, ds::Type::Body, "All documents"),
        ds::Ink::Main,
        Pickable::IGNORE,
        ChildOf(all),
    ));
    commands.spawn((
        ds::text(&assets, ds::Type::Meta, format!("{open}")),
        ds::Ink::Meta,
        Pickable::IGNORE,
        ChildOf(all),
    ));
    let doc = tab(
        &mut commands,
        seg,
        TopButton::Document,
        !grid.on || wb.doc_popup,
        true,
    );
    commands
        .entity(doc)
        .insert(TopTip("Switch document  Cmd+K".into()));
    commands.spawn((
        ds::text(&assets, ds::Type::Small, format!("{project} /")),
        ds::Ink::Meta,
        Pickable::IGNORE,
        ChildOf(doc),
    ));
    commands.spawn((
        ds::text(&assets, ds::Type::Title, model.name.clone()),
        ds::Ink::Main,
        Pickable::IGNORE,
        ChildOf(doc),
    ));
    commands.spawn((
        ds::icon(&mut icons, images, ds::Icon::Chevron, 12.0, ds::DIM),
        ds::Ink::Icon,
        Pickable::IGNORE,
        ChildOf(doc),
    ));

    commands.spawn((
        Node {
            width: Val::Px(1.0),
            height: Val::Px(22.0),
            ..default()
        },
        BackgroundColor(ds::LINE),
        ChildOf(root),
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
        let b = tab(&mut commands, modes, TopButton::Mode(m), on, ok);
        if let Err(why) = avail[i] {
            commands.entity(b).insert(TopTip(why.to_string()));
        }
        commands.spawn((
            ds::text(&assets, ds::Type::Body, m.label()),
            ds::Ink::Main,
            Pickable::IGNORE,
            ChildOf(b),
        ));
        let k = ds::kbd(&mut commands, &assets, b, &format!("{}", i + 1));
        commands.entity(k).insert(Pickable::IGNORE);
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
                    column_gap: Val::Px(6.0),
                    height: Val::Px(24.0),
                    padding: UiRect::horizontal(Val::Px(9.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(12.0)),
                    ..default()
                },
                BackgroundColor(ds::BG),
                BorderColor::all(if live {
                    ds::agent_color(&name).with_alpha(0.6)
                } else {
                    ds::LINE
                }),
                ChildOf(root),
            ))
            .id();
        commands.spawn((
            ds::dot(
                if live {
                    ds::agent_color(&name)
                } else {
                    ds::FAINT
                },
                7.0,
            ),
            ChildOf(pill),
        ));
        let short: String = name.chars().take(18).collect();
        commands.spawn((
            ds::text_in(
                &assets,
                ds::Type::Small,
                short,
                if live { ds::FG } else { ds::DIM },
            ),
            Pickable::IGNORE,
            ChildOf(pill),
        ));
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
