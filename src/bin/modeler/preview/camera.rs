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
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            yaw: 0.7,
            pitch: 0.45,
            dist: 6.0,
            target: Vec3::ZERO,
            user: false,
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

/// Mouse and keyboard camera control: left-drag orbits, middle/right-drag pans,
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
) {
    if scroll.delta.y != 0.0 {
        orbit.dist = (orbit.dist * (1.0 - 0.12 * scroll.delta.y)).clamp(0.2, 200.0);
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
        .find(|b| mouse.pressed(*b));
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
                        orbit.yaw -= d.x * 0.01;
                        orbit.pitch = (orbit.pitch + d.y * 0.01).clamp(-1.4, 1.4);
                        orbit.user = true;
                    }
                    // Middle/right-drag pans the target in the camera's screen plane.
                    _ => {
                        let (sin_y, cos_y) = orbit.yaw.sin_cos();
                        let (sin_p, cos_p) = orbit.pitch.sin_cos();
                        let right = Vec3::new(sin_y, 0.0, -cos_y);
                        let up = right
                            .cross(-Vec3::new(cos_y * cos_p, sin_p, sin_y * cos_p))
                            .normalize();
                        let s = orbit.dist * 0.0016;
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
    let (sin_y, cos_y) = orbit.yaw.sin_cos();
    let (sin_p, cos_p) = orbit.pitch.sin_cos();
    let eye = orbit.target + Vec3::new(cos_y * cos_p, sin_p, sin_y * cos_p) * orbit.dist;
    let Ok(mut cam) = camera.single_mut() else {
        return;
    };
    *cam = Transform::from_translation(eye).looking_at(orbit.target, Vec3::Y);
}
