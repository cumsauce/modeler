//! Map mode's minimap: a small top-down orthographic view at the 3D view's
//! bottom right (+x right, -z up, like the `top` agent view) with the
//! camera's position and heading drawn over it; a click (or drag) moves the
//! camera's target there.
//!
//! Cheap by design: the image depends only on the model, so its camera
//! renders for a few frames after the model changes or Map opens and is
//! idle otherwise. The camera marker is a UI overlay moved every frame.

use crate::prelude::*;

/// Minimap size (logical px) and its texture size.
const SIZE: f32 = 180.0;
const TEX: u32 = 360;
/// Frames the camera renders after a change (the first may be before the
/// scene is spawned).
const RENDER_FRAMES: u8 = 4;

#[derive(Component)]
struct MinimapCamera;

#[derive(Component)]
struct MinimapNode;

#[derive(Component)]
struct MinimapDot;

#[derive(Component)]
struct MinimapHeading;

#[derive(Resource, Default)]
struct Minimap {
    /// Model version and map-mode entry the image was rendered for.
    shown: Option<(u64, String)>,
    frames: u8,
    /// World xz centre and half-extent of the square the image covers.
    centre: Vec2,
    half: f32,
    dragging: bool,
}

pub(crate) fn minimap_plugin(app: &mut App) {
    app.init_resource::<Minimap>()
        .add_systems(Startup, setup_minimap)
        .add_systems(
            Update,
            (refresh_minimap, minimap_input, draw_marker)
                .chain()
                .after(rebuild_scene)
                .run_if(windowed),
        );
}

fn setup_minimap(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = images.add(view_target(TEX, TEX));
    commands.spawn((
        Name::new("minimap camera"),
        MinimapCamera,
        Camera3d::default(),
        Camera {
            is_active: false,
            order: -2,
            clear_color: ClearColorConfig::Custom(ds::BG),
            ..default()
        },
        RenderTarget::Image(image.clone().into()),
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed {
                width: 10.0,
                height: 10.0,
            },
            near: 0.0,
            far: 2000.0,
            ..OrthographicProjection::default_3d()
        }),
        AmbientLight {
            brightness: 400.0,
            ..default()
        },
        Msaa::Off,
        RenderLayers::layer(0),
        Transform::from_xyz(0.0, 100.0, 0.0).looking_at(Vec3::ZERO, Vec3::NEG_Z),
    ));
    let node = commands
        .spawn((
            Name::new("minimap"),
            MinimapNode,
            InSlot(PaneSlot::Centre),
            ImageNode::new(image),
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(8.0),
                bottom: Val::Px(8.0),
                width: Val::Px(SIZE),
                height: Val::Px(SIZE),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            BorderColor::all(ds::LINE),
            BlocksCamera,
            Visibility::Hidden,
        ))
        .id();
    commands.spawn((
        MinimapHeading,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Px(22.0),
            height: Val::Px(2.0),
            ..default()
        },
        BackgroundColor(ds::ACCENT),
        Pickable::IGNORE,
        ChildOf(node),
    ));
    commands.spawn((
        MinimapDot,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Px(8.0),
            height: Val::Px(8.0),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(ds::ACCENT),
        Pickable::IGNORE,
        ChildOf(node),
    ));
}

fn shown(wb: &Workbench, grid: &Grid) -> bool {
    wb.mode == Mode::Map && !grid.on
}

/// Re-frames and re-renders the image when the model changes or Map opens.
fn refresh_minimap(
    wb: Res<Workbench>,
    grid: Res<Grid>,
    modeler: Res<Modeler>,
    mut mm: ResMut<Minimap>,
    mut cams: Query<(&mut Camera, &mut Transform, &mut Projection), With<MinimapCamera>>,
    mut node: Query<&mut Visibility, With<MinimapNode>>,
) {
    let on = shown(&wb, &grid);
    if let Ok(mut v) = node.single_mut() {
        v.set_if_neq(if on {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    let Ok((mut cam, mut tf, mut proj)) = cams.single_mut() else {
        return;
    };
    if !on {
        mm.shown = None;
        if cam.is_active {
            cam.is_active = false;
        }
        return;
    }
    let model = &modeler.session.model;
    let key = (model.version, model.name.clone());
    if mm.shown.as_ref() != Some(&key) {
        mm.shown = Some(key);
        mm.frames = RENDER_FRAMES;
        let (min, max) = model
            .bounds()
            .unwrap_or((Vec3::splat(-5.0), Vec3::splat(5.0)));
        mm.centre = Vec2::new(min.x + max.x, min.z + max.z) * 0.5;
        mm.half = ((max.x - min.x).max(max.z - min.z) * 0.55).max(0.5);
        let top = max.y + 10.0;
        *tf = Transform::from_xyz(mm.centre.x, top, mm.centre.y)
            .looking_at(Vec3::new(mm.centre.x, min.y, mm.centre.y), Vec3::NEG_Z);
        if let Projection::Orthographic(o) = proj.as_mut() {
            o.scaling_mode = ScalingMode::Fixed {
                width: mm.half * 2.0,
                height: mm.half * 2.0,
            };
            o.far = top - min.y + 10.0;
        }
    }
    let want = mm.frames > 0;
    if mm.frames > 0 {
        mm.frames -= 1;
    }
    if cam.is_active != want {
        cam.is_active = want;
    }
}

/// A click or drag on the minimap moves the camera's target there.
fn minimap_input(
    wb: Res<Workbench>,
    grid: Res<Grid>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    node: Query<(&ComputedNode, &UiGlobalTransform), With<MinimapNode>>,
    mut mm: ResMut<Minimap>,
    mut orbit: ResMut<Orbit>,
) {
    if !shown(&wb, &grid) || !mouse.pressed(MouseButton::Left) {
        mm.dragging = false;
        return;
    }
    let (Ok(window), Ok((n, at))) = (windows.single(), node.single()) else {
        return;
    };
    let Some(c) = window.cursor_position().map(|c| c * window.scale_factor()) else {
        return;
    };
    let half = n.size() * 0.5;
    let local = (c - at.translation) / half.max(Vec2::ONE); // -1..1
    let inside = local.x.abs() <= 1.0 && local.y.abs() <= 1.0;
    if mouse.just_pressed(MouseButton::Left) && inside {
        mm.dragging = true;
    }
    if mm.dragging {
        let l = local.clamp(Vec2::NEG_ONE, Vec2::ONE);
        // Image +x is world +x, image down is world +z.
        orbit.target.x = mm.centre.x + l.x * mm.half;
        orbit.target.z = mm.centre.y + l.y * mm.half;
        orbit.user = true;
    }
}

/// The camera's eye and heading over the image.
fn draw_marker(
    wb: Res<Workbench>,
    grid: Res<Grid>,
    mm: Res<Minimap>,
    orbit: Res<Orbit>,
    mut dot: Query<&mut Node, (With<MinimapDot>, Without<MinimapHeading>)>,
    mut heading: Query<(&mut Node, &mut UiTransform), With<MinimapHeading>>,
) {
    if !shown(&wb, &grid) || mm.half <= 0.0 {
        return;
    }
    let eye = orbit_eye(&orbit);
    let to_px = |p: Vec3| {
        let l = (Vec2::new(p.x, p.z) - mm.centre) / mm.half; // -1..1
        (l.clamp(Vec2::NEG_ONE, Vec2::ONE) * 0.5 + 0.5) * SIZE
    };
    let e = to_px(eye);
    let set = |n: &mut Node, x: f32, y: f32| {
        let (l, t) = (Val::Px(x), Val::Px(y));
        if n.left != l || n.top != t {
            n.left = l;
            n.top = t;
        }
    };
    if let Ok(mut n) = dot.single_mut() {
        set(&mut n, e.x - 4.0, e.y - 4.0);
    }
    if let Ok((mut n, mut tf)) = heading.single_mut() {
        // The view direction in the image (x right, z down).
        let f = -orbit_dir(&orbit);
        let d = Vec2::new(f.x, f.z);
        let angle = if d.length_squared() > 1e-6 {
            d.y.atan2(d.x)
        } else {
            0.0
        };
        // A 22 px bar centred 11 px ahead of the eye, rotated about its centre.
        let c = e + Vec2::from_angle(angle) * 11.0;
        set(&mut n, c.x - 11.0, c.y - 1.0);
        let want = UiTransform::from_rotation(Rot2::radians(angle));
        if *tf != want {
            *tf = want;
        }
    }
}
