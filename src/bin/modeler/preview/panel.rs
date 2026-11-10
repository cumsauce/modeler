use crate::prelude::*;

/// The bottom UI line showing the pin's typed-so-far comment.
#[derive(Component)]
pub(crate) struct TypingHud;

/// The left panel showing the procedural op graph and history cursor. Toggled
/// with G; read-only.
#[derive(Component)]
pub(crate) struct GraphHud;

#[derive(Resource, Default)]
pub(crate) struct GraphHudVisible(pub(crate) bool);

/// The bottom HUD line: what pin is being typed for and the text so far.
pub(crate) fn update_typing_hud(
    modeler: Res<Modeler>,
    typing: Res<PinTyping>,
    mut hud: Query<&mut Text, With<TypingHud>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };
    text.0 = match &modeler.session.pending_pin {
        Some(pin) => format!(
            "pin #{} on `{}`: {}_  (Enter to comment, Esc to clear)",
            modeler.session.model.next_annotation_id, pin.part, typing.text
        ),
        None => String::new(),
    };
}

/// G toggles the read-only graph panel: the op chain the parts derive from,
/// plus the history tree with its cursor. Hidden until asked for.
pub(crate) fn update_graph_hud(
    keys: Res<ButtonInput<KeyCode>>,
    mut visible: ResMut<GraphHudVisible>,
    modeler: Res<Modeler>,
    mut hud: Query<(&mut Text, &mut Visibility), With<GraphHud>>,
) {
    if keys.just_pressed(KeyCode::KeyG) {
        visible.0 = !visible.0;
    }
    let Ok((mut text, mut vis)) = hud.single_mut() else {
        return;
    };
    *vis = if visible.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if !visible.0 {
        text.0.clear();
        return;
    }
    let mut s = format!(
        "{}\n---\n",
        modeler::graph::describe(&modeler.session.model.ops)
    );
    s += &modeler.session.model.history.describe();
    text.0 = s;
}
