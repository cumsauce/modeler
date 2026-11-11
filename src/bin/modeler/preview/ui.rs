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

/// Share of the window height the material strip takes along the bottom.
pub(crate) const STRIP_FRACTION: f32 = 0.3;

/// Window layout: the outliner (or the graph, G) on the left, the material
/// strip (M) along the bottom under both, the 3D view in what is left. The
/// 3D camera renders to that rectangle only, so screenshots never include a
/// pane.
pub(crate) fn apply_layout(
    windows: Query<&Window, With<PrimaryWindow>>,
    graph: Res<GraphHudVisible>,
    mat: Res<MatPane>,
    outline: Res<OutlinePane>,
    mut left: Query<&mut Node, Or<(With<GraphHud>, With<OutlineHud>)>>,
    mut cameras: Query<&mut Camera, With<MainCamera>>,
) {
    let (Ok(window), Ok(mut camera)) = (windows.single(), cameras.single_mut()) else {
        return;
    };
    let left_open = graph.0 || outline.visible;
    let h_pct = if mat.visible {
        100.0 - STRIP_FRACTION * 100.0
    } else {
        100.0
    };
    for mut n in &mut left {
        if n.height != Val::Percent(h_pct) {
            n.height = Val::Percent(h_pct);
        }
    }
    let (w, h) = (window.physical_width(), window.physical_height());
    let x = if left_open {
        (w as f32 * PANE_FRACTION).round() as u32
    } else {
        0
    };
    let bottom = if mat.visible {
        (h as f32 * STRIP_FRACTION).round() as u32
    } else {
        0
    };
    let want = (left_open || mat.visible).then(|| bevy::camera::Viewport {
        physical_position: UVec2::new(x, 0),
        physical_size: UVec2::new((w - x.min(w)).max(1), (h - bottom.min(h)).max(1)),
        ..default()
    });
    let same = match (&camera.viewport, &want) {
        (Some(a), Some(b)) => {
            a.physical_position == b.physical_position && a.physical_size == b.physical_size
        }
        (None, None) => true,
        _ => false,
    };
    if !same {
        camera.viewport = want;
    }
}

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
    Outline,
}

fn toggle(
    which: PaneToggle,
    graph: &mut GraphHudVisible,
    mat: &mut MatPane,
    outline: &mut OutlinePane,
    panel: &mut GraphPanel,
) {
    match which {
        PaneToggle::Outline => {
            // The graph shares the left side: O over it brings the outliner back.
            if graph.0 {
                graph.0 = false;
                outline.visible = true;
            } else {
                outline.visible = !outline.visible;
            }
        }
        PaneToggle::Graph => {
            graph.0 = !graph.0;
            panel.user_view = false;
            panel.version = None;
        }
        PaneToggle::Material => {
            mat.visible = !mat.visible;
        }
    }
}

fn on_toggle(
    activate: On<Activate>,
    which: Query<&PaneToggle>,
    mut graph: ResMut<GraphHudVisible>,
    mut mat: ResMut<MatPane>,
    mut outline: ResMut<OutlinePane>,
    mut panel: ResMut<GraphPanel>,
) {
    if let Ok(w) = which.get(activate.event_target()) {
        toggle(*w, &mut graph, &mut mat, &mut outline, &mut panel);
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
        (PaneToggle::Outline, "Outliner  O"),
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
    outline: Res<OutlinePane>,
    mut buttons: Query<(&PaneToggle, &mut ButtonVariant), With<ToolbarToggle>>,
) {
    for (which, mut variant) in &mut buttons {
        let open = match which {
            PaneToggle::Graph => graph.0,
            PaneToggle::Material => mat.visible,
            PaneToggle::Outline => outline.visible && !graph.0,
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
