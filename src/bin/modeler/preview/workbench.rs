//! The workbench layout (design/workbench-concept.html): a top bar, then
//! either All documents (the grid) or one document in a mode (Model,
//! Material, Animate, Map). In a document: the left pane lists what is in
//! it for that mode, the centre is the 3D view (the node graph in
//! Material), the right pane shows only with a selection, open comments
//! or in Material, and Animate adds a timeline dock under the centre.
//!
//! Every pane spawns its UI under one of the [`PaneSlot`] roots; this file
//! sizes the slots each frame and gives the 3D camera the centre rect.

use crate::prelude::*;

/// Top bar height (logical px).
pub(crate) const TOP_H: f32 = 48.0;
/// Left pane width.
pub(crate) const LEFT_W: f32 = 250.0;
/// Right pane width.
pub(crate) const RIGHT_W: f32 = 290.0;
/// Animate timeline dock height.
pub(crate) const DOCK_H: f32 = 210.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub(crate) enum Mode {
    #[default]
    Model,
    Material,
    Animate,
    Map,
}

impl Mode {
    pub(crate) const ALL: [Mode; 4] = [Mode::Model, Mode::Material, Mode::Animate, Mode::Map];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Mode::Model => "Model",
            Mode::Material => "Material",
            Mode::Animate => "Animate",
            Mode::Map => "Map",
        }
    }

    pub(crate) fn key(self) -> KeyCode {
        match self {
            Mode::Model => KeyCode::Digit1,
            Mode::Material => KeyCode::Digit2,
            Mode::Animate => KeyCode::Digit3,
            Mode::Map => KeyCode::Digit4,
        }
    }
}

/// Whether `model` can be shown in `mode`, or why not (a tooltip).
pub(crate) fn mode_available(model: &Model, mode: Mode) -> Result<(), &'static str> {
    let has = |f: fn(&Op) -> bool| model.ops.iter().any(f);
    match mode {
        Mode::Model | Mode::Material => Ok(()),
        Mode::Animate if has(|o| matches!(o, Op::Skeleton { .. })) => Ok(()),
        Mode::Animate => Err("no skeleton in this document"),
        Mode::Map if has(|o| matches!(o, Op::Terrain { .. } | Op::Marker { .. })) => Ok(()),
        Mode::Map => Err("no terrain or markers in this document"),
    }
}

/// The mode a document opens in: Map for maps, else Model.
pub(crate) fn default_mode(model: &Model) -> Mode {
    if model.ops.iter().any(|o| matches!(o, Op::Terrain { .. })) {
        Mode::Map
    } else {
        Mode::Model
    }
}

/// How the 3D view shades parts (the View menu; `shading.rs`).
#[allow(dead_code)] // Clay / Parts land with the shading stream
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub(crate) enum Shading {
    /// Materials and lights as exported.
    #[default]
    Lit,
    /// One neutral grey: the shape alone.
    Clay,
    /// Each part a flat colour, to tell parts apart.
    Parts,
}

/// The window's UI state, shared by every pane.
#[derive(Resource, Default)]
pub(crate) struct Workbench {
    pub(crate) mode: Mode,
    /// The document `mode` was chosen for: a switch picks its default mode.
    pub(crate) mode_doc: String,
    /// The selected part (3D click or left pane), shown in the right pane.
    pub(crate) selected: Option<String>,
    /// The part under the cursor in 3D.
    pub(crate) hover: Option<String>,
    /// The comments list is open (right pane, pins in 3D, a click pins).
    pub(crate) show_comments: bool,
    #[allow(dead_code)]
    pub(crate) shading: Shading,
    /// The document popup (top bar) is open.
    pub(crate) doc_popup: bool,
    /// The cursor is over UI, not the 3D view: the camera and picking
    /// leave the mouse alone.
    pub(crate) ui_hover: bool,
    /// Logical rects of the slots this frame (zero size when hidden).
    pub(crate) rects: SlotRects,
}

impl Workbench {
    /// The right pane shows: a selection, open comments, or Material.
    pub(crate) fn right_open(&self) -> bool {
        self.selected.is_some() || self.show_comments || self.mode == Mode::Material
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct SlotRects {
    pub(crate) top: Rect,
    pub(crate) left: Rect,
    pub(crate) right: Rect,
    pub(crate) dock: Rect,
    pub(crate) centre: Rect,
}

/// A root UI node that a pane spawns its content under. Children size
/// themselves to it (100% / absolute insets).
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaneSlot {
    TopBar,
    Left,
    Right,
    Dock,
    /// Over the 3D view: HUD chips, and the material graph in Material.
    Centre,
}

/// A UI node over the 3D view that takes the mouse (HUD chips, menus,
/// popups): while the cursor is on one, the camera and picking ignore it.
#[derive(Component)]
pub(crate) struct BlocksCamera;

/// Put on a pane's root at spawn: it is moved under that slot (so panes
/// spawn without waiting for the slots to exist).
#[derive(Component, Clone, Copy)]
pub(crate) struct InSlot(pub(crate) PaneSlot);

fn adopt(
    mut commands: Commands,
    new: Query<(Entity, &InSlot), Added<InSlot>>,
    slots: Query<(Entity, &PaneSlot)>,
) {
    for (e, InSlot(which)) in &new {
        if let Some(s) = slot(&slots, *which) {
            commands.entity(e).insert(ChildOf(s));
        }
    }
}

/// The slot entity, for `ChildOf`.
pub(crate) fn slot(slots: &Query<(Entity, &PaneSlot)>, which: PaneSlot) -> Option<Entity> {
    slots.iter().find(|(_, s)| **s == which).map(|(e, _)| e)
}

pub(crate) fn workbench_plugin(app: &mut App) {
    app.init_resource::<Workbench>()
        .add_systems(Startup, setup_slots)
        .add_systems(
            Update,
            (adopt, follow_document, mode_keys, apply_layout)
                .chain()
                .run_if(windowed),
        );
}

fn setup_slots(mut commands: Commands) {
    for (which, z) in [
        (PaneSlot::Centre, 1),
        (PaneSlot::Dock, 2),
        (PaneSlot::Left, 2),
        (PaneSlot::Right, 2),
        (PaneSlot::TopBar, 4),
    ] {
        let mut e = commands.spawn((
            Name::new(format!("slot {which:?}")),
            which,
            Node {
                position_type: PositionType::Absolute,
                overflow: Overflow::clip(),
                ..default()
            },
            GlobalZIndex(z),
        ));
        // The centre is transparent over the 3D image and only blocks the
        // mouse where a child does.
        if which == PaneSlot::Centre {
            e.insert(Pickable::IGNORE);
        } else {
            e.insert(pane_bg());
        }
    }
}

/// A new active document opens in its default mode; a mode it can't use
/// falls back to Model. The selection belongs to the document it was made in.
fn follow_document(modeler: Res<Modeler>, mut wb: ResMut<Workbench>) {
    let model = &modeler.session.model;
    if wb.mode_doc != model.name {
        wb.mode_doc = model.name.clone();
        wb.mode = default_mode(model);
        wb.selected = None;
        wb.hover = None;
    } else if mode_available(model, wb.mode).is_err() {
        wb.mode = Mode::Model;
    }
}

/// 1-4 switch modes (when the document has them), Esc deselects, C toggles
/// comments, G toggles All documents.
fn mode_keys(
    keys: Res<ButtonInput<KeyCode>>,
    typing: Res<PinTyping>,
    modeler: Res<Modeler>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut wb: ResMut<Workbench>,
    mut grid: ResMut<Grid>,
    focus: Res<bevy::input_focus::InputFocus>,
) {
    // Typing in any text field (comment, search, filter) leaves keys alone.
    if typing.open || flying(&mouse) || focus.get().is_some() {
        return;
    }
    for m in Mode::ALL {
        if keys.just_pressed(m.key()) && mode_available(&modeler.session.model, m).is_ok() {
            wb.mode = m;
            grid.on = false;
        }
    }
    if keys.just_pressed(KeyCode::Escape) {
        if wb.doc_popup {
            wb.doc_popup = false;
        } else {
            wb.selected = None;
        }
    }
    if keys.just_pressed(KeyCode::KeyC) && !grid.on {
        wb.show_comments = !wb.show_comments;
    }
}

/// Sizes the slots and the 3D view (the centre rect) every frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_layout(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut wb: ResMut<Workbench>,
    grid: Res<Grid>,
    mut slots: Query<(&PaneSlot, &mut Node, &mut Visibility), Without<View3dNode>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &InheritedVisibility), With<BlocksCamera>>,
    mut cameras: Query<(&mut RenderTarget, &mut Camera), With<MainCamera>>,
    (mut view, mut images): (ResMut<View3d>, ResMut<Assets<Image>>),
    mut node: Query<(&mut Node, &mut Visibility, &mut ImageNode), With<View3dNode>>,
) {
    let (Ok(window), Ok((mut target, mut camera))) = (windows.single(), cameras.single_mut())
    else {
        return;
    };
    let (ww, wh) = (window.width(), window.height());
    let doc = !grid.on;
    let left = if doc { LEFT_W.min(ww * 0.4) } else { 0.0 };
    let right = if doc && wb.right_open() {
        RIGHT_W.min(ww * 0.4)
    } else {
        0.0
    };
    let dock = if doc && wb.mode == Mode::Animate {
        DOCK_H.min((wh - TOP_H) * 0.5)
    } else {
        0.0
    };
    let body = TOP_H.min(wh);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| {
        Rect::from_corners(Vec2::new(x0, y0), Vec2::new(x1.max(x0), y1.max(y0)))
    };
    let rects = SlotRects {
        top: r(0.0, 0.0, ww, body),
        left: r(0.0, body, left, wh),
        right: r(ww - right, body, ww, wh),
        dock: r(left, wh - dock, ww - right, wh),
        centre: r(left, body, ww - right, wh - dock),
    };
    wb.rects = rects;
    for (which, mut n, mut vis) in &mut slots {
        let rect = match which {
            PaneSlot::TopBar => rects.top,
            PaneSlot::Left => rects.left,
            PaneSlot::Right => rects.right,
            PaneSlot::Dock => rects.dock,
            PaneSlot::Centre => rects.centre,
        };
        place(&mut n, rect);
        let want = if rect.width() < 1.0 || rect.height() < 1.0 {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        vis.set_if_neq(want);
    }

    // The 3D view fills the centre, except in Material (the graph) and the grid.
    let show_3d = doc && wb.mode != Mode::Material;
    if camera.is_active != show_3d && !grid.on {
        camera.is_active = show_3d;
    }
    let cursor = window.cursor_position();
    let over_blocker = cursor.is_some_and(|c| {
        let c = c * window.scale_factor();
        blockers.iter().any(|(n, t, v)| {
            v.get() && {
                let half = n.size() * 0.5;
                let local = c - t.translation;
                local.x.abs() <= half.x && local.y.abs() <= half.y
            }
        })
    });
    wb.ui_hover = !show_3d || over_blocker || cursor.is_none_or(|c| !rects.centre.contains(c));

    let sf = window.scale_factor();
    view.rect = rects.centre;
    if let Ok((mut n, mut vis, mut img)) = node.single_mut() {
        place(&mut n, rects.centre);
        if !grid.on {
            vis.set_if_neq(if show_3d {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            });
        }
        let size = UVec2::new(
            ((rects.centre.width() * sf).round() as u32).max(1),
            ((rects.centre.height() * sf).round() as u32).max(1),
        );
        if view.size != size {
            // A fresh target at the new size (as the agent rig does): the
            // camera and the UI node switch to it together.
            view.size = size;
            let fresh = images.add(view_target(size.x, size.y));
            view.image = fresh.clone();
            *target = RenderTarget::Image(fresh.clone().into());
            img.image = fresh;
        }
    }
}

fn place(n: &mut Node, rect: Rect) {
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
