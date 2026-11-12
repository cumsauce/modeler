//! The top bar: [All documents (N)] [project / document ▾], the mode
//! buttons (1-4) and the agents. Placeholder until its stream lands:
//! plain buttons for All documents and the modes.

use bevy::feathers::controls::{ButtonVariant, FeathersToolButton};
use bevy::feathers::display::caption;
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::ui_widgets::Activate;

use crate::prelude::*;

/// What a top-bar button does.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TopButton {
    AllDocuments,
    Mode(Mode),
}

pub(crate) fn top_bar_plugin(app: &mut App) {
    app.add_systems(Startup, setup_top_bar)
        .add_systems(Update, sync_top_bar.run_if(windowed));
}

fn setup_top_bar(mut commands: Commands) {
    let bar = commands
        .spawn((
            Name::new("top bar"),
            InSlot(PaneSlot::TopBar),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(4.0),
                padding: UiRect::horizontal(Val::Px(10.0)),
                ..default()
            },
        ))
        .id();
    let buttons = std::iter::once((TopButton::AllDocuments, "All documents  G".to_string())).chain(
        Mode::ALL.map(|m| {
            let n = Mode::ALL.iter().position(|x| *x == m).unwrap_or(0) + 1;
            (TopButton::Mode(m), format!("{}  {n}", m.label()))
        }),
    );
    for (which, label) in buttons {
        commands
            .spawn_scene(bsn! {
                @FeathersToolButton {
                    @variant: ButtonVariant::Normal,
                    @caption: bsn! { @caption(label) }
                }
            })
            .insert((which, ChildOf(bar)))
            .observe(on_top_button);
    }
}

fn on_top_button(
    activate: On<Activate>,
    which: Query<&TopButton>,
    modeler: Res<Modeler>,
    mut wb: ResMut<Workbench>,
    mut grid: ResMut<Grid>,
) {
    match which.get(activate.event_target()) {
        Ok(TopButton::AllDocuments) => grid.on = !grid.on,
        Ok(TopButton::Mode(m)) if mode_available(&modeler.session.model, *m).is_ok() => {
            wb.mode = *m;
            grid.on = false;
        }
        _ => {}
    }
}

fn sync_top_bar(
    wb: Res<Workbench>,
    grid: Res<Grid>,
    mut buttons: Query<(&TopButton, &mut ButtonVariant)>,
) {
    for (which, mut variant) in &mut buttons {
        let on = match which {
            TopButton::AllDocuments => grid.on,
            TopButton::Mode(m) => !grid.on && wb.mode == *m,
        };
        variant.set_if_neq(ButtonVariant::selected(on));
    }
}
