//! The style pane (S): the model's design tokens, under the outliner in the
//! left column. Colours as chips with their hex, material presets with their
//! look, numbers, field defaults and the brief. Read-only: agents edit the
//! style with the `style` tool, and the pane follows.

use modeler::style::Style;

use crate::prelude::*;

#[derive(Resource, Default)]
pub(crate) struct StylePane {
    pub(crate) visible: bool,
    pub(crate) hover: bool,
    /// The style the list was built for.
    shown: Option<String>,
}

impl StylePane {
    pub(crate) fn open(visible: bool) -> Self {
        StylePane {
            visible,
            ..default()
        }
    }
}

#[derive(Component)]
pub(crate) struct StyleHud;

#[derive(Component)]
pub(crate) struct StyleList;

#[derive(Component)]
pub(crate) struct StyleTitle;

fn chip(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    let b = |i: usize| {
        h.get(i..i + 2)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .unwrap_or(255)
    };
    Color::srgba_u8(b(0), b(2), b(4), b(6))
}

/// S toggles; rebuilds the list when the style changes; wheel scrolls.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_style_pane(
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    typing: Res<PinTyping>,
    modeler: Res<Modeler>,
    mut pane: ResMut<StylePane>,
    mut hud: Query<(&mut Visibility, &ComputedNode, &UiGlobalTransform), With<StyleHud>>,
    mut list: Query<(Entity, &mut ScrollPosition), With<StyleList>>,
    mut title: Query<&mut Text, With<StyleTitle>>,
    assets: Res<AssetServer>,
    mut commands: Commands,
) {
    if keys.just_pressed(KeyCode::KeyS) && !typing.open {
        pane.visible = !pane.visible;
    }
    let Ok((mut vis, computed, xf)) = hud.single_mut() else {
        return;
    };
    *vis = if pane.visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if !pane.visible {
        pane.hover = false;
        return;
    }
    // The pane's rectangle in logical window pixels.
    let inv = computed.inverse_scale_factor();
    let size = computed.size() * inv;
    let center = xf.translation * inv;
    let min = center - size * 0.5;
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    pane.hover = cursor.is_some_and(|c| {
        c.x >= min.x && c.y >= min.y && c.x < min.x + size.x && c.y < min.y + size.y
    });
    let Ok((list_e, mut pos)) = list.single_mut() else {
        return;
    };
    if pane.hover {
        let lines = scroll_lines(&scroll);
        if lines != 0.0 {
            pos.y = (pos.y - lines * 30.0).max(0.0);
        }
    }
    let style = &modeler.session.model.style;
    let key = serde_json::to_string(style).unwrap_or_default();
    if pane.shown.as_deref() == Some(key.as_str()) {
        return;
    }
    pane.shown = Some(key);
    if let Ok(mut t) = title.single_mut() {
        t.0 = if style.name.is_empty() {
            "Style".into()
        } else {
            format!("Style  ·  {}", style.name)
        };
    }
    commands.entity(list_e).despawn_children();
    build_rows(&mut commands, &assets, list_e, style);
}

fn build_rows(commands: &mut Commands, assets: &AssetServer, list: Entity, style: &Style) {
    let text =
        |commands: &mut Commands, parent: Entity, s: String, px: f32, bold: bool, dim: bool| {
            commands.spawn((
                Text::new(s),
                ui_text(assets, px, bold),
                ui_color(dim),
                TextLayout::no_wrap(),
                Pickable::IGNORE,
                ChildOf(parent),
            ));
        };
    let heading = |commands: &mut Commands, s: &str| {
        let row = commands
            .spawn((
                Node {
                    padding: UiRect {
                        left: Val::Px(10.0),
                        top: Val::Px(10.0),
                        bottom: Val::Px(4.0),
                        ..default()
                    },
                    ..default()
                },
                ChildOf(list),
            ))
            .id();
        text(commands, row, s.to_string(), 11.0, true, true);
    };
    let row = |commands: &mut Commands, swatch: Option<Color>, round: bool| {
        let r = commands
            .spawn((
                Node {
                    min_height: Val::Px(22.0),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(8.0),
                    padding: UiRect::horizontal(Val::Px(12.0)),
                    ..default()
                },
                ChildOf(list),
            ))
            .id();
        if let Some(c) = swatch {
            commands.spawn((
                Node {
                    width: Val::Px(16.0),
                    height: Val::Px(16.0),
                    border_radius: BorderRadius::all(Val::Px(if round { 8.0 } else { 3.0 })),
                    ..default()
                },
                BackgroundColor(c),
                ChildOf(r),
            ));
        }
        r
    };
    let spacer = |commands: &mut Commands, parent: Entity| {
        commands.spawn((
            Node {
                flex_grow: 1.0,
                ..default()
            },
            ChildOf(parent),
        ));
    };
    if style.is_empty() {
        let r = row(commands, None, false);
        text(
            commands,
            r,
            "No style yet: agents set tokens with the style tool".into(),
            12.0,
            false,
            true,
        );
        return;
    }
    if !style.brief.is_empty() {
        heading(commands, "BRIEF");
        let r = commands
            .spawn((
                Node {
                    padding: UiRect::horizontal(Val::Px(12.0)),
                    ..default()
                },
                ChildOf(list),
            ))
            .id();
        commands.spawn((
            Text::new(style.brief.clone()),
            ui_text(assets, 12.0, false),
            ui_color(false),
            Pickable::IGNORE,
            ChildOf(r),
        ));
    }
    if !style.colors.is_empty() {
        heading(commands, "COLOURS  @name");
        for (k, v) in &style.colors {
            let hex = style.resolve_color(&format!("@{k}")).unwrap_or_default();
            let r = row(commands, Some(chip(&hex)), false);
            text(commands, r, format!("@{k}"), 13.0, false, false);
            spacer(commands, r);
            let shown = match v.as_str() {
                Some(s) if s.starts_with('@') => format!("{s}  {}", &hex[..hex.len().min(7)]),
                _ => hex[..hex.len().min(7)].to_string(),
            };
            text(commands, r, shown, 11.0, false, true);
        }
    }
    if !style.materials.is_empty() {
        heading(commands, "MATERIALS  \"style\": name");
        for (k, m) in &style.materials {
            let col = m
                .get("color")
                .or_else(|| m.get("base_color"))
                .and_then(|c| match c {
                    serde_json::Value::String(s) if s.starts_with('@') => {
                        style.resolve_color(s).ok()
                    }
                    serde_json::Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .map_or(Color::srgb(0.8, 0.8, 0.8), |h| chip(&h));
            let r = row(commands, Some(col), true);
            text(commands, r, k.clone(), 13.0, false, false);
            spacer(commands, r);
            let mut d = Vec::new();
            for (f, label) in [
                ("roughness", "rough"),
                ("metalness", "metal"),
                ("pbr", "pbr"),
            ] {
                if let Some(v) = m.get(f) {
                    d.push(format!(
                        "{label} {}",
                        v.as_str().map_or(v.to_string(), String::from)
                    ));
                }
            }
            text(commands, r, d.join("  "), 11.0, false, true);
        }
    }
    if !style.numbers.is_empty() {
        heading(commands, "NUMBERS  $name");
        for (k, v) in &style.numbers {
            let r = row(commands, None, false);
            text(commands, r, format!("${k}"), 13.0, false, false);
            spacer(commands, r);
            text(commands, r, format!("{v}"), 12.0, false, true);
        }
    }
    if !style.defaults.is_empty() {
        heading(commands, "DEFAULTS  filled when omitted");
        for (ty, fields) in &style.defaults {
            for (f, v) in fields {
                let r = row(commands, None, false);
                text(commands, r, format!("{ty}.{f}"), 13.0, false, false);
                spacer(commands, r);
                text(
                    commands,
                    r,
                    v.as_str().map_or(v.to_string(), String::from),
                    12.0,
                    false,
                    true,
                );
            }
        }
    }
}

/// The pane's UI tree: under the outliner in the left column.
pub(crate) fn setup_style_pane(mut commands: Commands, assets: Res<AssetServer>) {
    let pane = commands
        .spawn((
            Name::new("style pane"),
            StyleHud,
            Visibility::Hidden,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Percent(35.0),
                left: Val::Px(0.0),
                width: Val::Percent(PANE_FRACTION * 100.0),
                height: Val::Percent(35.0),
                overflow: Overflow::clip(),
                border: UiRect::top(Val::Px(1.0)),
                ..default()
            },
            pane_bg(),
        ))
        .id();
    commands.spawn((
        StyleList,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(HEADER_H),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            bottom: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            overflow: Overflow::scroll_y(),
            padding: UiRect::bottom(Val::Px(8.0)),
            ..default()
        },
        ScrollPosition::default(),
        ChildOf(pane),
    ));
    spawn_pane_header(
        &mut commands,
        &assets,
        pane,
        PaneToggle::Style,
        (Text::new("Style"), StyleTitle),
    );
}
