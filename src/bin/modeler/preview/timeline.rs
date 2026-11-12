//! The Animate dock (`PaneSlot::Dock`, shown in Animate): a header with
//! Play / Pause, the clip (click to cycle), its duration and loop; a ruler
//! to drag (scrubs and pauses); one lane per animated bone with diamond
//! keys from the clip's tracks (mirrored tracks hollow, wave channels a
//! line, sway clips a "simulated" note) and a playhead. Space plays /
//! pauses. In Animate the window also draws the skeleton as gizmos (on
//! [`BONE_LAYER`], which only the window camera sees).
//!
//! The lanes are rebuilt only when the model or the clip changes; the
//! playhead and header text move every frame.

use bevy::gizmos::config::{GizmoConfigGroup, GizmoConfigStore};
use modeler::anim::{Channel, Clip, Skeleton};

use crate::prelude::*;

/// Width of the bone-name column.
const LABEL_W: f32 = 130.0;
const RULER_H: f32 = 22.0;
const LANE_H: f32 = 20.0;
/// Render layer of the bone gizmos: the window camera adds it, agent views
/// and the minimap don't.
pub(crate) const BONE_LAYER: usize = 30;

#[derive(Default, Reflect, GizmoConfigGroup)]
pub(crate) struct BoneGizmos;

/// One bone's lane.
struct Lane {
    bone: String,
    /// (time, hollow = a mirrored track).
    keys: Vec<(f32, bool)>,
    /// A wave channel moves it continuously.
    wave: bool,
}

/// The built clip and its lanes, rebuilt when the model or the clip changes.
#[derive(Resource, Default)]
struct TimelineCache {
    key: Option<(u64, Option<String>)>,
    clip: Option<(Clip, Skeleton)>,
    /// Skeletons at rest (no clip playing).
    rest: Vec<Skeleton>,
    lanes: Vec<Lane>,
    /// How many clips the model has (the name cycles when several).
    clips: usize,
    /// The lanes on screen were built for this key.
    shown: Option<(u64, Option<String>)>,
}

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum DockButton {
    Play,
    Clip,
}

#[derive(Component)]
struct DockText(DockField);

#[derive(Clone, Copy, PartialEq, Eq)]
enum DockField {
    Play,
    Clip,
    Info,
}

#[derive(Component)]
struct Ruler;

#[derive(Component)]
struct LanesBox;

#[derive(Component)]
struct Playhead;

/// A scrub that started on the ruler (holds until the button is released).
#[derive(Resource, Default)]
struct Scrub(bool);

pub(crate) fn timeline_plugin(app: &mut App) {
    app.init_resource::<TimelineCache>()
        .init_resource::<Scrub>()
        .init_gizmo_group::<BoneGizmos>()
        .add_systems(Startup, (setup_dock, setup_bone_gizmos))
        .add_systems(
            Update,
            (
                camera_sees_bones,
                pick_clip,
                refresh_cache,
                dock_input,
                build_lanes,
                update_dock,
                draw_bones,
            )
                .chain()
                .after(play_animation)
                .run_if(windowed),
        );
}

fn setup_bone_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (cfg, _) = store.config_mut::<BoneGizmos>();
    cfg.render_layers = RenderLayers::layer(BONE_LAYER);
    cfg.depth_bias = -1.0; // drawn over the mesh
    cfg.line.width = 2.5;
}

/// The window camera sees the bone layer as well as the scene's.
fn camera_sees_bones(
    mut commands: Commands,
    cams: Query<Entity, (With<MainCamera>, Without<RenderLayers>)>,
) {
    for e in &cams {
        commands
            .entity(e)
            .insert(RenderLayers::from_layers(&[0, BONE_LAYER]));
    }
}

fn chip(bg: f32) -> (Node, BackgroundColor) {
    (
        Node {
            padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(if bg > 0.0 { ds::RAISE } else { Color::NONE }),
    )
}

fn setup_dock(mut commands: Commands, assets: Res<AssetServer>) {
    let root = commands
        .spawn((
            Name::new("animate dock"),
            InSlot(PaneSlot::Dock),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                border: UiRect::top(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(ds::LINE),
        ))
        .id();
    let header = commands
        .spawn((
            Node {
                height: Val::Px(HEADER_H),
                flex_shrink: 0.0,
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                padding: UiRect::horizontal(Val::Px(8.0)),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    for (button, field) in [
        (DockButton::Play, DockField::Play),
        (DockButton::Clip, DockField::Clip),
    ] {
        let b = commands
            .spawn((button, chip(0.08), ChildOf(header)))
            .observe(on_dock_click)
            .id();
        commands.spawn((
            DockText(field),
            Text::new(""),
            ui_text(&assets, 12.0, field == DockField::Clip),
            ui_color(false),
            ChildOf(b),
        ));
    }
    commands.spawn((
        DockText(DockField::Info),
        Text::new(""),
        ui_text(&assets, 12.0, false),
        ui_color(true),
        ChildOf(header),
    ));
    let body = commands
        .spawn((
            Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    // Ruler: tick labels at tenths of the clip.
    let ruler_row = commands
        .spawn((
            Node {
                height: Val::Px(RULER_H),
                flex_shrink: 0.0,
                flex_direction: FlexDirection::Row,
                border: UiRect::vertical(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(ds::LINE),
            ChildOf(body),
        ))
        .id();
    commands.spawn((
        Node {
            width: Val::Px(LABEL_W),
            flex_shrink: 0.0,
            ..default()
        },
        ChildOf(ruler_row),
    ));
    let ruler = commands
        .spawn((
            Ruler,
            Node {
                flex_grow: 1.0,
                ..default()
            },
            BackgroundColor(ds::BG),
            ChildOf(ruler_row),
        ))
        .id();
    for i in 0..=10 {
        let x = i as f32 * 10.0;
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(x),
                width: Val::Px(1.0),
                bottom: Val::Px(0.0),
                height: Val::Px(if i % 5 == 0 { 8.0 } else { 4.0 }),
                ..default()
            },
            BackgroundColor(ds::FAINT),
            Pickable::IGNORE,
            ChildOf(ruler),
        ));
        if i < 10 {
            commands.spawn((
                DockTick(i),
                Text::new(""),
                ui_text(&assets, 10.0, false),
                ui_color(true),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(x + 0.4),
                    top: Val::Px(1.0),
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(ruler),
            ));
        }
    }
    commands.spawn((
        LanesBox,
        Node {
            flex_grow: 1.0,
            flex_direction: FlexDirection::Column,
            overflow: Overflow::scroll_y(),
            ..default()
        },
        ScrollPosition::default(),
        ChildOf(body),
    ));
    // Playhead over ruler and lanes.
    let overlay = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(LABEL_W),
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(body),
        ))
        .id();
    commands.spawn((
        Playhead,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(0.0),
            width: Val::Px(2.0),
            top: Val::Px(0.0),
            bottom: Val::Px(0.0),
            ..default()
        },
        BackgroundColor(ds::ACCENT),
        Pickable::IGNORE,
        ChildOf(overlay),
    ));
}

#[derive(Component)]
struct DockTick(u32);

/// Entering Animate with nothing chosen picks the first clip, paused at 0,
/// so the dock has something to show.
fn pick_clip(wb: Res<Workbench>, modeler: Res<Modeler>, mut pb: ResMut<Playback>) {
    if wb.mode != Mode::Animate || pb.clip.is_some() {
        return;
    }
    if let Some(first) = clip_names(&modeler.session.model).into_iter().next() {
        pb.clip = Some(first);
        pb.time = 0.0;
        pb.playing = false;
    }
}

fn refresh_cache(
    wb: Res<Workbench>,
    modeler: Res<Modeler>,
    pb: Res<Playback>,
    mut cache: ResMut<TimelineCache>,
) {
    if wb.mode != Mode::Animate {
        return;
    }
    let model = &modeler.session.model;
    let key = (model.version, pb.clip.clone());
    if cache.key.as_ref() == Some(&key) {
        return;
    }
    cache.key = Some(key);
    cache.clip = pb
        .clip
        .as_deref()
        .and_then(|c| modeler::anim::clip(model, Some(c)).ok());
    cache.clips = clip_names(model).len();
    cache.rest = modeler::anim::skeletons(model).unwrap_or_default();
    cache.lanes = cache
        .clip
        .as_ref()
        .map(|(c, s)| lanes_of(c, s))
        .unwrap_or_default();
}

fn lanes_of(clip: &Clip, skel: &Skeleton) -> Vec<Lane> {
    let mut lanes: Vec<(usize, Lane)> = Vec::new();
    let mut add = |bone: usize, keys: Vec<(f32, bool)>, wave: bool| {
        if let Some((_, l)) = lanes.iter_mut().find(|(b, _)| *b == bone) {
            l.keys.extend(keys);
            l.wave |= wave;
        } else {
            let name = skel
                .bones
                .get(bone)
                .map(|b| b.name.clone())
                .unwrap_or_default();
            lanes.push((
                bone,
                Lane {
                    bone: name,
                    keys,
                    wave,
                },
            ));
        }
    };
    let mut layer = Some(clip);
    while let Some(c) = layer {
        let reps = (clip.duration / c.duration.max(1e-6)).round().max(1.0) as usize;
        for (bone, track, mirrored, shift) in &c.tracks {
            let mut keys = Vec::new();
            let mut wave = false;
            for ch in [&track.rotate, &track.translate, &track.scale]
                .into_iter()
                .flatten()
            {
                match ch {
                    Channel::Keys { keys: k, .. } => {
                        for (t, _) in k {
                            for r in 0..reps {
                                keys.push((c.wrap(t - shift) + r as f32 * c.duration, *mirrored));
                            }
                        }
                    }
                    Channel::Wave { .. } => wave = true,
                }
            }
            add(*bone, keys, wave);
        }
        layer = c.base.as_deref();
    }
    if clip.layer.is_some() {
        for (bone, ..) in clip.channels(skel) {
            add(bone, Vec::new(), false);
        }
    }
    lanes.sort_by_key(|(b, _)| *b);
    lanes.into_iter().map(|(_, l)| l).collect()
}

fn on_dock_click(
    click: On<PointerClick>,
    which: Query<&DockButton>,
    modeler: Res<Modeler>,
    mut pb: ResMut<Playback>,
) {
    let Ok(b) = which.get(click.entity) else {
        return;
    };
    match b {
        DockButton::Play => {
            if pb.clip.is_some() {
                pb.playing = !pb.playing;
            }
        }
        DockButton::Clip => {
            let names = clip_names(&modeler.session.model);
            if names.is_empty() {
                return;
            }
            let i = pb
                .clip
                .as_ref()
                .and_then(|c| names.iter().position(|n| n == c))
                .map_or(0, |i| (i + 1) % names.len());
            pb.clip = Some(names[i].clone());
            pb.time = 0.0;
        }
    }
}

/// Space, ruler scrubbing and the lanes' wheel.
#[allow(clippy::too_many_arguments)]
fn dock_input(
    wb: Res<Workbench>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    typing: Res<PinTyping>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cache: Res<TimelineCache>,
    ruler: Query<(&ComputedNode, &UiGlobalTransform), With<Ruler>>,
    mut lanes_box: Query<&mut ScrollPosition, With<LanesBox>>,
    mut scrub: ResMut<Scrub>,
    mut pb: ResMut<Playback>,
) {
    if wb.mode != Mode::Animate {
        scrub.0 = false;
        return;
    }
    if !typing.open && keys.just_pressed(KeyCode::Space) && pb.clip.is_some() {
        pb.playing = !pb.playing;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let cursor = window.cursor_position();
    if cursor.is_some_and(|c| wb.rects.dock.contains(c)) {
        let dy = crate::preview::camera::scroll_lines(&scroll);
        if dy != 0.0
            && let Ok(mut sp) = lanes_box.single_mut()
        {
            sp.y = (sp.y - dy * LANE_H).max(0.0);
        }
    }
    let Some(duration) = cache.clip.as_ref().map(|(c, _)| c.duration) else {
        scrub.0 = false;
        return;
    };
    let Ok((node, at)) = ruler.single() else {
        return;
    };
    let sf = window.scale_factor();
    let half = node.size() * 0.5;
    let (min, max) = (at.translation - half, at.translation + half);
    let Some(c) = cursor.map(|c| c * sf) else {
        return;
    };
    if mouse.just_pressed(MouseButton::Left)
        && c.x >= min.x
        && c.x <= max.x
        && c.y >= min.y
        && c.y <= max.y
    {
        scrub.0 = true;
    }
    if !mouse.pressed(MouseButton::Left) {
        scrub.0 = false;
    }
    if scrub.0 && max.x > min.x {
        let f = ((c.x - min.x) / (max.x - min.x)).clamp(0.0, 1.0);
        pb.time = f * duration;
        pb.playing = false;
    }
}

/// Respawns the lanes when the clip or model changed.
fn build_lanes(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut cache: ResMut<TimelineCache>,
    boxes: Query<Entity, With<LanesBox>>,
) {
    if cache.shown == cache.key {
        return;
    }
    let Ok(lbox) = boxes.single() else {
        return;
    };
    cache.shown = cache.key.clone();
    commands.entity(lbox).despawn_related::<Children>();
    let Some((clip, _)) = cache.clip.as_ref() else {
        commands.spawn((
            Text::new("No clip: add a `clip` or `sway` item to animate the skeleton."),
            ui_text(&assets, 12.0, false),
            ui_color(true),
            Node {
                margin: UiRect::all(Val::Px(10.0)),
                ..default()
            },
            ChildOf(lbox),
        ));
        return;
    };
    let d = clip.duration.max(1e-6);
    for lane in &cache.lanes {
        let row = commands
            .spawn((
                Node {
                    height: Val::Px(LANE_H),
                    flex_shrink: 0.0,
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    border: UiRect::bottom(Val::Px(1.0)),
                    ..default()
                },
                BorderColor::all(ds::LINE.with_alpha(0.5)),
                ChildOf(lbox),
            ))
            .id();
        commands.spawn((
            Text::new(lane.bone.clone()),
            ui_text(&assets, 11.0, false),
            ui_color(false),
            TextLayout::no_wrap(),
            Node {
                width: Val::Px(LABEL_W),
                flex_shrink: 0.0,
                padding: UiRect::left(Val::Px(10.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(row),
        ));
        let track = commands
            .spawn((
                Node {
                    flex_grow: 1.0,
                    height: Val::Percent(100.0),
                    ..default()
                },
                ChildOf(row),
            ))
            .id();
        if lane.wave || lane.keys.is_empty() {
            // A wave (or simulated motion) moves the bone all through the clip.
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    top: Val::Px(LANE_H * 0.5 - 1.0),
                    height: Val::Px(2.0),
                    ..default()
                },
                BackgroundColor(if lane.wave {
                    ds::ACCENT.with_alpha(0.45)
                } else {
                    ds::OK.with_alpha(0.35)
                }),
                ChildOf(track),
            ));
        }
        for (t, hollow) in &lane.keys {
            let col = ds::WARN;
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent((t / d).clamp(0.0, 1.0) * 100.0),
                    top: Val::Px(LANE_H * 0.5 - 4.0),
                    width: Val::Px(8.0),
                    height: Val::Px(8.0),
                    margin: UiRect::left(Val::Px(-4.0)),
                    border: UiRect::all(Val::Px(1.5)),
                    ..default()
                },
                UiTransform::from_rotation(Rot2::degrees(45.0)),
                BorderColor::all(col),
                BackgroundColor(if *hollow { Color::NONE } else { col }),
                ChildOf(track),
            ));
        }
    }
}

fn update_dock(
    wb: Res<Workbench>,
    pb: Res<Playback>,
    cache: Res<TimelineCache>,
    mut texts: Query<(&mut Text, Option<&DockText>, Option<&DockTick>)>,
    mut head: Query<&mut Node, With<Playhead>>,
) {
    if wb.mode != Mode::Animate {
        return;
    }
    let clip = cache.clip.as_ref().map(|(c, _)| c);
    let t = clip.map_or(0.0, |c| c.wrap(pb.time));
    let d = clip.map_or(1.0, |c| c.duration.max(1e-6));
    for (mut text, field, tick) in &mut texts {
        let want = match (field, tick) {
            (Some(DockText(DockField::Play)), _) => {
                if pb.playing { "Pause" } else { "Play" }.to_string()
            }
            (Some(DockText(DockField::Clip)), _) => clip.map_or("no clip".into(), |c| {
                if cache.clips > 1 {
                    format!("{} ▸", c.name)
                } else {
                    c.name.clone()
                }
            }),
            (Some(DockText(DockField::Info)), _) => clip.map_or(String::new(), |c| {
                format!(
                    "{:.2} / {:.2} s   {}{}{}   Space play · drag the ruler to scrub",
                    t,
                    c.duration,
                    if c.looping { "loop" } else { "once" },
                    if (pb.speed - 1.0).abs() > 1e-3 {
                        format!("   {}x", pb.speed)
                    } else {
                        String::new()
                    },
                    if c.layer.is_some() {
                        "   simulated (sway): no keys to edit"
                    } else {
                        ""
                    },
                )
            }),
            (None, Some(DockTick(i))) => {
                if clip.is_some() {
                    format!("{:.2}", d * *i as f32 / 10.0)
                } else {
                    String::new()
                }
            }
            _ => continue,
        };
        if text.0 != want {
            text.0 = want;
        }
    }
    if let Ok(mut n) = head.single_mut() {
        let want = Val::Percent((t / d).clamp(0.0, 1.0) * 100.0);
        if n.left != want {
            n.left = want;
        }
    }
}

/// The skeleton in Animate: posed at the playback time, else at rest.
fn draw_bones(
    wb: Res<Workbench>,
    grid: Res<Grid>,
    pb: Res<Playback>,
    cache: Res<TimelineCache>,
    mut gizmos: Gizmos<BoneGizmos>,
) {
    if wb.mode != Mode::Animate || grid.on {
        return;
    }
    let segs: Vec<(Vec3, Vec3)> = match cache.clip.as_ref() {
        Some((c, s)) => modeler::anim::posed_segments_at(c, s, c.wrap(pb.time)),
        None => cache
            .rest
            .iter()
            .flat_map(|s| s.posed_segments(&s.rest()))
            .collect(),
    };
    let col = ds::WARN;
    for (a, b) in segs {
        gizmos.line(a, b, col);
        let r = ((b - a).length() * 0.06).clamp(0.004, 0.05);
        gizmos.sphere(Isometry3d::from_translation(a), r, col);
    }
}
