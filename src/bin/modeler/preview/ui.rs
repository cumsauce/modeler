//! Shared window chrome built on Bevy Feathers (the editor widget set): the
//! dark theme, Fira fonts, pane headers with a close button, and the toolbar
//! over the 3D view that toggles the outliner (O), style (S) and material (M) panes.

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

/// Window layout: the outliner on the left with the style pane (S) under it,
/// the material strip (M) along the bottom under both, the 3D view in what
/// is left. The
/// 3D camera renders to that rectangle only, so screenshots never include a
/// pane.
pub(crate) fn apply_layout(
    windows: Query<&Window, With<PrimaryWindow>>,
    style: Res<StylePane>,
    mat: Res<MatPane>,
    outline: Res<OutlinePane>,
    mut outline_nodes: Query<&mut Node, (With<OutlineHud>, Without<StyleHud>)>,
    mut style_nodes: Query<&mut Node, (With<StyleHud>, Without<OutlineHud>)>,
    mut cameras: Query<&mut RenderTarget, With<MainCamera>>,
    (mut view, mut images): (ResMut<View3d>, ResMut<Assets<Image>>),
    mut node: Query<&mut Node, (With<View3dNode>, Without<OutlineHud>, Without<StyleHud>)>,
    mut shown: Query<&mut ImageNode, With<View3dNode>>,
) {
    let (Ok(window), Ok(mut target)) = (windows.single(), cameras.single_mut()) else {
        return;
    };
    let left_open = style.visible || outline.visible;
    let h_pct = if mat.visible {
        100.0 - STRIP_FRACTION * 100.0
    } else {
        100.0
    };
    // Both open: the outliner takes the top 55%, the style pane the rest.
    let (o_h, s_top, s_h) = match (outline.visible, style.visible) {
        (true, true) => (h_pct * 0.55, h_pct * 0.55, h_pct * 0.45),
        _ => (h_pct, 0.0, h_pct),
    };
    let set = |n: &mut Node, top: f32, h: f32| {
        if n.top != Val::Percent(top) {
            n.top = Val::Percent(top);
        }
        if n.height != Val::Percent(h) {
            n.height = Val::Percent(h);
        }
    };
    for mut n in &mut outline_nodes {
        set(&mut n, 0.0, o_h);
    }
    for mut n in &mut style_nodes {
        set(&mut n, s_top, s_h);
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
    let size = UVec2::new((w - x.min(w)).max(1), (h - bottom.min(h)).max(1));
    let sf = window.scale_factor();
    let rect = Rect::from_corners(
        Vec2::new(x as f32, 0.0) / sf,
        Vec2::new(x as f32 + size.x as f32, size.y as f32) / sf,
    );
    view.rect = rect;
    if let Ok(mut n) = node.single_mut() {
        let want = (
            Val::Px(rect.min.x),
            Val::Px(rect.min.y),
            Val::Px(rect.width()),
            Val::Px(rect.height()),
        );
        if (n.left, n.top, n.width, n.height) != want {
            (n.left, n.top, n.width, n.height) = want;
        }
    }
    if view.size != size {
        // A fresh target at the new size (as the agent rig does): the
        // camera and the UI node switch to it together.
        view.size = size;
        let fresh = images.add(view_target(size.x, size.y));
        view.image = fresh.clone();
        *target = RenderTarget::Image(fresh.clone().into());
        if let Ok(mut img) = shown.single_mut() {
            img.image = fresh;
        }
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
    Style,
    Material,
    Outline,
}

fn toggle(which: PaneToggle, style: &mut StylePane, mat: &mut MatPane, outline: &mut OutlinePane) {
    match which {
        PaneToggle::Outline => outline.visible = !outline.visible,
        PaneToggle::Style => style.visible = !style.visible,
        PaneToggle::Material => mat.visible = !mat.visible,
    }
}

fn on_toggle(
    activate: On<Activate>,
    which: Query<&PaneToggle>,
    mut style: ResMut<StylePane>,
    mut mat: ResMut<MatPane>,
    mut outline: ResMut<OutlinePane>,
) {
    if let Ok(w) = which.get(activate.event_target()) {
        toggle(*w, &mut style, &mut mat, &mut outline);
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
        (PaneToggle::Style, "Style  S"),
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
    style: Res<StylePane>,
    mat: Res<MatPane>,
    outline: Res<OutlinePane>,
    mut buttons: Query<(&PaneToggle, &mut ButtonVariant), With<ToolbarToggle>>,
) {
    for (which, mut variant) in &mut buttons {
        let open = match which {
            PaneToggle::Style => style.visible,
            PaneToggle::Material => mat.visible,
            PaneToggle::Outline => outline.visible,
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
