use crate::prelude::*;

/// The orbit camera: angles in a resource, mouse drag / arrow keys to steer.
/// `user` is set once the human takes over, which stops the slow auto-drift.
#[derive(Resource)]
pub(crate) struct Orbit {
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) dist: f32,
    pub(crate) target: Vec3,
    pub(crate) user: bool,
    /// Half the model's bounding diagonal: a floor for pan speed, so panning
    /// stays useful after zooming in close on a large scene.
    pub(crate) scene_radius: f32,
}

/// Wheel / trackpad scroll in "lines": macOS trackpads report pixels (dozens
/// per gesture frame), mouse wheels report lines.
pub(crate) fn scroll_lines(scroll: &AccumulatedMouseScroll) -> f32 {
    match scroll.unit {
        bevy::input::mouse::MouseScrollUnit::Line => scroll.delta.y,
        bevy::input::mouse::MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
    }
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            yaw: 0.7,
            pitch: 0.45,
            dist: 6.0,
            target: Vec3::ZERO,
            user: false,
            scene_radius: 1.0,
        }
    }
}

/// State of the current mouse drag (orbit with left, pan with middle/right).
#[derive(Resource, Default)]
pub(crate) struct CameraDrag {
    pub(crate) button: Option<MouseButton>,
    pub(crate) last: Vec2,
    pub(crate) moved: f32,
}

#[derive(Component)]
pub(crate) struct MainCamera;

/// Mouse and keyboard camera control: left-drag orbits, middle-drag pans,
/// right-drag looks around and flies with WASD / Q E (Shift faster),
/// scroll wheel zooms, arrows/PageUp/PageDown still work. A slow auto-drift runs
/// until the user first moves the camera themselves.
pub(crate) fn orbit_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut orbit: ResMut<Orbit>,
    mut drag: ResMut<CameraDrag>,
    mut camera: Query<&mut Transform, With<MainCamera>>,
    wb: Res<Workbench>,
) {
    let scroll_y = if wb.ui_hover {
        0.0
    } else {
        scroll_lines(&scroll)
    };
    if scroll_y != 0.0 {
        orbit.dist = (orbit.dist * (-0.15 * scroll_y).exp()).clamp(0.2, 200.0);
        orbit.user = true;
    }
    if keys.pressed(KeyCode::PageUp) {
        orbit.dist = (orbit.dist * 0.95).max(0.2);
        orbit.user = true;
    }
    if keys.pressed(KeyCode::PageDown) {
        orbit.dist = (orbit.dist * 1.05).min(200.0);
        orbit.user = true;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let dragging = [MouseButton::Left, MouseButton::Middle, MouseButton::Right]
        .into_iter()
        .find(|b| mouse.pressed(*b))
        // A drag that starts over the node panel belongs to the panel.
        .filter(|_| drag.button.is_some() || !wb.ui_hover);
    if let Some(button) = dragging {
        if let Some(cursor) = window.cursor_position() {
            if drag.button != Some(button) {
                drag.button = Some(button);
                drag.last = cursor;
                drag.moved = 0.0;
            } else {
                let d = cursor - drag.last;
                drag.moved += d.length();
                drag.last = cursor;
                match button {
                    // Left-drag orbits around the target; click-without-drag pins.
                    MouseButton::Left => {
                        orbit.yaw += d.x * 0.01;
                        orbit.pitch = (orbit.pitch + d.y * 0.01).clamp(-1.4, 1.4);
                        orbit.user = true;
                    }
                    // Right-drag looks around from where the camera is (fly
                    // mode, with WASD below): the eye stays, the target turns.
                    MouseButton::Right => {
                        let eye = orbit_eye(&orbit);
                        orbit.yaw += d.x * 0.004;
                        orbit.pitch = (orbit.pitch + d.y * 0.004).clamp(-1.5, 1.5);
                        orbit.target = eye - orbit_dir(&orbit) * orbit.dist;
                        orbit.user = true;
                    }
                    // Middle-drag pans the target in the camera's screen plane.
                    _ => {
                        let (sin_y, cos_y) = orbit.yaw.sin_cos();
                        let (sin_p, cos_p) = orbit.pitch.sin_cos();
                        let right = Vec3::new(sin_y, 0.0, -cos_y);
                        let up = right
                            .cross(-Vec3::new(cos_y * cos_p, sin_p, sin_y * cos_p))
                            .normalize();
                        // Grab-style: the target moves with the cursor at the target's
                        // depth (45° fov), never slower than a fraction of the scene.
                        let depth = orbit.dist.max(orbit.scene_radius * 0.4);
                        let s = depth * 0.828 / window.height().max(1.0);
                        orbit.target -= right * (d.x * s);
                        orbit.target += up * (d.y * s);
                        orbit.user = true;
                    }
                }
            }
        }
    } else {
        drag.button = None;
    }

    // Gentle auto-orbit only until the user steers the camera themselves.
    if !orbit.user && drag.button.is_none() {
        orbit.yaw += time.delta_secs() * 0.25;
    }
    let spin = 1.5 * time.delta_secs();
    if keys.pressed(KeyCode::ArrowLeft) {
        orbit.yaw -= spin;
        orbit.user = true;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        orbit.yaw += spin;
        orbit.user = true;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        orbit.pitch = (orbit.pitch + spin).min(1.4);
        orbit.user = true;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        orbit.pitch = (orbit.pitch - spin).max(-1.4);
        orbit.user = true;
    }
    // Fly: hold the right button and WASD moves along the view, Q/E down /
    // up, Shift 4x. Speed scales with the scene (a 200 m map flies faster
    // than a mug).
    if mouse.pressed(MouseButton::Right) && drag.button == Some(MouseButton::Right) {
        let f = -orbit_dir(&orbit);
        let right = Vec3::new(orbit.yaw.sin(), 0.0, -orbit.yaw.cos());
        let mut v = Vec3::ZERO;
        for (k, d) in [
            (KeyCode::KeyW, f),
            (KeyCode::KeyS, -f),
            (KeyCode::KeyD, right),
            (KeyCode::KeyA, -right),
            (KeyCode::KeyE, Vec3::Y),
            (KeyCode::KeyQ, -Vec3::Y),
        ] {
            if keys.pressed(k) {
                v += d;
            }
        }
        if v != Vec3::ZERO {
            let fast = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                4.0
            } else {
                1.0
            };
            let speed = (orbit.scene_radius * 0.25).clamp(1.0, 40.0) * fast;
            orbit.target += v.normalize() * speed * time.delta_secs();
            orbit.user = true;
        }
    }
    let eye = orbit_eye(&orbit);
    let Ok(mut cam) = camera.single_mut() else {
        return;
    };
    *cam = Transform::from_translation(eye).looking_at(orbit.target, Vec3::Y);
}

/// From the target toward the eye (unit).
pub(crate) fn orbit_dir(o: &Orbit) -> Vec3 {
    let (sin_y, cos_y) = o.yaw.sin_cos();
    let (sin_p, cos_p) = o.pitch.sin_cos();
    Vec3::new(cos_y * cos_p, sin_p, sin_y * cos_p)
}

pub(crate) fn orbit_eye(o: &Orbit) -> Vec3 {
    o.target + orbit_dir(o) * o.dist
}

/// True while the right button flies the camera: single-key shortcuts
/// (S for the style pane, ...) stand aside so WASD only moves.
pub(crate) fn flying(mouse: &ButtonInput<MouseButton>) -> bool {
    mouse.pressed(MouseButton::Right)
}
