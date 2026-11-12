//! The document popup from the top bar (click the document half, or ⌘K):
//! a searchable list of the open documents grouped by project, each row
//! with a live dot, its name, the modes it has, its last author, how long
//! ago it changed and "unsaved"; and per project an "Open from library…"
//! row that lists the matching catalog prefabs once something is typed.
//! Click a row (or Enter for the first) to switch to / open it; Esc closes.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use bevy::feathers::controls::{FeathersTextInput, FeathersTextInputContainer};
use bevy::feathers::theme::{ThemeBackgroundColor, ThemeBorderColor};
use bevy::feathers::tokens;
use bevy::input_focus::{FocusCause, InputFocus};
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::text::EditableText;

use crate::prelude::*;

#[derive(Resource, Default)]
struct DocPopup {
    root: Option<Entity>,
    list: Option<Entity>,
    shown: Option<u64>,
    /// What Enter opens: the first row of the list.
    first: Option<Pick>,
    /// Catalog entries per project, read when the popup opens.
    library: Vec<(Option<PathBuf>, Vec<String>)>,
}

/// What a row opens: an open document, or a catalog prefab of a project.
#[derive(Component, Clone, PartialEq, Debug)]
struct Pick {
    doc: String,
    project: Option<PathBuf>,
    open: bool,
}

#[derive(Component)]
struct PopupInput;

pub(crate) fn doc_popup_plugin(app: &mut App) {
    app.init_resource::<DocPopup>()
        .add_systems(Update, (cmd_k, doc_popup).chain().run_if(windowed));
}

fn cmd_k(keys: Res<ButtonInput<KeyCode>>, mut wb: ResMut<Workbench>) {
    let cmd = keys.any_pressed([
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
    ]);
    if cmd && keys.just_pressed(KeyCode::KeyK) {
        wb.doc_popup = !wb.doc_popup;
    }
}

fn ago(t: Option<Instant>, now: Instant) -> String {
    let Some(t) = t else {
        return String::new();
    };
    let s = now.duration_since(t).as_secs();
    match s {
        0..10 => "now".into(),
        10..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        _ => format!("{}h ago", s / 3600),
    }
}

#[allow(clippy::too_many_arguments)]
fn doc_popup(
    keys: Res<ButtonInput<KeyCode>>,
    mut wb: ResMut<Workbench>,
    mut modeler: ResMut<Modeler>,
    mut pop: ResMut<DocPopup>,
    mut focus: ResMut<InputFocus>,
    inputs: Query<(Entity, &EditableText)>,
    new_inputs: Query<Entity, Added<EditableText>>,
    parents: Query<&ChildOf>,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    if !wb.doc_popup {
        if let Some(r) = pop.root.take() {
            commands.entity(r).despawn();
            pop.list = None;
            pop.shown = None;
            focus.clear();
        }
        return;
    }
    let Some(root) = pop.root else {
        let s = &modeler.session;
        let mut projects: Vec<Option<PathBuf>> = std::iter::once(&s.model)
            .chain(s.others.iter())
            .map(|m| m.project.clone())
            .collect();
        projects.sort();
        projects.dedup();
        pop.library = projects
            .into_iter()
            .map(|p| {
                let names = catalog::with_project(p.clone(), catalog::list)
                    .map(|es| es.into_iter().map(|e| e.name).collect())
                    .unwrap_or_default();
                (p, names)
            })
            .collect();
        let (root, list) = spawn_popup(&mut commands, wb.rects.top);
        pop.root = Some(root);
        pop.list = Some(list);
        return;
    };
    let mine = |e: Entity| parents.iter_ancestors(e).any(|a| a == root);
    if let Some(e) = new_inputs.iter().find(|e| mine(*e)) {
        commands.entity(e).insert(PopupInput);
        focus.set(e, FocusCause::Auto);
    }
    let query = inputs
        .iter()
        .find(|(e, _)| mine(*e))
        .map(|(_, t)| t.value().to_string().trim().to_lowercase())
        .unwrap_or_default();

    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        if let Some(p) = pop.first.clone() {
            open_pick(&mut modeler.session, &p);
            wb.doc_popup = false;
        }
        return;
    }

    let s = &modeler.session;
    let now = Instant::now();
    let docs: Vec<&Model> = std::iter::once(&s.model).chain(s.others.iter()).collect();
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        query.hash(&mut h);
        for m in &docs {
            (&m.name, m.version, s.is_saved_doc(&m.name)).hash(&mut h);
            ago(s.last_touched(&m.name), now).hash(&mut h);
        }
        h.finish()
    };
    if pop.shown == Some(key) {
        return;
    }
    pop.shown = Some(key);
    let Some(list) = pop.list else {
        return;
    };
    commands.entity(list).despawn_children();
    let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
    let mut first = None;
    // Projects: the active document's first, then by label.
    let mut groups: Vec<(String, Option<PathBuf>)> = docs
        .iter()
        .map(|m| (project_label(m), m.project.clone()))
        .collect();
    let active = groups[0].clone();
    groups.sort();
    groups.dedup();
    groups.retain(|g| *g != active);
    groups.insert(0, active);
    for (label, project) in groups {
        let in_proj: Vec<&&Model> = docs
            .iter()
            .filter(|m| m.project == project && matches(&m.name))
            .collect();
        let open_names: Vec<String> = docs.iter().map(|m| catalog::sanitize(&m.name)).collect();
        let lib: Vec<&String> = pop
            .library
            .iter()
            .find(|(p, _)| *p == project)
            .map(|(_, v)| v.iter().filter(|n| !open_names.contains(n)).collect())
            .unwrap_or_default();
        let lib_hits: Vec<&String> = if query.is_empty() {
            Vec::new()
        } else {
            lib.iter().copied().filter(|n| matches(n)).collect()
        };
        if in_proj.is_empty() && lib_hits.is_empty() && !query.is_empty() {
            continue;
        }
        let head = row(&mut commands, list, None, false);
        text(&mut commands, &assets, head, &label, 11.0, true, true);
        spacer(&mut commands, head);
        text(
            &mut commands,
            &assets,
            head,
            &format!("{} open", in_proj.len()),
            11.0,
            false,
            true,
        );
        for m in in_proj {
            let pick = Pick {
                doc: m.name.clone(),
                project: m.project.clone(),
                open: true,
            };
            first.get_or_insert(pick.clone());
            let is_active = m.name == s.model.name;
            let r = row(&mut commands, list, Some(pick), is_active);
            let live = s
                .last_touched(&m.name)
                .is_some_and(|t| now.duration_since(t) < LIVE);
            dot(
                &mut commands,
                r,
                if live {
                    live_color(true)
                } else if is_active {
                    Color::srgb(0.35, 0.62, 0.95)
                } else {
                    live_color(false)
                },
            );
            text(&mut commands, &assets, r, &m.name, 13.0, is_active, false);
            let modes: Vec<&str> = Mode::ALL
                .into_iter()
                .filter(|md| mode_available(m, *md).is_ok())
                .map(|md| md.label())
                .collect();
            text(
                &mut commands,
                &assets,
                r,
                &modes.join(" · "),
                10.0,
                false,
                true,
            );
            spacer(&mut commands, r);
            if !s.is_saved_doc(&m.name) {
                text(&mut commands, &assets, r, "unsaved", 10.0, false, false);
            }
            let by = m
                .history
                .current
                .and_then(|c| m.history.nodes.get(c))
                .and_then(|n| n.author.clone())
                .map(|a| a.chars().take(16).collect::<String>())
                .unwrap_or_default();
            let when = ago(s.last_touched(&m.name), now);
            let detail = match (by.is_empty(), when.is_empty()) {
                (false, false) => format!("{by} · {when}"),
                (false, true) => by,
                (true, _) => when,
            };
            text(&mut commands, &assets, r, &detail, 11.0, false, true);
        }
        let lr = row(&mut commands, list, None, false);
        text(
            &mut commands,
            &assets,
            lr,
            &if query.is_empty() {
                format!("Open from library…  type to search {} prefabs", lib.len())
            } else {
                format!("Open from library…  {} match", lib_hits.len())
            },
            12.0,
            false,
            true,
        );
        for n in lib_hits.into_iter().take(30) {
            let pick = Pick {
                doc: n.clone(),
                project: project.clone(),
                open: false,
            };
            first.get_or_insert(pick.clone());
            let r = row(&mut commands, list, Some(pick), false);
            commands.entity(r).insert(Node {
                height: Val::Px(24.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::left(Val::Px(26.0)).with_right(Val::Px(10.0)),
                ..default()
            });
            text(&mut commands, &assets, r, n, 12.0, false, false);
            spacer(&mut commands, r);
            text(&mut commands, &assets, r, "prefab", 10.0, false, true);
        }
    }
    pop.first = first;
}

fn spacer(commands: &mut Commands, parent: Entity) {
    commands.spawn((
        Node {
            flex_grow: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(parent),
    ));
}

fn row(commands: &mut Commands, list: Entity, pick: Option<Pick>, active: bool) -> Entity {
    let mut e = commands.spawn((
        Node {
            height: Val::Px(26.0),
            min_height: Val::Px(24.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            padding: UiRect::horizontal(Val::Px(10.0)),
            ..default()
        },
        BackgroundColor(if active {
            Color::srgba(0.35, 0.62, 0.95, 0.18)
        } else {
            Color::NONE
        }),
        ChildOf(list),
    ));
    if let Some(p) = pick {
        e.insert(p).observe(on_pick);
    }
    e.id()
}

fn spawn_popup(commands: &mut Commands, top: Rect) -> (Entity, Entity) {
    let root = commands
        .spawn((
            Name::new("document popup"),
            BlocksCamera,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(top.max.y + 4.0),
                left: Val::Px(150.0),
                width: Val::Px(520.0),
                max_height: Val::Percent(70.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                padding: UiRect::all(Val::Px(8.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            ThemeBackgroundColor(tokens::PANE_BODY_BG),
            ThemeBorderColor(tokens::PANE_HEADER_BORDER),
            BoxShadow::new(
                Color::BLACK.with_alpha(0.5),
                Val::Px(0.0),
                Val::Px(6.0),
                Val::Px(0.0),
                Val::Px(20.0),
            ),
            GlobalZIndex(20),
        ))
        .id();
    commands
        .spawn_scene(bsn! {
            @FeathersTextInputContainer
            Children [
                @FeathersTextInput
            ]
        })
        .insert(ChildOf(root));
    let list = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            ChildOf(root),
        ))
        .id();
    (root, list)
}

fn open_pick(s: &mut Session, p: &Pick) {
    let r = if p.open {
        s.scene("open", Some(&p.doc))
    } else {
        catalog::with_project(p.project.clone(), || s.scene("open", Some(&p.doc)))
    };
    match r {
        Ok(_) => s.mark_autosave(true, true, false),
        Err(e) => eprintln!("opening `{}`: {e}", p.doc),
    }
}

fn on_pick(
    click: On<PointerClick>,
    picks: Query<&Pick>,
    mut modeler: ResMut<Modeler>,
    mut wb: ResMut<Workbench>,
    mut grid: ResMut<Grid>,
) {
    if click.button != PointerButton::Primary {
        return;
    }
    let Ok(p) = picks.get(click.event_target()) else {
        return;
    };
    open_pick(&mut modeler.session, p);
    wb.doc_popup = false;
    grid.on = false;
}
