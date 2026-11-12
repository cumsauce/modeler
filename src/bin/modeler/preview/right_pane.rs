//! The right pane (`PaneSlot::Right`), shown only when it has something to
//! say (`Workbench::right_open`):
//! - a selection: its name with a close button, its properties (kind, what
//!   made it, material, triangles; a marker's kind, preview and
//!   components), then the comments on it and "Add a comment";
//! - comments open with nothing selected: every comment, numbered like its
//!   pin, with the agents' replies under them, and how to pin a new one.
//!
//! The comment box (`comments.rs`) opens inside this pane. Material mode
//! puts its own preview in the same slot (`material_pane.rs`); this pane's
//! content hides there.

use bevy::feathers::constants::icons;
use bevy::feathers::controls::{ButtonVariant, FeathersButton, FeathersToolButton};
use bevy::feathers::display::{caption, icon};
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::ui_widgets::Activate;
use modeler::AnnotationTarget;

use crate::prelude::*;

pub(crate) fn right_pane_plugin(app: &mut App) {
    app.init_resource::<RightPane>()
        .add_systems(Startup, setup_right_pane)
        .add_systems(Update, (update_right_pane, comments_chip).run_if(windowed));
}

#[derive(Resource, Default)]
pub(crate) struct RightPane {
    shown: Option<Key>,
}

/// What the pane was last built for.
#[derive(PartialEq, Clone)]
struct Key {
    mode: Mode,
    selected: Option<String>,
    comments: bool,
    doc: String,
    version: u64,
    annotations: usize,
}

#[derive(Component)]
struct RightRoot;

/// Where the comment box goes (the end of the pane).
#[derive(Component)]
pub(crate) struct CommentHost;

#[derive(Component, Clone, Copy)]
enum PaneButton {
    Deselect,
    CloseComments,
    AllComments,
    AddComment,
}

/// A comment row: clicking selects its part.
#[derive(Component)]
struct CommentRow(String);

fn setup_right_pane(mut commands: Commands) {
    commands.spawn((
        Name::new("right pane"),
        RightRoot,
        InSlot(PaneSlot::Right),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            overflow: Overflow::scroll_y(),
            border: UiRect::left(Val::Px(1.0)),
            ..default()
        },
        BorderColor::all(Color::srgb(0.18, 0.2, 0.24)),
        ScrollPosition::default(),
    ));
}

#[allow(clippy::too_many_arguments)]
fn update_right_pane(
    modeler: Res<Modeler>,
    wb: Res<Workbench>,
    mut pane: ResMut<RightPane>,
    mut root: Query<(Entity, &mut Visibility, &mut ScrollPosition), With<RightRoot>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    scroll: Res<AccumulatedMouseScroll>,
    mut cb: ResMut<CommentBox>,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    let Ok((root, mut vis, mut pos)) = root.single_mut() else {
        return;
    };
    let material = wb.mode == Mode::Material;
    vis.set_if_neq(if material {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    });
    if windows
        .single()
        .ok()
        .and_then(|w| w.cursor_position())
        .is_some_and(|c| wb.rects.right.contains(c))
    {
        let lines = scroll_lines(&scroll);
        if lines != 0.0 {
            pos.y = (pos.y - lines * 24.0).max(0.0);
        }
    }
    let model = &modeler.session.model;
    let key = Key {
        mode: wb.mode,
        selected: wb.selected.clone(),
        comments: wb.show_comments,
        doc: model.name.clone(),
        version: model.version,
        annotations: model.annotations.len(),
    };
    if pane.shown.as_ref() == Some(&key) {
        return;
    }
    pane.shown = Some(key);
    commands.entity(root).despawn_children();
    cb.set_host(None);
    if material || !wb.right_open() {
        return;
    }
    let b = Build {
        assets: &assets,
        root,
    };
    let notes: Vec<&modeler::Annotation> = model
        .annotations
        .iter()
        .filter(|a| a.material.is_none())
        .collect();
    if let Some(sel) = &wb.selected {
        b.header(&mut commands, sel, PaneButton::Deselect);
        let rows = properties(model, sel);
        let list = b.section(&mut commands);
        for (k, v) in rows {
            b.kv(&mut commands, list, k, &v);
        }
        let mine: Vec<&modeler::Annotation> = notes
            .iter()
            .copied()
            .filter(|a| a.is_on(&AnnotationTarget::Part(sel.clone())))
            .collect();
        let title = if mine.is_empty() {
            "Comments".to_string()
        } else {
            format!("Comments · {}", mine.len())
        };
        b.subheader(
            &mut commands,
            &title,
            (!notes.is_empty()).then_some(notes.len()),
        );
        let list = b.section(&mut commands);
        for a in &mine {
            b.comment(&mut commands, list, a);
        }
        let host = b.host(&mut commands);
        cb.set_host(Some(host));
        if !cb.is_open() {
            b.button(&mut commands, host, "Add a comment", PaneButton::AddComment);
        }
    } else {
        b.header(
            &mut commands,
            &format!("Comments · {}", notes.len()),
            PaneButton::CloseComments,
        );
        let list = b.section(&mut commands);
        if notes.is_empty() {
            b.hint(&mut commands, list, "No comments yet.");
        }
        for a in &notes {
            b.comment(&mut commands, list, a);
        }
        let host = b.host(&mut commands);
        cb.set_host(Some(host));
        if !cb.is_open() {
            b.hint(
                &mut commands,
                host,
                "Click the model to place a pin and comment on that spot.",
            );
        }
    }
}

/// The selection's properties as label / value rows.
fn properties(model: &Model, name: &str) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    if let Some(op) = model.ops.iter().find(|o| match o {
        Op::Marker { name: n, .. } => n == name,
        _ => false,
    }) && let Op::Marker {
        kind,
        preview,
        components,
        size,
        ..
    } = op
    {
        rows.push(("kind", format!("marker · {kind}")));
        if let Some(p) = preview {
            rows.push(("preview", p.clone()));
        }
        if let Some(s) = size {
            rows.push(("size", format!("{:.1} × {:.1} × {:.1} m", s[0], s[1], s[2])));
        }
        for (c, v) in components {
            rows.push(("component", format!("{c} {v}")));
        }
        return rows;
    }
    let Some(part) = model.parts.iter().find(|p| p.name == name) else {
        rows.push(("kind", "not a part".into()));
        return rows;
    };
    let made = model
        .ops
        .iter()
        .find(|o| modeler::graph::outputs(o).iter().any(|n| n == name));
    match (name.split_once('.'), made) {
        (Some((inst, _)), _) => rows.push(("kind", format!("part of prefab instance {inst}"))),
        (None, Some(op)) => rows.push((
            "made by",
            format!(
                "{} {}",
                modeler::graph::kind(op),
                modeler::graph::output(op)
            ),
        )),
        (None, None) => rows.push(("kind", "baked part".into())),
    }
    let m = &part.material;
    rows.push((
        "material",
        match &m.pbr {
            Some(r) => r.material.clone(),
            None => {
                let c = m.base_color;
                format!(
                    "#{:02x}{:02x}{:02x}{}",
                    (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                    if m.flat { " (flat)" } else { "" }
                )
            }
        },
    ));
    rows.push(("triangles", format!("{}", part.indices.len() / 3)));
    if let Some((min, max)) = part_bounds(part) {
        let s = max - min;
        rows.push(("size", format!("{:.2} × {:.2} × {:.2} m", s.x, s.y, s.z)));
    }
    if !part.visible {
        rows.push(("visible", "hidden".into()));
    }
    rows
}

/// A part's world box.
fn part_bounds(p: &Part) -> Option<(Vec3, Vec3)> {
    let t = Transform {
        translation: Vec3::from_array(p.translation),
        rotation: Quat::from_array(p.rotation),
        scale: Vec3::from_array(p.scale),
    };
    p.verts.iter().fold(None, |acc, v| {
        let w = t.transform_point(Vec3::from_array(*v));
        Some(acc.map_or((w, w), |(a, b): (Vec3, Vec3)| (a.min(w), b.max(w))))
    })
}

struct Build<'a> {
    assets: &'a AssetServer,
    root: Entity,
}

const PAD: f32 = 12.0;

impl Build<'_> {
    fn header(&self, c: &mut Commands, title: &str, close: PaneButton) {
        let row = c
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    padding: UiRect::new(Val::Px(PAD), Val::Px(4.0), Val::Px(6.0), Val::Px(2.0)),
                    min_height: Val::Px(HEADER_H),
                    ..default()
                },
                ChildOf(self.root),
            ))
            .id();
        c.spawn((
            Text::new(title),
            ui_text(self.assets, 14.0, true),
            ui_color(false),
            Node {
                flex_shrink: 1.0,
                ..default()
            },
            ChildOf(row),
        ));
        c.spawn_scene(bsn! {
            @FeathersToolButton {
                @variant: ButtonVariant::Plain,
                @caption: bsn! { @icon(icons::X) }
            }
        })
        .insert((close, ChildOf(row)))
        .observe(on_button);
    }

    /// A small uppercase heading, with an "all N" button when given.
    fn subheader(&self, c: &mut Commands, title: &str, all: Option<usize>) {
        let row = c
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    padding: UiRect::new(Val::Px(PAD), Val::Px(4.0), Val::Px(14.0), Val::Px(2.0)),
                    ..default()
                },
                ChildOf(self.root),
            ))
            .id();
        c.spawn((
            Text::new(title.to_uppercase()),
            ui_text(self.assets, 11.0, false),
            ui_color(true),
            ChildOf(row),
        ));
        if let Some(n) = all {
            let label = format!("all {n}");
            c.spawn_scene(bsn! {
                @FeathersToolButton {
                    @variant: ButtonVariant::Plain,
                    @caption: bsn! { @caption(label) }
                }
            })
            .insert((PaneButton::AllComments, ChildOf(row)))
            .observe(on_button);
        }
    }

    fn section(&self, c: &mut Commands) -> Entity {
        c.spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                padding: UiRect::axes(Val::Px(PAD), Val::Px(4.0)),
                ..default()
            },
            ChildOf(self.root),
        ))
        .id()
    }

    fn host(&self, c: &mut Commands) -> Entity {
        c.spawn((
            CommentHost,
            Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::axes(Val::Px(PAD), Val::Px(8.0)),
                ..default()
            },
            ChildOf(self.root),
        ))
        .id()
    }

    fn kv(&self, c: &mut Commands, parent: Entity, k: &str, v: &str) {
        let row = c
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(8.0),
                    ..default()
                },
                ChildOf(parent),
            ))
            .id();
        c.spawn((
            Text::new(k),
            ui_text(self.assets, 12.0, false),
            ui_color(true),
            Node {
                width: Val::Px(80.0),
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(row),
        ));
        c.spawn((
            Text::new(v),
            ui_text(self.assets, 12.0, false),
            ui_color(false),
            Node {
                flex_shrink: 1.0,
                ..default()
            },
            ChildOf(row),
        ));
    }

    fn hint(&self, c: &mut Commands, parent: Entity, text: &str) {
        c.spawn((
            Text::new(text),
            ui_text(self.assets, 12.0, false),
            ui_color(true),
            ChildOf(parent),
        ));
    }

    /// One comment: its pin number in a yellow disc, the part, the text;
    /// a click selects the part, × deletes it.
    fn comment(&self, c: &mut Commands, parent: Entity, a: &modeler::Annotation) {
        let row = c
            .spawn((
                CommentRow(a.part.clone()),
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(10.0),
                    padding: UiRect::vertical(Val::Px(8.0)),
                    border: UiRect::bottom(Val::Px(1.0)),
                    ..default()
                },
                BorderColor::all(Color::srgb(0.18, 0.2, 0.24)),
                ChildOf(parent),
            ))
            .observe(on_comment_click)
            .id();
        let disc = c
            .spawn((
                Node {
                    width: Val::Px(20.0),
                    height: Val::Px(20.0),
                    flex_shrink: 0.0,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border_radius: BorderRadius::all(Val::Px(10.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.89, 0.7, 0.31)),
                ChildOf(row),
            ))
            .id();
        c.spawn((
            Text::new(format!("{}", a.id)),
            ui_text(self.assets, 11.0, true),
            TextColor(Color::srgb(0.1, 0.08, 0.0)),
            ChildOf(disc),
        ));
        let body = c
            .spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                ChildOf(row),
            ))
            .id();
        c.spawn((
            Text::new(a.group.clone().unwrap_or_else(|| a.part.clone())),
            ui_text(self.assets, 12.0, true),
            ui_color(false),
            ChildOf(body),
        ));
        c.spawn((
            Text::new(a.text.clone()),
            ui_text(self.assets, 12.5, false),
            ui_color(false),
            ChildOf(body),
        ));
        let id = a.id;
        c.spawn_scene(bsn! {
            @FeathersToolButton {
                @variant: ButtonVariant::Plain,
                @caption: bsn! { @icon(icons::X) }
            }
        })
        .insert((DeleteComment(id), ChildOf(row)))
        .observe(on_delete);
    }

    fn button(&self, c: &mut Commands, parent: Entity, label: &str, which: PaneButton) {
        let label = label.to_string();
        c.spawn_scene(bsn! {
            @FeathersButton {
                @variant: ButtonVariant::Normal,
                @caption: bsn! { @caption(label) }
            }
        })
        .insert((which, ChildOf(parent)))
        .observe(on_button);
    }
}

fn on_button(
    activate: On<Activate>,
    which: Query<&PaneButton>,
    mut wb: ResMut<Workbench>,
    mut cb: ResMut<CommentBox>,
) {
    let Ok(w) = which.get(activate.event_target()) else {
        return;
    };
    match w {
        PaneButton::Deselect => wb.selected = None,
        PaneButton::CloseComments => wb.show_comments = false,
        PaneButton::AllComments => {
            wb.selected = None;
            wb.show_comments = true;
        }
        PaneButton::AddComment => {
            if let Some(sel) = wb.selected.clone() {
                cb.request(AnnotationTarget::Part(sel));
            }
        }
    }
}

fn on_comment_click(
    click: On<PointerClick>,
    rows: Query<&CommentRow>,
    modeler: Res<Modeler>,
    mut wb: ResMut<Workbench>,
) {
    if let Ok(r) = rows.get(click.event_target())
        && modeler.session.model.parts.iter().any(|p| p.name == r.0)
    {
        wb.selected = Some(r.0.clone());
    }
}

/// The comments chip at the end of the 3D view's HUD row (`shading.rs`):
/// the comment count; a click (or C) opens / closes the comments list.
#[derive(Component)]
struct CommentsChip;

#[derive(Component)]
struct CommentsChipLabel;

fn comments_chip(
    rows: Query<Entity, With<HudRow>>,
    chip: Query<(), With<CommentsChip>>,
    mut label: Query<&mut Text, With<CommentsChipLabel>>,
    mut variant: Query<&mut ButtonVariant, With<CommentsChip>>,
    modeler: Res<Modeler>,
    wb: Res<Workbench>,
    mut commands: Commands,
) {
    let n = modeler
        .session
        .model
        .annotations
        .iter()
        .filter(|a| a.material.is_none())
        .count();
    let text = format!("Comments {n}");
    if chip.is_empty() {
        let Some(row) = rows.iter().next() else {
            return;
        };
        let chip = commands
            .spawn_scene(bsn! {
                @FeathersToolButton {
                    @variant: ButtonVariant::Normal,
                    @caption: bsn! { }
                }
            })
            .insert((CommentsChip, ChildOf(row)))
            .observe(on_chip)
            .id();
        commands.spawn((CommentsChipLabel, Text::new(text), ChildOf(chip)));
        return;
    }
    for mut t in &mut label {
        if t.0 != text {
            t.0 = text.clone();
        }
    }
    for mut v in &mut variant {
        v.set_if_neq(ButtonVariant::selected(wb.show_comments));
    }
}

fn on_chip(_: On<Activate>, mut wb: ResMut<Workbench>) {
    wb.show_comments = !wb.show_comments;
}
