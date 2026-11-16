//! Comments (annotations) from the human side.
//!
//! Anything can open the comment box: a click on a part in the 3D view while
//! comments are open (a pin on that spot), a right-click on a part any time,
//! "Add a comment" on the selection, a right-click on a left-pane row (a part
//! or a group) or on a material node / the material preview.
//!
//! The box floats at the top right of the 3D view, outside every pane, so
//! nothing rebuilds it while it is open: the right pane rebuilds on every
//! model change (an agent's edit), and when the box lived inside it each
//! rebuild despawned the box, its focus and the keys typed in between. It is
//! built once per target: what it is on (× closes), a wrapping text input
//! with focus, and Cancel / Post. Enter posts (Shift+Enter is a new line),
//! Escape cancels, clicking empty space cancels an empty box.

use bevy::feathers::controls::{FeathersTextInput, FeathersTextInputContainer};
use bevy::input_focus::{FocusCause, InputFocus};
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::text::EditableText;
use modeler::AnnotationTarget;

use crate::prelude::*;

/// What the open box comments on.
#[derive(Clone, Debug)]
pub(crate) enum CommentTarget {
    /// The spot clicked in 3D. The box keeps its own copy of the pin: the
    /// session's `pending_pin` is dropped whenever the model changes (an
    /// agent's edit) and may be taken by an agent's `add_annotation`.
    Spot(PendingPin),
    Target(AnnotationTarget),
}

impl CommentTarget {
    fn describe(&self) -> String {
        match self {
            CommentTarget::Spot(p) => format!("a spot on {}", p.part),
            CommentTarget::Target(AnnotationTarget::Part(p)) => p.clone(),
            CommentTarget::Target(AnnotationTarget::Group { label, .. }) => {
                format!("group {label}")
            }
            CommentTarget::Target(AnnotationTarget::Material { material, node }) => match node {
                Some(n) => format!("node {n} of {material}"),
                None => format!("material {material}"),
            },
        }
    }
}

#[derive(Resource, Default)]
pub(crate) struct CommentBox {
    pub(crate) target: Option<CommentTarget>,
    /// The box needs (re)building: a new target.
    dirty: bool,
    root: Option<Entity>,
    /// A short confirmation shown in the HUD, with when it was set.
    pub(crate) flash: Option<(String, f64)>,
    /// Submit / cancel requested by a button or a key.
    action: Option<Action>,
    /// What is typed so far: kept when the box is rebuilt for a new spot.
    draft: String,
    /// The document the box was opened in (switching documents cancels it).
    doc: String,
    /// Whether the comments pane was showing last frame (closing it cancels).
    comments_were_open: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    Submit,
    Cancel,
}

/// What a key press does to an open box: Escape cancels, Enter posts,
/// Shift+Enter is left to the input (a new line).
pub(crate) fn key_action(escape: bool, enter: bool, shift: bool) -> Option<Action> {
    if escape {
        Some(Action::Cancel)
    } else if enter && !shift {
        Some(Action::Submit)
    } else {
        None
    }
}

/// A click on empty space (no part under it) with the box open: an empty
/// box closes, one with text in it stays (Escape or × discards it).
pub(crate) fn empty_click_cancels(draft: &str) -> bool {
    draft.trim().is_empty()
}

impl CommentBox {
    /// Open the box for a target (replaces any open one).
    pub(crate) fn request(&mut self, t: AnnotationTarget) {
        self.target = Some(CommentTarget::Target(t));
        self.draft.clear();
        self.dirty = true;
    }

    /// Open the box for the spot just clicked. A box already open on another
    /// spot moves there and keeps what was typed.
    pub(crate) fn request_spot(&mut self, pin: PendingPin) {
        if !matches!(self.target, Some(CommentTarget::Spot(_))) {
            self.draft.clear();
        }
        self.target = Some(CommentTarget::Spot(pin));
        self.dirty = true;
    }

    /// Close the box (as Cancel does).
    pub(crate) fn cancel(&mut self) {
        if self.target.is_some() {
            self.action = Some(Action::Cancel);
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.target.is_some()
    }

    /// The spot an open box is pinned to, if any.
    pub(crate) fn spot(&self) -> Option<&PendingPin> {
        match &self.target {
            Some(CommentTarget::Spot(p)) => Some(p),
            _ => None,
        }
    }

    pub(crate) fn draft(&self) -> &str {
        &self.draft
    }

    pub(crate) fn say(&mut self, msg: String, now: f64) {
        self.flash = Some((msg, now));
    }
}

/// The comment box's text input (other panes have inputs of their own).
#[derive(Component)]
pub(crate) struct CommentInput;

#[derive(Component)]
pub(crate) struct DeleteComment(pub(crate) u32);

/// A button of the box.
#[derive(Component, Clone, Copy)]
struct BoxButton(Action);

fn on_box_button(click: On<PointerClick>, which: Query<&BoxButton>, mut cb: ResMut<CommentBox>) {
    if click.button != PointerButton::Primary {
        return;
    }
    if let Ok(b) = which.get(click.event_target()) {
        cb.action = Some(b.0);
    }
}

pub(crate) fn on_delete(
    activate: On<PointerClick>,
    ids: Query<&DeleteComment>,
    mut modeler: ResMut<Modeler>,
    mut cb: ResMut<CommentBox>,
    time: Res<Time>,
) {
    if activate.button != PointerButton::Primary {
        return;
    }
    if let Ok(d) = ids.get(activate.event_target()) {
        let msg = match modeler.session.delete_annotation(d.0) {
            Ok(m) => m,
            Err(e) => e.to_string(),
        };
        modeler.session.mark_autosave(true, false, false);
        cb.say(msg, time.elapsed_secs_f64());
    }
}

/// Width of the floating box.
const BOX_W: f32 = 320.0;

/// Builds the box when its target changes, keeps it on the 3D view's top
/// right, Enter / Escape, submit / cancel.
#[allow(clippy::too_many_arguments)]
pub(crate) fn comment_box(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut cb: ResMut<CommentBox>,
    mut typing: ResMut<PinTyping>,
    mut modeler: ResMut<Modeler>,
    mut focus: ResMut<InputFocus>,
    mut inputs: Query<&mut EditableText, With<CommentInput>>,
    new_inputs: Query<Entity, Added<CommentInput>>,
    mut nodes: Query<&mut Node>,
    assets: Res<AssetServer>,
    mut commands: Commands,
    mut wb: ResMut<Workbench>,
) {
    typing.open = cb.is_open() || wb.doc_popup;
    // An agent's call has its own document active: leave everything as it is.
    if modeler.session.viewer_away() {
        cb.comments_were_open = wb.show_comments;
        return;
    }
    let now = time.elapsed_secs_f64();
    // `set_camera ui.comment`: open, type, post or cancel as the human would.
    if let Some(mut req) = wb.comment_ui.take() {
        let mut typed = false;
        if let Some(part) = req.part.take() {
            match req.spot.take() {
                Some(at) => cb.request_spot(PendingPin {
                    part,
                    face: None,
                    vertex: None,
                    position: at,
                }),
                None => cb.request(AnnotationTarget::Part(part)),
            }
        }
        if let Some(text) = req.text.take() {
            typed = true;
            let live = inputs
                .iter_mut()
                .find(|_| !cb.dirty && new_inputs.is_empty());
            match live {
                // Typed into the field: the draft follows next frame.
                Some(mut t) => t.queue_edit(bevy::text::TextEdit::Insert(text.into())),
                // No field yet: the new one starts with it.
                None => cb.draft.push_str(&text),
            }
        }
        match req.action.as_deref() {
            Some(_)
                if cb.is_open()
                    && (typed || cb.dirty || inputs.is_empty() || !new_inputs.is_empty()) =>
            {
                // Wait for the box and the typed text to land first.
                wb.comment_ui = Some(req);
            }
            Some("post") => cb.action = Some(Action::Submit),
            Some(_) => cb.cancel(),
            None => {}
        }
    }
    if cb.dirty && cb.is_open() {
        // A new target shows its pins (and the comments list).
        wb.show_comments = true;
        cb.doc = modeler.session.model.name.clone();
    } else if cb.is_open() && cb.action.is_none() {
        // Closing comments or switching documents closes the box.
        let closed = cb.comments_were_open && !wb.show_comments;
        if closed || cb.doc != modeler.session.model.name {
            cb.action = Some(Action::Cancel);
        }
    }
    cb.comments_were_open = wb.show_comments;
    // Focus the input as soon as it exists; a rebuilt box gets back what
    // was typed in the old one.
    if let Some(e) = new_inputs.iter().next() {
        focus.set(e, FocusCause::Auto);
        if !cb.draft.is_empty()
            && let Ok(mut t) = inputs.get_mut(e)
        {
            t.queue_edit(bevy::text::TextEdit::Insert(cb.draft.as_str().into()));
        }
    } else if let Some(t) = inputs.iter().next() {
        // Compare in place; only copy when it changed.
        let v = t.value();
        if v != cb.draft.as_str() {
            cb.draft = v.to_string();
        }
    }
    if cb.is_open() && cb.action.is_none() {
        let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
        cb.action = key_action(
            keys.just_pressed(KeyCode::Escape),
            keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter),
            shift,
        );
    }
    match cb.action.take() {
        Some(Action::Cancel) => {
            if cb.spot().is_some() {
                modeler.session.pending_pin = None;
            }
            cb.target = None;
            cb.draft.clear();
            cb.dirty = true;
        }
        Some(Action::Submit) => {
            // The input's own text (the draft lags a frame behind).
            let text = inputs
                .iter()
                .next()
                .map(|t| t.value().to_string().trim().to_string())
                .unwrap_or_else(|| cb.draft.trim().to_string());
            if text.is_empty() {
                cb.say("type a comment first".into(), now);
            } else if let Some(t) = cb.target.clone() {
                let r = match &t {
                    CommentTarget::Spot(pin) => {
                        // The box's own pin, never whatever the session
                        // holds now.
                        modeler.session.pending_pin = None;
                        modeler.session.add_annotation(
                            &pin.part,
                            &text,
                            pin.face,
                            pin.vertex,
                            Some(pin.position),
                        )
                    }
                    CommentTarget::Target(t) => modeler.session.annotate(t, &text),
                };
                match r {
                    Ok(msg) => {
                        modeler.session.mark_autosave(true, false, false);
                        cb.say(msg, now);
                        cb.target = None;
                        cb.draft.clear();
                        cb.dirty = true;
                    }
                    Err(e) => cb.say(e.to_string(), now),
                }
            }
        }
        None => {}
    }
    if cb.dirty {
        cb.dirty = false;
        if let Some(r) = cb.root.take()
            && let Ok(mut e) = commands.get_entity(r)
        {
            e.despawn();
        }
        if focus.get().is_some() {
            focus.clear();
        }
        if let Some(target) = cb.target.clone() {
            let root = spawn_box(&mut commands, &assets, &target);
            cb.root = Some(root);
        }
    }
    // Keep the box at the 3D view's top right (below the HUD chips).
    if let Some(root) = cb.root
        && let Ok(mut node) = nodes.get_mut(root)
    {
        let c = wb.rects.centre;
        let w = BOX_W.min((c.width() - 24.0).max(160.0));
        let left = Val::Px((c.max.x - w - 12.0).max(0.0));
        let top = Val::Px(c.min.y + 52.0);
        if node.left != left || node.top != top || node.width != Val::Px(w) {
            node.left = left;
            node.top = top;
            node.width = Val::Px(w);
        }
    }
}

fn spawn_box(commands: &mut Commands, assets: &AssetServer, target: &CommentTarget) -> Entity {
    let root = commands
        .spawn((
            Name::new("comment box"),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(TOP_H + 52.0),
                width: Val::Px(BOX_W),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                padding: UiRect::all(Val::Px(10.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(ds::RADIUS + 2.0)),
                ..default()
            },
            BackgroundColor(ds::PANEL),
            BorderColor::all(ds::LINE),
            BoxShadow::new(
                Color::BLACK.with_alpha(0.5),
                Val::Px(0.0),
                Val::Px(4.0),
                Val::Px(0.0),
                Val::Px(16.0),
            ),
            GlobalZIndex(10),
            BlocksCamera,
        ))
        .id();
    // Header: what this is on, and ×.
    let header = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                column_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    commands.spawn((
        ds::text_wrap(
            assets,
            ds::Type::Title,
            format!("Comment on {}", target.describe()),
        ),
        Node {
            flex_shrink: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(header),
    ));
    let close = commands
        .spawn((
            ds::piece(ds::Kind::IconButton),
            BoxButton(Action::Cancel),
            ChildOf(header),
        ))
        .observe(on_box_button)
        .id();
    commands.spawn((
        ImageNode::new(assets.load(bevy::feathers::constants::icons::X)).with_color(ds::DIM),
        Node {
            width: Val::Px(13.0),
            height: Val::Px(13.0),
            ..default()
        },
        ds::Ink::Icon,
        Pickable::IGNORE,
        ChildOf(close),
    ));
    // The input (tagged once its scene has spawned: see `tag_input`).
    commands
        .spawn_scene(bsn! {
            @FeathersTextInputContainer
            Children [
                @FeathersTextInput
            ]
        })
        .insert((ChildOf(root), CommentInputHost));
    // Hint and buttons.
    let row = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    commands.spawn((
        ds::text_in(
            assets,
            ds::Type::Meta,
            "Enter posts · Esc cancels",
            ds::FAINT,
        ),
        Node {
            flex_grow: 1.0,
            ..default()
        },
        Pickable::IGNORE,
        ChildOf(row),
    ));
    for (label, kind, action) in [
        ("Cancel", ds::Kind::Button, Action::Cancel),
        ("Post", ds::Kind::Primary, Action::Submit),
    ] {
        let b = commands
            .spawn((ds::piece(kind), BoxButton(action), ChildOf(row)))
            .observe(on_box_button)
            .id();
        commands.spawn((
            ds::text(assets, ds::Type::Body, label),
            ds::Ink::Main,
            Pickable::IGNORE,
            ChildOf(b),
        ));
    }
    root
}

/// The HUD line: confirmations ("annotation #4 on ...") for a few seconds.
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
        Some((m, t)) if now - t < 4.0 => m.as_str(),
        _ => "",
    };
    vis.set_if_neq(if msg.is_empty() {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    });
    if text.0 != msg {
        text.0 = msg.to_string();
    }
}

/// The container the comment box's input spawns in.
#[derive(Component)]
pub(crate) struct CommentInputHost;

/// Tags the text input inside the comment box once the scene has made it,
/// so the box reads (and focuses) its own field, never another pane's.
pub(crate) fn tag_input(
    new: Query<(Entity, &ChildOf), (Added<EditableText>, Without<CommentInput>)>,
    hosts: Query<(), With<CommentInputHost>>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    for (e, parent) in &new {
        if !hosts.contains(parent.parent()) {
            continue;
        }
        // Long comments wrap onto more lines instead of scrolling sideways
        // out of sight. Adjust the widget's own nodes rather than replacing
        // them, so whatever Feathers set up stays.
        commands
            .entity(e)
            .insert((CommentInput, TextLayout::linebreak(LineBreak::WordBoundary)));
        if let Ok(mut n) = nodes.get_mut(e) {
            n.flex_grow = 1.0;
            n.width = Val::Percent(100.0);
        }
        if let Ok(mut n) = nodes.get_mut(parent.parent()) {
            n.min_height = Val::Px(52.0);
            n.width = Val::Percent(100.0);
            n.align_items = AlignItems::FlexStart;
            n.padding = UiRect::all(Val::Px(6.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_actions() {
        assert_eq!(key_action(true, false, false), Some(Action::Cancel));
        assert_eq!(key_action(true, true, false), Some(Action::Cancel));
        assert_eq!(key_action(false, true, false), Some(Action::Submit));
        assert_eq!(key_action(false, true, true), None);
        assert_eq!(key_action(false, false, false), None);
    }

    #[test]
    fn empty_click_closes_only_an_empty_box() {
        assert!(empty_click_cancels(""));
        assert!(empty_click_cancels("  \n"));
        assert!(!empty_click_cancels("move this"));
    }

    fn pin(part: &str) -> PendingPin {
        PendingPin {
            part: part.into(),
            face: Some(3),
            vertex: None,
            position: [0.0, 1.0, 0.0],
        }
    }

    #[test]
    fn moving_the_spot_keeps_the_draft() {
        let mut cb = CommentBox::default();
        cb.request_spot(pin("a"));
        cb.draft = "half typed".into();
        cb.request_spot(pin("b"));
        assert_eq!(cb.draft(), "half typed");
        assert_eq!(cb.spot().map(|p| p.part.as_str()), Some("b"));
        // A different kind of target starts empty.
        cb.request(AnnotationTarget::Part("c".into()));
        assert_eq!(cb.draft(), "");
        assert!(cb.spot().is_none());
    }

    #[test]
    fn cancel_only_acts_on_an_open_box() {
        let mut cb = CommentBox::default();
        cb.cancel();
        assert_eq!(cb.action, None);
        cb.request_spot(pin("a"));
        cb.cancel();
        assert_eq!(cb.action, Some(Action::Cancel));
    }
}
