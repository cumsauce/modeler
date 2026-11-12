//! Shared window chrome built on Bevy Feathers (the editor widget set): the
//! dark theme, Fira fonts and pane headers with a close button. The layout
//! is `workbench.rs`, the top bar `top_bar.rs`.

use bevy::feathers::constants::{fonts, icons, size};
use bevy::feathers::controls::{ButtonVariant, FeathersToolButton};

use bevy::feathers::display::icon;
use bevy::feathers::theme::{
    InheritableThemeTextColor, ThemeBackgroundColor, ThemeBorderColor, ThemeTextColor, ThemedText,
};
use bevy::feathers::tokens;
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::ui_widgets::Activate;

use crate::prelude::*;

/// Height of a pane header row.
pub(crate) const HEADER_H: f32 = 30.0;

/// Text in the theme's UI font.
pub(crate) fn ui_text(assets: &AssetServer, px: f32, bold: bool) -> TextFont {
    TextFont {
        font: assets
            .load(if bold { fonts::BOLD } else { fonts::REGULAR })
            .into(),
        font_size: FontSize::Px(px),
        ..default()
    }
}

/// Main (or dimmed) theme text colour.
pub(crate) fn ui_color(dim: bool) -> ThemeTextColor {
    ThemeTextColor(if dim {
        tokens::TEXT_DIM
    } else {
        tokens::TEXT_MAIN
    })
}

/// A pane's body background.
pub(crate) fn pane_bg() -> ThemeBackgroundColor {
    ThemeBackgroundColor(tokens::PANE_BODY_BG)
}

/// Which pane a close button acts on.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaneToggle {
    /// The left pane's close: hides it (Model shows it again).
    Outline,
    /// The material graph's close: back to Model mode.
    Material,
}

pub(crate) fn on_toggle(
    activate: On<Activate>,
    which: Query<&PaneToggle>,
    mut outline: ResMut<OutlinePane>,
    mut wb: ResMut<Workbench>,
) {
    let Ok(w) = which.get(activate.event_target()) else {
        return;
    };
    match w {
        PaneToggle::Outline => outline.visible = !outline.visible,
        PaneToggle::Material => wb.mode = Mode::Model,
    }
}

/// A pane's header row: a bold title (the returned text entity, for live
/// titles) and a close button. Spawned as a child of `pane`.
pub(crate) fn spawn_pane_header(
    commands: &mut Commands,
    assets: &AssetServer,
    pane: Entity,
    which: PaneToggle,
    title: impl Bundle,
) {
    let header = commands
        .spawn((
            Name::new("pane header"),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                height: Val::Px(HEADER_H),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::horizontal(Val::Px(8.0)),
                border: UiRect::bottom(Val::Px(1.0)),
                ..default()
            },
            ThemeBackgroundColor(tokens::PANE_HEADER_BG),
            ThemeBorderColor(tokens::PANE_HEADER_BORDER),
            InheritableThemeTextColor(tokens::PANE_HEADER_TEXT),
            GlobalZIndex(2),
            ChildOf(pane),
        ))
        .id();
    commands.spawn((
        title,
        ui_text(assets, 13.0, true),
        TextLayout::no_wrap(),
        ThemedText,
        Node {
            flex_shrink: 1.0,
            overflow: Overflow::clip(),
            ..default()
        },
        ChildOf(header),
    ));
    commands
        .spawn_scene(bsn! {
            @FeathersToolButton {
                @variant: ButtonVariant::Plain,
                @caption: bsn! { @icon(icons::X) }
            }
        })
        .insert((which, ChildOf(header)))
        .observe(on_toggle);
}

/// An agent the server hasn't seen made its first call: bring the window
/// to the front so the human sees where the new work happens.
pub(crate) fn focus_on_new_agent(
    mut modeler: ResMut<Modeler>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if !modeler.session.focus_requested {
        return;
    }
    modeler.session.focus_requested = false;
    for mut w in &mut windows {
        w.focused = true;
    }
}

/// The theme's row height in pixels (for laying out under the header).
#[allow(dead_code)]
pub(crate) fn row_height() -> Val {
    size::ROW_HEIGHT
}
