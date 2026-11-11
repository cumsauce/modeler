//! Shared window chrome built on Bevy Feathers (the editor widget set): the
//! dark theme, Fira fonts, pane headers with a close button, and the toolbar
//! over the 3D view that toggles the graph (G) and material (M) panes.

use bevy::feathers::constants::{fonts, icons, size};
use bevy::feathers::controls::{ButtonVariant, FeathersToolButton};
use bevy::feathers::display::{caption, icon};
use bevy::feathers::theme::{
    InheritableThemeTextColor, ThemeBackgroundColor, ThemeBorderColor, ThemeTextColor, ThemedText,
};
use bevy::feathers::tokens;
use bevy::scene::{CommandsSceneExt, bsn};
use bevy::ui_widgets::Activate;

use crate::prelude::*;

/// Share of the window width a side pane (graph or material) takes; the 3D
/// view gets the rest.
pub(crate) const PANE_FRACTION: f32 = 0.4;

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

/// Which pane a close button or toolbar toggle acts on.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaneToggle {
    Graph,
    Material,
}

fn toggle(
    which: PaneToggle,
    graph: &mut GraphHudVisible,
    mat: &mut MatPane,
    panel: &mut GraphPanel,
) {
    match which {
        PaneToggle::Graph => {
            graph.0 = !graph.0;
            panel.user_view = false;
            panel.version = None;
            if graph.0 {
                mat.visible = false;
            }
        }
        PaneToggle::Material => {
            mat.visible = !mat.visible;
            if mat.visible {
                graph.0 = false;
                panel.version = None;
            }
        }
    }
}

fn on_toggle(
    activate: On<Activate>,
    which: Query<&PaneToggle>,
    mut graph: ResMut<GraphHudVisible>,
    mut mat: ResMut<MatPane>,
    mut panel: ResMut<GraphPanel>,
) {
    if let Ok(w) = which.get(activate.event_target()) {
        toggle(*w, &mut graph, &mut mat, &mut panel);
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

/// The toolbar floating over the 3D view's top-right corner.
pub(crate) fn setup_toolbar(mut commands: Commands) {
    let bar = commands
        .spawn((
            Name::new("toolbar"),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                right: Val::Px(8.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(4.0),
                padding: UiRect::all(Val::Px(4.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            ThemeBackgroundColor(tokens::PANE_HEADER_BG),
            GlobalZIndex(3),
        ))
        .id();
    for (which, label) in [
        (PaneToggle::Graph, "Graph  G"),
        (PaneToggle::Material, "Material  M"),
    ] {
        commands
            .spawn_scene(bsn! {
                @FeathersToolButton {
                    @variant: ButtonVariant::Normal,
                    @caption: bsn! { @caption(label) }
                }
            })
            .insert((which, ToolbarToggle, ChildOf(bar)))
            .observe(on_toggle);
    }
}

/// Marks the toolbar's toggles (their variant shows which pane is open).
#[derive(Component)]
pub(crate) struct ToolbarToggle;

/// Highlights the toolbar toggle of the open pane.
pub(crate) fn sync_toolbar(
    graph: Res<GraphHudVisible>,
    mat: Res<MatPane>,
    mut buttons: Query<(&PaneToggle, &mut ButtonVariant), With<ToolbarToggle>>,
) {
    for (which, mut variant) in &mut buttons {
        let open = match which {
            PaneToggle::Graph => graph.0,
            PaneToggle::Material => mat.visible,
        };
        let want = ButtonVariant::selected(open);
        if *variant != want {
            *variant = want;
        }
    }
}

/// The theme's row height in pixels (for laying out under the header).
#[allow(dead_code)]
pub(crate) fn row_height() -> Val {
    size::ROW_HEIGHT
}
