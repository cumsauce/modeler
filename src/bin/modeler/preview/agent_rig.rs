use crate::prelude::*;

/// The agent's offscreen camera: renders to a texture so screenshots don't
/// touch the human's window or steal their orbit camera. Spawned by the
/// windowed startup; absent in headless mode.
#[derive(Resource, Default)]
pub(crate) struct AgentRig {
    pub(crate) camera: Option<Entity>,
    pub(crate) image: Option<Handle<Image>>,
}

/// Marks the offscreen agent camera entity (the main window camera is
/// [`MainCamera`]).
#[derive(Component)]
pub(crate) struct AgentCamera;

/// One offscreen shot waiting for a frame: view, pixel size, reply.
#[derive(Resource, Default)]
pub(crate) struct PendingAgentShots(
    pub(crate) Vec<(AgentView, u32, u32, Option<Vec<String>>, ShotReply)>,
);

/// Named agent camera views. The six cardinal views are orthographic (true
/// proportions, auto-framed to the model bounds); `iso` is a perspective
/// three-quarter view.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AgentView {
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
    Iso,
    Custom(CustomCam),
}

/// An agent-placed camera. yaw 0 = on +z looking toward -z (the front
/// view), 90 = on +x (the right view); pitch up from the horizon.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CustomCam {
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    /// Distance from the target; None auto-fits the framed bounds.
    pub(crate) dist: Option<f32>,
    /// Look-at point; None = centre of the framed bounds.
    pub(crate) target: Option<[f32; 3]>,
    pub(crate) ortho: bool,
    /// Vertical field of view in degrees (perspective only).
    pub(crate) fov: f32,
}

impl std::fmt::Display for AgentView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentView::Custom(c) => write!(
                f,
                "{} yaw {:.0} pitch {:.0}",
                if c.ortho { "ortho" } else { "cam" },
                c.yaw,
                c.pitch
            ),
            other => write!(f, "{}", format!("{other:?}").to_lowercase()),
        }
    }
}

/// Unit vector from the target toward the eye for a yaw/pitch in degrees
/// (yaw 0 = +z, 90 = +x). Shared with `set_camera`.
pub(crate) fn yaw_pitch_dir(yaw: f32, pitch: f32) -> Vec3 {
    let (y, p) = (yaw.to_radians(), pitch.clamp(-89.0, 89.0).to_radians());
    Vec3::new(y.sin() * p.cos(), p.sin(), y.cos() * p.cos())
}

pub(crate) fn parse_agent_view(s: &str) -> Result<AgentView, String> {
    match s.to_ascii_lowercase().as_str() {
        "front" => Ok(AgentView::Front),
        "back" => Ok(AgentView::Back),
        "left" => Ok(AgentView::Left),
        "right" => Ok(AgentView::Right),
        "top" => Ok(AgentView::Top),
        "bottom" => Ok(AgentView::Bottom),
        "iso" => Ok(AgentView::Iso),
        other => Err(format!(
            "unknown view `{other}` (front, back, left, right, top, bottom, iso)"
        )),
    }
}

/// Spawns the agent's offscreen camera rig: a render-to-texture target plus a
/// camera the tools reposition per shot. Independent of the human's orbit
/// camera, so agent screenshots never disturb the window view. The camera
/// carries its own directional light so every view is lit regardless of the
/// scene's light orientation.
pub(crate) fn setup_agent_camera(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = images.add(Image::new_target_texture(
        1280,
        720,
        TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    // Camera3d requires RenderTarget, so it cannot also appear in the spawn
    // bundle; insert the image target after the spawn instead. ClearColor
    // follows the world default, matching the human window's background.
    let camera = commands
        .spawn((
            Name::new("agent camera"),
            AgentCamera,
            Camera3d::default(),
            Projection::from(PerspectiveProjection::default()),
            AmbientLight {
                brightness: 300.0,
                ..default()
            },
            Transform::from_translation(Vec3::new(4.0, 3.0, 6.0)).looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .with_children(|c| {
            // Key + fill: dimmer than the scene's sun so low-roughness parts
            // don't blow out to white in agent views (a hot highlight reads
            // as solid geometry to an agent that cannot orbit to check).
            // The key comes from above-left of the view, with shadows, so
            // depth reads even in flat orthographic views: a door or window
            // opening shows the shadowed interior instead of a back wall lit
            // exactly like the facade.
            c.spawn((
                DirectionalLight {
                    illuminance: 350.0,
                    shadow_maps_enabled: true,
                    ..default()
                },
                Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.45, -0.55, 0.0)),
            ));
            c.spawn((
                DirectionalLight {
                    illuminance: 150.0,
                    ..default()
                },
                Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
            ));
        })
        .insert(RenderTarget::Image(image.clone().into()))
        .id();
    commands.insert_resource(AgentRig {
        camera: Some(camera),
        image: Some(image),
    });
}

/// Computes the camera placement for one agent view, auto-framed to the
/// model's world bounds. Returns (eye, target, up, projection).
pub(crate) fn agent_view_projection(
    view: AgentView,
    bounds: Option<(Vec3, Vec3)>,
    aspect: f32,
) -> (Vec3, Vec3, Vec3, Projection) {
    let (min, max) = bounds.unwrap_or((Vec3::splat(-1.0), Vec3::splat(1.0)));
    let center = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let radius = half.length().max(1e-3);
    let margin = 1.2;
    match view {
        AgentView::Custom(c) => {
            let target = c.target.map_or(center, Vec3::from);
            // Fit the bounds' sphere around the (possibly off-centre) target.
            let reach = radius + (center - target).length();
            let dir = yaw_pitch_dir(c.yaw, c.pitch);
            // Straight up/down: keep a valid up vector.
            let up = if dir.y.abs() > 0.99 {
                -Vec3::Z * dir.y.signum()
            } else {
                Vec3::Y
            };
            if c.ortho {
                let mut hh = reach * margin;
                let mut hw = hh;
                if hw / hh < aspect {
                    hw = hh * aspect;
                } else {
                    hh = hw / aspect;
                }
                let dist = c.dist.unwrap_or(reach * 3.0 + 1.0);
                (
                    target + dir * dist,
                    target,
                    up,
                    Projection::from(OrthographicProjection {
                        near: 0.0,
                        far: dist * 2.0 + reach * 4.0 + 1.0,
                        scaling_mode: ScalingMode::Fixed {
                            width: hw * 2.0,
                            height: hh * 2.0,
                        },
                        ..OrthographicProjection::default_3d()
                    }),
                )
            } else {
                let fov = c.fov.clamp(5.0, 150.0).to_radians();
                // The tighter half-angle is vertical unless the frame is tall.
                let half = if aspect < 1.0 {
                    ((fov * 0.5).tan() * aspect).atan()
                } else {
                    fov * 0.5
                };
                let dist = c.dist.unwrap_or(reach * margin / half.sin()).max(1e-3);
                (
                    target + dir * dist,
                    target,
                    up,
                    Projection::from(PerspectiveProjection {
                        fov,
                        aspect_ratio: aspect,
                        near: (dist * 0.01).max(1e-4),
                        far: (dist + reach * 4.0) * 2.0,
                        ..default()
                    }),
                )
            }
        }
        AgentView::Iso => {
            // The bounding sphere fits the vertical half-fov (22.5°, the
            // tighter axis) at radius / sin(22.5°) ≈ 2.61 r; 3.0 r adds margin
            // so tall models' tips aren't cropped.
            let dist = (radius * 3.0).max(0.05);
            let dir = Vec3::new(1.0, 0.75, 1.0).normalize();
            (
                center + dir * dist,
                center,
                Vec3::Y,
                Projection::from(PerspectiveProjection {
                    fov: 45.0_f32.to_radians(),
                    aspect_ratio: aspect,
                    // Scale-aware so a 2 cm part still fills the frame.
                    near: (dist * 0.01).max(1e-4),
                    far: (dist * 4.0 + radius * 10.0).max(1.0),
                    ..default()
                }),
            )
        }
        view => {
            let (offset, up, (mut hw, mut hh)): (Vec3, Vec3, (f32, f32)) = match view {
                AgentView::Front => (Vec3::Z, Vec3::Y, (half.x, half.y)),
                AgentView::Back => (-Vec3::Z, Vec3::Y, (half.x, half.y)),
                AgentView::Right => (Vec3::X, Vec3::Y, (half.z, half.y)),
                AgentView::Left => (-Vec3::X, Vec3::Y, (half.z, half.y)),
                // Plan view: +x right, -z (north) up.
                AgentView::Top => (Vec3::Y, -Vec3::Z, (half.x, half.z)),
                AgentView::Bottom => (-Vec3::Y, Vec3::Z, (half.x, half.z)),
                AgentView::Iso | AgentView::Custom(_) => unreachable!(),
            };
            hw = hw.max(1e-3) * margin;
            hh = hh.max(1e-3) * margin;
            // Fit the world span into the requested pixel aspect.
            if hw / hh < aspect {
                hw = hh * aspect;
            } else {
                hh = hw / aspect;
            }
            let dist = radius * 3.0 + 5.0;
            (
                center + offset * dist,
                center,
                up,
                Projection::from(OrthographicProjection {
                    near: 0.0,
                    far: dist * 2.0 + radius * 10.0 + 10.0,
                    // Fixed, not WindowSize: the target is an offscreen image,
                    // so the world span must come from our framing, not the
                    // human's window dimensions.
                    scaling_mode: ScalingMode::Fixed {
                        width: hw * 2.0,
                        height: hh * 2.0,
                    },
                    ..OrthographicProjection::default_3d()
                }),
            )
        }
    }
}

/// Takes one pending agent shot per frame: repositions the offscreen camera,
/// resizes the target image, then requests the capture. One per frame so each
/// capture is guaranteed to show its own view.
pub(crate) fn process_agent_shot(
    mut agent_shots: ResMut<PendingAgentShots>,
    mut rig: ResMut<AgentRig>,
    mut images: ResMut<Assets<Image>>,
    mut cameras: Query<(&mut Projection, &mut RenderTarget, &mut Transform), With<AgentCamera>>,
    modeler: Res<Modeler>,
    mut commands: Commands,
) {
    let Some((view, width, height, focus, reply)) = agent_shots.0.pop() else {
        return;
    };
    let (Some(camera_entity), Some(image_handle)) = (rig.camera, rig.image.clone()) else {
        let _ = reply.send(Err("agent camera is not initialized".into()));
        return;
    };
    let mut image_handle = image_handle;
    let aspect = width as f32 / height.max(1) as f32;
    let bounds = match &focus {
        Some(names) => match modeler.session.model.bounds_of(names) {
            Some(b) => Some(b),
            None => {
                let _ = reply.send(Err(format!("focus: none of {names:?} exist as parts")));
                return;
            }
        },
        None => modeler.session.model.bounds(),
    };
    let (eye, target, up, new_projection) = agent_view_projection(view, bounds, aspect);
    let Ok((mut projection, mut render_target, mut transform)) = cameras.get_mut(camera_entity)
    else {
        let _ = reply.send(Err("agent camera missing".into()));
        return;
    };
    *projection = new_projection;
    *transform = Transform::from_translation(eye).looking_at(target, up);
    let (cur_w, cur_h) = images
        .get(&image_handle)
        .map(|i| (i.width(), i.height()))
        .unwrap_or((0, 0));
    if cur_w != width || cur_h != height {
        // Recreate the target instead of resizing: the camera's render target
        // handle is updated below, so this is invisible to the GPU graph.
        let fresh = images.add(Image::new_target_texture(
            width,
            height,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
        rig.image = Some(fresh.clone());
        image_handle = fresh;
    }
    *render_target = RenderTarget::Image(image_handle.clone().into());
    let slot = Mutex::new(Some(reply));
    commands
        .spawn(Screenshot::image(image_handle))
        .observe(move |captured: On<ScreenshotCaptured>| {
            if let Ok(mut s) = slot.lock()
                && let Some(reply) = s.take()
            {
                let _ = reply.send(encode_png(&captured.image));
            }
        })
        .insert(Name::new("mcp agent shot"));
}
