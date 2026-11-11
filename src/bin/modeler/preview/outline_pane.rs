//! The outliner (O): the scene as a hierarchy in the left pane. Prefab
//! instances and array runs fold into expandable groups, hidden parts sink
//! into a dimmed group (the tree is `modeler::outline`). Click a row to
//! select it (its parts pulse in 3D; a group's chevron or a second click
//! opens it), double-click to frame it.

use std::collections::HashSet;

use bevy::feathers::constants::icons;
use modeler::AnnotationTarget;
use modeler::outline::{self, Row};

use crate::prelude::*;

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
    /// The model version and selection the list was built for.
    shown: Option<(u64, u64)>,
    /// Bumped by clicks (expansion / selection), forcing a rebuild.
    generation: u64,
    tree: Vec<Row>,
}

impl OutlinePane {
    /// Open at start (MODELER_OUTLINE=1), like MODELER_GRAPH for G.
    pub(crate) fn open(visible: bool) -> Self {
        OutlinePane {
            visible,
            ..default()
        }
    }
}

#[derive(Component)]
pub(crate) struct OutlineHud;

#[derive(Component)]
pub(crate) struct OutlineList;

#[derive(Component)]
pub(crate) struct OutlineRow {
    key: String,
    group: bool,
}

/// The chevron of a group row (a click there only opens / closes).
#[derive(Component)]
pub(crate) struct OutlineChevron(String);

const ROW_H: f32 = 22.0;
const INDENT: f32 = 14.0;

/// O toggles; keeps the list in step with the model; wheel scrolls.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_outline_pane(
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    typing: Res<PinTyping>,
    modeler: Res<Modeler>,
    mut pane: ResMut<OutlinePane>,
    mut graph: ResMut<GraphHudVisible>,
    mut hud: Query<(&mut Visibility, &ComputedNode), With<OutlineHud>>,
    mut list: Query<(Entity, &mut ScrollPosition), With<OutlineList>>,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    if keys.just_pressed(KeyCode::KeyO) && !typing.open {
        if graph.0 {
            graph.0 = false;
            pane.visible = true;
        } else {
            pane.visible = !pane.visible;
        }
    }
    let shown = pane.visible && !graph.0;
    let Ok((mut vis, computed)) = hud.single_mut() else {
        return;
    };
    *vis = if shown {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if !shown {
        pane.hover = false;
        return;
    }
    let size = computed.size() * computed.inverse_scale_factor();
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    pane.hover = cursor.is_some_and(|c| c.x < size.x && c.y < size.y);
    let Ok((list_e, mut pos)) = list.single_mut() else {
        return;
    };
    if pane.hover {
        let lines = scroll_lines(&scroll);
        if lines != 0.0 {
            pos.y = (pos.y - lines * ROW_H * 1.5).max(0.0);
        }
    }

    let model = &modeler.session.model;
    let want = (model.version, pane.generation);
    if pane.shown == Some(want) {
        return;
    }
    if pane.shown.is_none_or(|s| s.0 != model.version) {
        pane.tree = outline::build(model);
        // Keep the selection on the same row if it still exists.
        let keys: Vec<(String, Vec<String>)> = outline::flatten(&pane.tree, &|_| true)
            .into_iter()
            .map(|(_, r)| (r.key.clone(), r.parts.clone()))
            .collect();
        match pane
            .selected
            .clone()
            .and_then(|k| keys.into_iter().find(|(key, _)| *key == k))
        {
            Some((_, parts)) => pane.selected_parts = parts,
            None => {
                pane.selected = None;
                pane.selected_parts.clear();
            }
        }
    }
    pane.shown = Some(want);
    let pane = &*pane;
    commands.entity(list_e).despawn_children();
    let rows = outline::flatten(&pane.tree, &|k| pane.expanded.contains(k));
    let chevron_right: Handle<Image> = assets.load(icons::CHEVRON_RIGHT);
    let chevron_down: Handle<Image> = assets.load(icons::CHEVRON_DOWN);
    for (depth, r) in rows {
        let selected = pane.selected.as_deref() == Some(r.key.as_str());
        let group = !r.children.is_empty();
        let row = commands
            .spawn((
                OutlineRow {
                    key: r.key.clone(),
                    group,
                },
                Node {
                    height: Val::Px(ROW_H),
                    min_height: Val::Px(ROW_H),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    padding: UiRect {
                        left: Val::Px(6.0 + depth as f32 * INDENT),
                        right: Val::Px(8.0),
                        ..default()
                    },
                    ..default()
                },
                BackgroundColor(if selected {
                    Color::srgba(0.25, 0.45, 0.85, 0.45)
                } else {
                    Color::NONE
                }),
                ChildOf(list_e),
            ))
            .observe(on_row_click)
            .id();
        // Chevron (groups) or a spacer of the same width (leaves).
        let mut chev = commands.spawn((
            Node {
                width: Val::Px(12.0),
                height: Val::Px(12.0),
                ..default()
            },
            ChildOf(row),
        ));
        if group {
            let open = pane.expanded.contains(&r.key);
            chev.insert((
                ImageNode::new(if open {
                    chevron_down.clone()
                } else {
                    chevron_right.clone()
                })
                .with_color(Color::srgba(0.85, 0.87, 0.9, 0.9)),
                OutlineChevron(r.key.clone()),
            ));
        }
        let [cr, cg, cb] = r.swatch;
        commands.spawn((
            Node {
                width: Val::Px(10.0),
                height: Val::Px(10.0),
                border_radius: BorderRadius::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::linear_rgb(cr, cg, cb).with_alpha(if r.dim {
                0.4
            } else {
                1.0
            })),
            ChildOf(row),
        ));
        commands.spawn((
            Text::new(r.label.clone()),
            ui_text(&assets, 13.0, group),
            ui_color(r.dim),
            TextLayout::no_wrap(),
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
            Text::new(r.detail.clone()),
            ui_text(&assets, 11.0, false),
            ui_color(true),
            TextLayout::no_wrap(),
            Pickable::IGNORE,
            ChildOf(row),
        ));
    }
}

/// Row clicks: the chevron opens / closes a group; elsewhere selects (a
/// second click on a selected group opens it too); a double click frames
/// the row's parts.
fn on_row_click(
    click: On<PointerClick>,
    rows: Query<&OutlineRow>,
    chevrons: Query<&OutlineChevron>,
    modeler: Res<Modeler>,
    mut pane: ResMut<OutlinePane>,
    mut orbit: ResMut<Orbit>,
    mut comment: ResMut<CommentBox>,
) {
    let target = click.original_event_target();
    let Ok(row) = rows.get(click.event_target()) else {
        return;
    };
    // Right-click: comment on the row (a group comments on all its parts).
    if click.button == PointerButton::Secondary {
        let found = outline::flatten(&pane.tree, &|_| true)
            .into_iter()
            .find(|(_, r)| r.key == row.key)
            .map(|(_, r)| (r.label.clone(), r.parts.clone()));
        if let Some((label, parts)) = found {
            comment.request(if row.group {
                AnnotationTarget::Group { label, parts }
            } else {
                AnnotationTarget::Part(parts.into_iter().next().unwrap_or_default())
            });
        }
        return;
    }
    if click.button != PointerButton::Primary {
        return;
    }
    let key = row.key.clone();
    pane.generation += 1;
    if let Ok(ch) = chevrons.get(target) {
        if !pane.expanded.remove(&ch.0) {
            pane.expanded.insert(ch.0.clone());
        }
        return;
    }
    let double = click.count >= 2;
    let parts = outline::flatten(&pane.tree, &|_| true)
        .into_iter()
        .find(|(_, r)| r.key == key)
        .map(|(_, r)| r.parts.clone())
        .unwrap_or_default();
    if double {
        if let Some((min, max)) = modeler.session.model.bounds_of(&parts) {
            orbit.target = (min + max) * 0.5;
            orbit.dist = ((max - min).length() * 0.5 * 2.4).max(0.3);
            orbit.user = true;
        }
    } else if pane.selected.as_deref() == Some(key.as_str())
        && row.group
        && !pane.expanded.remove(&key)
    {
        pane.expanded.insert(key.clone());
    }
    pane.selected = Some(key);
    pane.selected_parts = parts;
}

/// The pane's UI tree (hidden until O).
pub(crate) fn setup_outline_pane(mut commands: Commands, assets: Res<AssetServer>) {
    let pane = commands
        .spawn((
            Name::new("outliner"),
            OutlineHud,
            Visibility::Hidden,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(PANE_FRACTION * 100.0),
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
        Text::new("Outliner"),
    );
}
