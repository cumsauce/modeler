//! Comments (annotations) from the human side, on Feathers widgets.
//!
//! Anything can open the comment box: a click on a part in the 3D view while
//! comments are open (a pin on that spot), "Add a comment" on the selection,
//! a right-click on a left-pane row (a part or a group) or on a material
//! node / the material preview. The box opens at the end of the right pane
//! (`right_pane.rs`, which lists the comments themselves; floating at the
//! top right when there is no pane to hold it): what it is on, a text input
//! with focus, and Cancel / Comment; Enter comments, Escape cancels.

use bevy::feathers::constants::icons;
use bevy::feathers::controls::{ButtonVariant, FeathersButton, FeathersToolButton};
use bevy::feathers::controls::{FeathersTextInput, FeathersTextInputContainer};
use bevy::feathers::display::{caption, icon};
use bevy::feathers::theme::{InheritableThemeTextColor, ThemeBackgroundColor, ThemeBorderColor};
use bevy::feathers::tokens;
use bevy::input_focus::{FocusCause, InputFocus};
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use modeler::AnnotationTarget;

use crate::prelude::*;

/// What the open box comments on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CommentTarget {
    /// The spot clicked in 3D (the session's pending pin) on this part.
    Spot(String),
    Target(AnnotationTarget),
}

impl CommentTarget {
    fn describe(&self) -> String {
        match self {
            CommentTarget::Spot(p) => format!("a spot on `{p}`"),
            CommentTarget::Target(AnnotationTarget::Part(p)) => format!("`{p}`"),
            CommentTarget::Target(AnnotationTarget::Group { label, .. }) => {
                format!("group `{label}`")
            }
            CommentTarget::Target(AnnotationTarget::Material { material, node }) => match node {
                Some(n) => format!("node `{n}` of `{material}`"),
                None => format!("material `{material}`"),
            },
        }
    }
}

#[derive(Resource, Default)]
pub(crate) struct CommentBox {
    pub(crate) target: Option<CommentTarget>,
    /// The box needs (re)building: a new target or a deleted comment.
    dirty: bool,
    root: Option<Entity>,
    /// A short confirmation shown in the HUD, with when it was set.
    pub(crate) flash: Option<(String, f64)>,
    /// Submit / cancel requested by a button (handled with the session).
    action: Option<Action>,
    /// The right pane's spot for the box (it is rebuilt with the pane).
    host: Option<Entity>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Submit,
    Cancel,
}

impl CommentBox {
    /// Open the box for a target (replaces any open one).
    pub(crate) fn request(&mut self, t: AnnotationTarget) {
        self.target = Some(CommentTarget::Target(t));
        self.dirty = true;
    }

    /// Open the box for the spot just clicked on `part`.
    pub(crate) fn request_spot(&mut self, part: String) {
        self.target = Some(CommentTarget::Spot(part));
        self.dirty = true;
    }

    /// The right pane rebuilt (or went away): the box moves with it.
    pub(crate) fn set_host(&mut self, host: Option<Entity>) {
        if self.host != host {
            self.host = host;
            self.root = None; // despawned with the old pane content
            self.dirty = self.target.is_some();
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.target.is_some()
    }

    pub(crate) fn say(&mut self, msg: String, now: f64) {
        self.flash = Some((msg, now));
    }
}

#[derive(Component)]
pub(crate) struct DeleteComment(pub(crate) u32);

fn on_submit(_: On<Activate>, mut cb: ResMut<CommentBox>) {
    cb.action = Some(Action::Submit);
}

fn on_cancel(_: On<Activate>, mut cb: ResMut<CommentBox>) {
    cb.action = Some(Action::Cancel);
}

pub(crate) fn on_delete(
    activate: On<Activate>,
    ids: Query<&DeleteComment>,
    mut modeler: ResMut<Modeler>,
    mut cb: ResMut<CommentBox>,
    time: Res<Time>,
) {
    if let Ok(d) = ids.get(activate.event_target()) {
        let msg = match modeler.session.delete_annotation(d.0) {
            Ok(m) => m,
            Err(e) => e.to_string(),
        };
        modeler.session.mark_autosave(true, false, false);
        cb.say(msg, time.elapsed_secs_f64());
        cb.dirty = true;
    }
}

/// Builds the box when its target changes; Enter / Escape; submit / cancel.
#[allow(clippy::too_many_arguments)]
pub(crate) fn comment_box(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut cb: ResMut<CommentBox>,
    mut typing: ResMut<PinTyping>,
    mut modeler: ResMut<Modeler>,
    mut focus: ResMut<InputFocus>,
    inputs: Query<(Entity, &EditableText)>,
    new_inputs: Query<Entity, Added<EditableText>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    uncommitted: Query<
        (Entity, &MeshMaterial3d<StandardMaterial>),
        (With<AnnotationPin>, Without<PinCommitted>),
    >,
    assets: Res<AssetServer>,
    mut commands: Commands,
    mut wb: ResMut<Workbench>,
) {
    typing.open = cb.is_open() || wb.doc_popup;
    // The box lives in the right pane: open the comments list when nothing
    // else would show the pane.
    if cb.is_open() && !wb.right_open() {
        wb.show_comments = true;
    }
    // Focus the input as soon as it exists.
    if let Some(e) = new_inputs.iter().next() {
        focus.set(e, FocusCause::Auto);
    }
    if cb.is_open() {
        if keys.just_pressed(KeyCode::Escape) {
            cb.action = Some(Action::Cancel);
        } else if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
            cb.action = Some(Action::Submit);
        }
    }
    let now = time.elapsed_secs_f64();
    match cb.action.take() {
        Some(Action::Cancel) => {
            if matches!(cb.target, Some(CommentTarget::Spot(_))) {
                modeler.session.pending_pin = None;
                for (e, _) in &uncommitted {
                    commands.entity(e).despawn();
                }
            }
            cb.target = None;
            cb.dirty = true;
        }
        Some(Action::Submit) => {
            let text = inputs
                .iter()
                .next()
                .map(|(_, t)| t.value().to_string())
                .unwrap_or_default();
            let text = text.trim().to_string();
            if text.is_empty() {
                cb.say("type a comment first".into(), now);
            } else if let Some(t) = cb.target.clone() {
                let r = match &t {
                    CommentTarget::Spot(part) => {
                        let part = part.clone();
                        modeler
                            .session
                            .add_annotation(&part, &text, None, None, None)
                    }
                    CommentTarget::Target(t) => modeler.session.annotate(t, &text),
                };
                match r {
                    Ok(msg) => {
                        if matches!(t, CommentTarget::Spot(_)) {
                            for (e, m) in &uncommitted {
                                commands.entity(e).insert(PinCommitted);
                                if let Some(mut mat) = materials.get_mut(&m.0) {
                                    mat.base_color = Color::srgb(0.2, 0.9, 0.3);
                                    mat.emissive = LinearRgba::rgb(0.2, 0.9, 0.3);
                                }
                            }
                        }
                        modeler.session.mark_autosave(true, false, false);
                        cb.say(msg, now);
                        cb.target = None;
                        cb.dirty = true;
                    }
                    Err(e) => cb.say(e.to_string(), now),
                }
            }
        }
        None => {}
    }
    if !cb.dirty {
        return;
    }
    cb.dirty = false;
    if let Some(r) = cb.root.take()
        && let Ok(mut e) = commands.get_entity(r)
    {
        e.despawn();
    }
    focus.clear();
    let Some(target) = cb.target.clone() else {
        return;
    };
    let root = spawn_box(&mut commands, &assets, &target, cb.host);
    cb.root = Some(root);
}

fn spawn_box(
    commands: &mut Commands,
    assets: &AssetServer,
    target: &CommentTarget,
    host: Option<Entity>,
) -> Entity {
    let node = match host {
        Some(_) => Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(6.0)),
            overflow: Overflow::clip(),
            ..default()
        },
        None => Node {
            position_type: PositionType::Absolute,
            top: Val::Px(TOP_H + 8.0),
            right: Val::Px(12.0),
            width: Val::Px(380.0),
            flex_direction: FlexDirection::Column,
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(6.0)),
            overflow: Overflow::clip(),
            ..default()
        },
    };
    let mut root = commands.spawn((
        Name::new("comment box"),
        node,
        ThemeBackgroundColor(tokens::PANE_BODY_BG),
        ThemeBorderColor(tokens::PANE_HEADER_BORDER),
        GlobalZIndex(10),
    ));
    match host {
        Some(h) => {
            root.insert(ChildOf(h));
        }
        None => {
            root.insert(BoxShadow::new(
                Color::BLACK.with_alpha(0.5),
                Val::Px(0.0),
                Val::Px(4.0),
                Val::Px(0.0),
                Val::Px(16.0),
            ));
        }
    }
    let root = root.id();
    // Header: what this is on, and a close button.
    let header = commands
        .spawn((
            Node {
                height: Val::Px(HEADER_H),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::horizontal(Val::Px(8.0)),
                ..default()
            },
            ThemeBackgroundColor(tokens::PANE_HEADER_BG),
            InheritableThemeTextColor(tokens::PANE_HEADER_TEXT),
            ChildOf(root),
        ))
        .id();
    commands.spawn((
        Text::new(format!("Comment on {}", target.describe())),
        ui_text(assets, 13.0, true),
        ui_color(false),
        TextLayout::no_wrap(),
        ChildOf(header),
    ));
    commands
        .spawn_scene(bsn! {
            @FeathersToolButton {
                @variant: ButtonVariant::Plain,
                @caption: bsn! { @icon(icons::X) }
            }
        })
        .insert(ChildOf(header))
        .observe(on_cancel);
    let body = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(8.0)),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    // The input.
    commands
        .spawn_scene(bsn! {
            @FeathersTextInputContainer
            Children [
                @FeathersTextInput
            ]
        })
        .insert(ChildOf(body));
    // Buttons.
    let buttons = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::FlexEnd,
                column_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(body),
        ))
        .id();
    commands
        .spawn_scene(bsn! {
            @FeathersButton {
                @variant: ButtonVariant::Normal,
                @caption: bsn! { @caption("Cancel") }
            }
        })
        .insert(ChildOf(buttons))
        .observe(on_cancel);
    commands
        .spawn_scene(bsn! {
            @FeathersButton {
                @variant: ButtonVariant::Primary,
                @caption: bsn! { @caption("Comment") }
            }
        })
        .insert(ChildOf(buttons))
        .observe(on_submit);
    root
}

/// The HUD line: confirmations ("annotation #4 on ...") for a few seconds,
/// otherwise a hint while the box is closed.
pub(crate) fn update_typing_hud(
    time: Res<Time>,
    cb: Res<CommentBox>,
    mut hud: Query<(&mut Text, &mut Visibility), With<TypingHud>>,
) {
    let Ok((mut text, mut vis)) = hud.single_mut() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    let msg = match &cb.flash {
        Some((m, t)) if now - t < 4.0 => m.clone(),
        _ => String::new(),
    };
    *vis = if msg.is_empty() {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if text.0 != msg {
        text.0 = msg;
    }
}
