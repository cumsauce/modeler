//! Shared window chrome built on Bevy Feathers (the editor widget set):
//! Fira fonts and the panes' caption headers (the palette and components are
//! `ds.rs`). The layout
//! is `workbench.rs`, the top bar `top_bar.rs`.

use bevy::feathers::constants::size;
use bevy::feathers::theme::{ThemeBackgroundColor, ThemeTextColor};
use bevy::feathers::tokens;

use crate::prelude::*;

/// Height of a pane header row.
pub(crate) const HEADER_H: f32 = 30.0;

/// Text in the theme's UI font.
pub(crate) fn ui_text(assets: &AssetServer, px: f32, bold: bool) -> TextFont {
    TextFont {
        font: ds::font(
            assets,
            if bold {
                ds::Face::Medium
            } else {
                ds::Face::Regular
            },
        )
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

/// A pane's header row: a quiet caption (`title` carries the text, for live
/// titles). Spawned as a child of `pane`.
pub(crate) fn spawn_pane_header(
    commands: &mut Commands,
    assets: &AssetServer,
    pane: Entity,
    which: PaneToggle,
    title: impl Bundle,
) {
    // The concept's panes have no title bar: a quiet caption over the list
    // (the panes come and go with the mode, so no close button).
    let _ = which;
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
                padding: UiRect::horizontal(Val::Px(ds::PAD)),
                ..default()
            },
            BackgroundColor(ds::PANEL),
            GlobalZIndex(2),
            ChildOf(pane),
        ))
        .id();
    commands.spawn((
        title,
        ui_text(assets, 11.0, false),
        TextColor(ds::FAINT),
        TextLayout::no_wrap(),
        Node {
            flex_shrink: 1.0,
            overflow: Overflow::clip(),
            ..default()
        },
        ChildOf(header),
    ));
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
