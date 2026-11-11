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
    pub(crate)  Vec<(
        AgentView,
        u32,
        u32,
        Option<Vec<String>>,
        ShotStyle,
        ShotReply,
    )>,
);

/// How an agent shot is drawn: `ids` swaps every part for a flat, unlit,
/// distinct colour (index = position among visible parts, legend drawn in);
/// `dims` overlays the framed bounds' dimensions and a scale bar.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ShotStyle {
    pub(crate) ids: bool,
    pub(crate) dims: bool,
    /// Light with the model's own lights only (studio lights hidden).
    pub(crate) scene_lit: bool,
}

/// The studio's own lights (sun, fills, agent key), hidden for a
/// `lighting: "scene"` shot.
#[derive(Component)]
pub(crate) struct StudioLight;

/// Flat-colour stand-ins for an `ids` shot; only the agent camera sees their
/// render layer, so the human's window never flickers.
const ID_LAYER: usize = 7;

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
            studio_env(&mut images, STUDIO_INTENSITY * 0.6),
            contact_shading(),
            // No screen-space reflections, unlike the window: SSR's ray march
            // is noisy per frame, and a single-frame shot (no TAA history to
            // average it) kept that noise as speckle on glossy and metal
            // parts, plus smeared reflections that read as geometry (dark
            // columns under typewriter keys). The studio environment still
            // gives glossy parts their reflections. Deferred like the window
            // camera (SSR used to imply this), so materials shade the same.
            bevy::core_pipeline::prepass::DeferredPrepass,
            AmbientLight {
                brightness: 40.0,
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
                StudioLight,
                DirectionalLight {
                    illuminance: 350.0,
                    shadow_maps_enabled: true,
                    ..default()
                },
                Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.45, -0.55, 0.0)),
            ));
            c.spawn((
                StudioLight,
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

/// Frames to wait after the environment light appears before shooting.
const ENV_WARM_FRAMES: u32 = 4;

/// Takes pending agent shots one at a time: repositions the offscreen
/// camera, resizes the target image, then requests the capture. The next
/// shot waits until that capture has landed and its observer has restored
/// the rig (studio lights, environment, layers), so a view is never aimed
/// while the previous one's `lighting: "scene"` or `ids` setup is live, and
/// a scene-lit shot never records already-dimmed lights as the ones to
/// restore.
pub(crate) fn process_agent_shot(
    mut agent_shots: ResMut<PendingAgentShots>,
    mut rig: ResMut<AgentRig>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut pbr_cache: ResMut<PbrCache>,
    mut cameras: Query<(&mut Projection, &mut RenderTarget, &mut Transform), With<AgentCamera>>,
    modeler: Res<Modeler>,
    env_ready: Query<(), (With<AgentCamera>, With<bevy::light::EnvironmentMapLight>)>,
    mut warm: Local<(u32, u32)>,
    capturing: Local<Arc<AtomicBool>>,
    mut studio: Query<(Entity, &mut DirectionalLight), With<StudioLight>>,
    mut env: Query<(&mut bevy::light::EnvironmentMapLight, &mut AmbientLight), With<AgentCamera>>,
    mut commands: Commands,
) {
    // A camera with a generated environment light draws nothing until its
    // maps are filtered (the component appears, then the GPU filters over
    // the next frames), so hold shots until then. If filtering never comes
    // (no compute support) shoot anyway after ~2 s rather than time out.
    warm.1 = warm.1.saturating_add(1);
    if env_ready.is_empty() {
        warm.0 = 0;
    } else {
        warm.0 = warm.0.saturating_add(1);
    }
    if warm.0 < ENV_WARM_FRAMES && warm.1 < 120 {
        return;
    }
    if capturing.load(Ordering::Acquire) {
        return;
    }
    let Some((view, width, height, focus, style, reply)) = agent_shots.0.pop() else {
        return;
    };
    let (Some(camera_entity), Some(image_handle)) = (rig.camera, rig.image.clone()) else {
        let _ = reply.send(Err("agent camera is not initialized".into()));
        return;
    };
    let mut image_handle = image_handle;
    let aspect = width as f32 / height.max(1) as f32;
    let model = &modeler.session.model;
    let bounds = match &focus {
        Some(names) => match model.bounds_of(names) {
            Some(b) => Some(b),
            None => {
                let _ = reply.send(Err(format!("focus: none of {names:?} exist as parts")));
                return;
            }
        },
        None => model.bounds(),
    };
    let (eye, target, up, new_projection) = agent_view_projection(view, bounds, aspect);
    // Pixels per metre for orthographic shots (dimension overlays).
    let ppm = match &new_projection {
        Projection::Orthographic(o) => match o.scaling_mode {
            ScalingMode::Fixed { width: ww, .. } => Some(width as f32 / ww),
            _ => None,
        },
        _ => None,
    };
    // Perspective: pixels per metre on the plane through the look-at point.
    let ppm_at_target = match &new_projection {
        Projection::Perspective(p) => {
            let d = (eye - target).length();
            Some(height as f32 / (2.0 * d * (p.fov * 0.5).tan()))
        }
        _ => None,
    };
    let Ok((mut projection, mut render_target, mut transform)) = cameras.get_mut(camera_entity)
    else {
        let _ = reply.send(Err("agent camera missing".into()));
        return;
    };
    *projection = new_projection;
    *transform = Transform::from_translation(eye).looking_at(target, up);
    let cam = *transform;
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
    // Part-ID stand-ins on their own layer; the camera looks only at it.
    let mut legend: Vec<(String, [u8; 4])> = Vec::new();
    let mut stand_ins: Vec<Entity> = Vec::new();
    if style.ids {
        let drawable = model
            .parts
            .iter()
            .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty());
        for (i, part) in drawable.enumerate() {
            legend.push((part.name.clone(), modeler::raster::id_color_u8(i)));
            let bundle = part_bundle(
                part,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut pbr_cache,
            );
            let e = commands.spawn((bundle, RenderLayers::layer(ID_LAYER))).id();
            stand_ins.push(e);
        }
        // Unlit: swap the materials the bundle made for flat ones.
        for (k, e) in stand_ins.iter().enumerate() {
            let [r, g, b] = modeler::raster::id_color(k);
            let m = materials.add(StandardMaterial {
                base_color: Color::srgb(r, g, b),
                unlit: true,
                ..default()
            });
            commands.entity(*e).insert(MeshMaterial3d(m));
        }
        commands
            .entity(camera_entity)
            .insert(RenderLayers::layer(ID_LAYER));
    }
    // Scene lighting: hide the studio, keep a faint environment so unlit
    // corners read as dark rather than void. Restored after the capture.
    let mut studio_lights: Vec<(Entity, f32)> = Vec::new();
    let mut restore_env = None;
    if style.scene_lit {
        for (e, mut l) in &mut studio {
            studio_lights.push((e, l.illuminance));
            l.illuminance = 0.0;
        }
        if let Ok((mut g, mut a)) = env.get_mut(camera_entity) {
            restore_env = Some((g.intensity, a.brightness));
            g.intensity *= 0.01;
            a.brightness = 0.5;
        }
    }
    // World box of what is framed, projected to pixels (ortho only).
    let overlay = style.dims.then(|| {
        let (lo, hi) = bounds.unwrap_or((Vec3::ZERO, Vec3::ZERO));
        DimsOverlay {
            ppm,
            ppm_at_target,
            right: cam.right().as_vec3(),
            up: cam.up().as_vec3(),
            target,
            lo,
            hi,
        }
    });
    let slot = Mutex::new(Some(reply));
    let ids = style.ids;
    let capturing = Arc::clone(&capturing);
    capturing.store(true, Ordering::Release);
    commands
        .spawn(Screenshot::image(image_handle))
        .observe(
            move |captured: On<ScreenshotCaptured>,
                  mut commands: Commands,
                  mut env: Query<
                (&mut bevy::light::EnvironmentMapLight, &mut AmbientLight),
                With<AgentCamera>,
            >,
                  mut lights: Query<&mut DirectionalLight, With<StudioLight>>| {
                for (e, lux) in &studio_lights {
                    if let Ok(mut l) = lights.get_mut(*e) {
                        l.illuminance = *lux;
                    }
                }
                if let Some((gi, ab)) = restore_env
                    && let Ok((mut g, mut a)) = env.get_mut(camera_entity)
                {
                    g.intensity = gi;
                    a.brightness = ab;
                }
                for e in &stand_ins {
                    commands.entity(*e).despawn();
                }
                if ids {
                    commands
                        .entity(camera_entity)
                        .insert(RenderLayers::layer(0));
                }
                capturing.store(false, Ordering::Release);
                if let Ok(mut s) = slot.lock()
                    && let Some(reply) = s.take()
                {
                    let out = image_to_canvas(&captured.image).and_then(|mut c| {
                        if let Some(o) = &overlay {
                            o.draw(&mut c);
                        }
                        if !legend.is_empty() {
                            draw_legend(&mut c, &legend);
                        }
                        c.to_png()
                    });
                    let _ = reply.send(out);
                }
            },
        )
        .insert(Name::new("mcp agent shot"));
}

/// What a `dims` overlay needs: the camera basis, framing scale and the
/// framed world box.
struct DimsOverlay {
    ppm: Option<f32>,
    ppm_at_target: Option<f32>,
    right: Vec3,
    up: Vec3,
    target: Vec3,
    lo: Vec3,
    hi: Vec3,
}

impl DimsOverlay {
    fn draw(&self, c: &mut Canvas) {
        let size = self.hi - self.lo;
        let Some(ppm) = self.ppm else {
            // Perspective has no single scale: say the size is the framed
            // model's (not what is in view), and give a scale bar valid only
            // at the look-at point's depth.
            let t = format!(
                "MODEL {:.2} X {:.2} X {:.2} M (NOT THE VIEW)",
                size.x, size.y, size.z
            );
            c.text(6, c.h.saturating_sub(24), &t, 2);
            if let Some(k) = self.ppm_at_target {
                c.text(
                    c.w.saturating_sub(190),
                    c.h.saturating_sub(88),
                    "AT TARGET DEPTH",
                    2,
                );
                modeler::inspect::scale_bar(c, k as f64, c.w - 10, c.h - 34);
            }
            return;
        };
        let (w, h) = (c.w as f32, c.h as f32);
        let to_px = |p: Vec3| {
            let d = p - self.target;
            (
                w * 0.5 + d.dot(self.right) * ppm,
                h * 0.5 - d.dot(self.up) * ppm,
            )
        };
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for k in 0..8 {
            let p = Vec3::new(
                if k & 1 == 0 { self.lo.x } else { self.hi.x },
                if k & 2 == 0 { self.lo.y } else { self.hi.y },
                if k & 4 == 0 { self.lo.z } else { self.hi.z },
            );
            let (x, y) = to_px(p);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let col = [255, 220, 60, 255];
        let (x0, y0, x1, y1) = (x0 as f64, y0 as f64, x1 as f64, y1 as f64);
        // Horizontal dimension under the box, vertical one to its left.
        let yb = (y1 + 14.0).min(h as f64 - 4.0);
        let xl = (x0 - 14.0).max(4.0);
        c.line((x0, yb), (x1, yb), col, 2);
        c.line((x0, yb - 6.0), (x0, yb + 6.0), col, 2);
        c.line((x1, yb - 6.0), (x1, yb + 6.0), col, 2);
        c.line((xl, y0), (xl, y1), col, 2);
        c.line((xl - 6.0, y0), (xl + 6.0, y0), col, 2);
        c.line((xl - 6.0, y1), (xl + 6.0, y1), col, 2);
        let wlab = modeler::raster::fmt_len(((x1 - x0) / ppm as f64).abs());
        let hlab = modeler::raster::fmt_len(((y1 - y0) / ppm as f64).abs());
        let tw = Canvas::text_width(&wlab, 2) as f64;
        c.text(
            ((x0 + x1) * 0.5 - tw * 0.5).max(0.0) as usize,
            (yb + 6.0).min(h as f64 - 20.0).max(0.0) as usize,
            &wlab,
            2,
        );
        c.text(
            (xl + 4.0).max(0.0) as usize,
            ((y0 + y1) * 0.5 - 9.0).max(0.0) as usize,
            &hlab,
            2,
        );
        modeler::inspect::scale_bar(c, ppm as f64, c.w - 10, c.h - 10);
    }
}

fn draw_legend(c: &mut Canvas, legend: &[(String, [u8; 4])]) {
    let scale = if c.h >= 480 { 2 } else { 1 };
    let row = 10 * scale + 2;
    let max_rows = (c.h / row).saturating_sub(3).max(1);
    // Below the grid's cell label (up to 22 px tall).
    let mut y = 30;
    for (name, col) in legend.iter().take(max_rows) {
        for dy in 0..(8 * scale) as i64 {
            for dx in 0..(8 * scale) as i64 {
                c.set(4 + dx, y as i64 + dy, *col);
            }
        }
        c.text(6 + 8 * scale, y, name, scale);
        y += row;
    }
    if legend.len() > max_rows {
        c.text(4, y, &format!("+{} MORE", legend.len() - max_rows), scale);
    }
}
