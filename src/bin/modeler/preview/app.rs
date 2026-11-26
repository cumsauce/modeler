use crate::prelude::*;

#[derive(Resource)]
pub(crate) struct Modeler {
    pub(crate) session: Session,
    pub(crate) bridge: Arc<Bridge>,
}

#[derive(Resource)]
pub(crate) struct Headless(pub(crate) bool);

/// Rendering without a window (`--offscreen`): agent views work, the human
/// view (screenshot, set_camera) does not.
#[derive(Resource)]
pub(crate) struct Offscreen(pub(crate) bool);

pub(crate) fn windowed(headless: Option<Res<Headless>>) -> bool {
    !headless.map(|h| h.0).unwrap_or(false)
}

#[derive(Component)]
pub(crate) struct PreviewPart;

/// A light from the model's `light` items (respawned with the parts).
#[derive(Component)]
pub(crate) struct SceneLight;

/// Spawns the model's lights in Bevy units (lumens / lux), as a game would.
fn spawn_lights(commands: &mut Commands, model: &modeler::Model) {
    let lights = match modeler::light::collect(model) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("lights: {e}");
            return;
        }
    };
    for l in lights {
        let color = Color::linear_rgb(l.color[0], l.color[1], l.color[2]);
        let tf = Transform::from_translation(l.position).with_rotation(l.rotation());
        let name = Name::new(format!("light {}", l.name));
        match l.kind.as_str() {
            "directional" => {
                commands.spawn((
                    name,
                    SceneLight,
                    DirectionalLight {
                        color,
                        illuminance: l.intensity,
                        shadow_maps_enabled: l.shadows,
                        ..default()
                    },
                    tf,
                ));
            }
            "spot" => {
                commands.spawn((
                    name,
                    SceneLight,
                    SpotLight {
                        color,
                        intensity: l.intensity,
                        range: l.range.unwrap_or(20.0),
                        inner_angle: l.inner,
                        outer_angle: l.outer,
                        shadow_maps_enabled: l.shadows,
                        ..default()
                    },
                    tf,
                ));
            }
            _ => {
                commands.spawn((
                    name,
                    SceneLight,
                    PointLight {
                        color,
                        intensity: l.intensity,
                        range: l.range.unwrap_or(20.0),
                        shadow_maps_enabled: l.shadows,
                        ..default()
                    },
                    tf,
                ));
            }
        }
    }
}

/// Which model version the scene was last built from.
#[derive(Resource, Default)]
pub(crate) struct SceneBuilt {
    pub(crate) version: Option<u64>,
    /// The document the window's scene entities show.
    pub(crate) doc: Option<String>,
    /// The model the camera was last framed for.
    pub(crate) framed: Option<String>,
    /// Parts still streaming in (no finished entity yet).
    pub(crate) pending: usize,
    /// Some part shows a distance LOD copy when the camera is far: the
    /// window's entities aren't the full-detail model at every distance.
    pub(crate) lods: bool,
}

/// Exits headless mode once the MCP server has hung up; windowed mode stays open so
/// the human can keep looking at the model.
pub(crate) fn quit_when_server_done(
    headless: bool,
) -> impl FnMut(ResMut<Modeler>, MessageWriter<AppExit>) {
    move |mut modeler, mut exit| {
        if headless && modeler.bridge.quit() {
            modeler
                .bypass_change_detection()
                .session
                .flush_autosave(true);
            exit.write(AppExit::Success);
        }
    }
}

/// Writes pending autosaves once edits pause (every frame; cheap when
/// nothing is due). Bypasses change detection: saving changes nothing shown.
pub(crate) fn autosave_tick(mut modeler: ResMut<Modeler>) {
    modeler
        .bypass_change_detection()
        .session
        .flush_autosave(false);
}

/// On exit (window closed, quit), writes whatever autosave is still pending.
pub(crate) fn autosave_on_exit(mut exits: MessageReader<AppExit>, mut modeler: ResMut<Modeler>) {
    if exits.read().next().is_some() {
        modeler
            .bypass_change_detection()
            .session
            .flush_autosave(true);
    }
}

/// Drains MCP commands every frame and executes them against the session.
pub(crate) fn drain_commands(
    headless: Res<Headless>,
    offscreen: Res<Offscreen>,
    mut modeler: ResMut<Modeler>,
    mut pending: ResMut<PendingShots>,
    mut agent_shots: ResMut<PendingAgentShots>,
    mut orbit: ResMut<Orbit>,
    mut playback: ResMut<Playback>,
    // Window-only: absent headless, where `set_camera ui` says so.
    (mut wb, mut grid): (Option<ResMut<Workbench>>, Option<ResMut<Grid>>),
    mut agent_cam: Local<AgentCam>,
) {
    let _profile = crate::preview::profile::span("drain_commands");
    let bridge = Arc::clone(&modeler.bridge);
    let Ok(mut rx) = bridge.rx.lock() else {
        return;
    };
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Cmd::Exec(f) => {
                // The reply goes back to the MCP tool through the closure's channel.
                let _ = f(&mut modeler.session);
            }
            Cmd::Screenshot(reply) => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no renderer, no screenshot".into()));
                } else if offscreen.0
                    && (grid.as_ref().is_some_and(|g| g.on)
                        || wb.as_ref().is_some_and(|w| w.capture_window))
                {
                    // Offscreen window capture: the UI canvas.
                    pending.0.push(Mutex::new(Some(reply)));
                } else if offscreen.0 || !wb.as_ref().is_some_and(|w| w.capture_window) {
                    // Offscreen: the window camera's view (the orbit that
                    // set_camera and auto-framing steer). Windowed: the
                    // agents' own camera, so a screenshot never moves the
                    // human's view or shows their document instead.
                    let cam = if offscreen.0 {
                        CustomCam {
                            yaw: 90.0 - orbit.yaw.to_degrees(),
                            pitch: orbit.pitch.to_degrees(),
                            dist: Some(orbit.dist),
                            target: Some(orbit.target.to_array()),
                            ortho: false,
                            fov: 45.0,
                        }
                    } else {
                        CustomCam {
                            yaw: agent_cam.yaw,
                            pitch: agent_cam.pitch,
                            dist: agent_cam.dist,
                            target: None,
                            ortho: false,
                            fov: 45.0,
                        }
                    };
                    agent_shots.0.push((
                        AgentView::Custom(cam),
                        1280,
                        800,
                        None,
                        // The window's own pose when it is playing a clip.
                        ShotStyle {
                            pose: playback.clip.clone().map(|c| (Some(c), playback.time)),
                            variant: modeler.session.model.active_variant.clone(),
                            ..default()
                        },
                        reply,
                    ));
                } else {
                    pending.0.push(Mutex::new(Some(reply)));
                }
            }
            Cmd::SetCamera {
                yaw,
                pitch,
                dist,
                play,
                ui,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err(
                        "headless: no camera to move; agent_views takes its own camera".into(),
                    ));
                } else if !offscreen.0 {
                    // A window: agents steer their own camera (what their
                    // screenshots see), never the human's.
                    if let Some(yaw) = yaw {
                        agent_cam.yaw = yaw;
                    }
                    if let Some(pitch) = pitch {
                        agent_cam.pitch = pitch.clamp(-89.0, 89.0);
                    }
                    if let Some(dist) = dist {
                        agent_cam.dist = Some(dist.clamp(0.2, 200.0));
                    }
                    finish_set_camera(
                        play,
                        ui,
                        reply,
                        &modeler.session.model,
                        &mut playback,
                        wb.as_deref_mut(),
                        grid.as_deref_mut(),
                    );
                } else {
                    // Same convention as agent custom cameras: yaw 0 = on +z looking -z.
                    if let Some(yaw) = yaw {
                        orbit.yaw = (90.0 - yaw).to_radians();
                        orbit.user = true;
                    }
                    if let Some(pitch) = pitch {
                        orbit.pitch = pitch.clamp(-89.0, 89.0).to_radians();
                        orbit.user = true;
                    }
                    if let Some(dist) = dist {
                        orbit.dist = dist.clamp(0.2, 200.0);
                        orbit.user = true;
                    }
                    finish_set_camera(
                        play,
                        ui,
                        reply,
                        &modeler.session.model,
                        &mut playback,
                        wb.as_deref_mut(),
                        grid.as_deref_mut(),
                    );
                }
            }
            Cmd::AgentShot {
                view,
                width,
                height,
                focus,
                style,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err(
                        "headless: no renderer, no agent view (run with --offscreen, or the daemon without --headless)".into(),
                    ));
                } else {
                    agent_shots
                        .0
                        .push((view, width, height, focus, style, reply));
                }
            }
        }
    }
}

/// The agents' camera in a window: what `set_camera` yaw / pitch / dist
/// steer and `screenshot` renders, apart from the human's orbit.
pub(crate) struct AgentCam {
    yaw: f32,
    pitch: f32,
    /// None auto-fits the model.
    dist: Option<f32>,
}

impl Default for AgentCam {
    fn default() -> Self {
        AgentCam {
            yaw: 35.0,
            pitch: 25.0,
            dist: None,
        }
    }
}

/// The rest of `set_camera`: playback and the window UI, then the reply.
fn finish_set_camera(
    play: Option<super::playback::PlayRequest>,
    ui: Option<crate::server::UiArg>,
    reply: tokio::sync::oneshot::Sender<Result<Vec<u8>, String>>,
    model: &Model,
    playback: &mut Playback,
    wb: Option<&mut Workbench>,
    grid: Option<&mut Grid>,
) {
    let mut msg = match play {
        Some(r) => playback.request(model, &r),
        None => Ok(String::new()),
    };
    if let (Some(u), Ok(m)) = (ui, &mut msg) {
        let applied = match (wb, grid) {
            (Some(wb), Some(grid)) => apply_ui(&u, model, wb, grid),
            _ => Err("set_camera ui: no window (headless server)".to_string()),
        };
        match applied {
            Ok(said) => {
                if !m.is_empty() {
                    m.push('\n');
                }
                m.push_str(&said);
            }
            Err(e) => msg = Err(e),
        }
    }
    let _ = reply.send(msg.map(String::into_bytes));
}

/// `set_camera(ui: ...)`: the window's UI as the human would set it.
fn apply_ui(
    u: &crate::server::UiArg,
    model: &Model,
    wb: &mut Workbench,
    grid: &mut Grid,
) -> Result<String, String> {
    // Check every field before changing anything: a refused call leaves
    // the window as it was.
    let shading = match u.shading.as_deref() {
        None => None,
        Some("lit") => Some(Shading::Lit),
        Some("clay") => Some(Shading::Clay),
        Some("parts") => Some(Shading::Parts),
        Some(s) => return Err(format!("ui.shading: `{s}` is not lit / clay / parts")),
    };
    let capture = match u.capture.as_deref() {
        None => None,
        Some("window") => Some(true),
        Some("view") => Some(false),
        Some(c) => return Err(format!("ui.capture: `{c}` is not window / view")),
    };
    if let Some(p) = u.select.as_deref().filter(|p| !p.is_empty())
        && !super::workbench::selectable(model, p)
    {
        return Err(format!(
            "ui.select: `{p}` is not a part or marker of `{}`",
            model.name
        ));
    }
    let mode = match u.mode.as_deref() {
        None => None,
        Some(m) if m.eq_ignore_ascii_case("grid") => Some(None),
        Some(m) => {
            let mode = Mode::ALL
                .into_iter()
                .find(|x| x.label().eq_ignore_ascii_case(m))
                .ok_or_else(|| {
                    format!("ui.mode: `{m}` is not model / material / animate / map / grid")
                })?;
            mode_available(model, mode).map_err(|why| format!("ui.mode {m}: {why}"))?;
            Some(Some(mode))
        }
    };
    let mut said = Vec::new();
    if let (Some(mode), Some(m)) = (mode, &u.mode) {
        if mode.is_none() {
            grid.on = true;
        } else if let Some(mode) = mode {
            wb.mode = mode;
            wb.mode_doc = model.name.clone();
            grid.on = false;
        }
        said.push(format!("mode {m}"));
    }
    if let Some(p) = &u.select {
        wb.selected = (!p.is_empty()).then(|| p.clone());
        said.push(if p.is_empty() {
            "deselected".into()
        } else {
            format!("selected {p}")
        });
    }
    if let Some(y) = u.scroll {
        grid.scroll = y.max(0.0);
        said.push(format!("grid scrolled to {y:.0} px"));
    }
    if let Some(p) = u.popup {
        wb.doc_popup = p;
        said.push(format!(
            "document popup {}",
            if p { "open" } else { "closed" }
        ));
    }
    if let Some(c) = &u.comment {
        if let Some(a) = c.action.as_deref()
            && a != "post"
            && a != "cancel"
        {
            return Err(format!("ui.comment.action: `{a}` is not post / cancel"));
        }
        if let Some(p) = &c.part
            && !super::workbench::selectable(model, p)
        {
            return Err(format!(
                "ui.comment.part: `{p}` is not a part of `{}`",
                model.name
            ));
        }
        if c.spot.is_some() && c.part.is_none() {
            return Err("ui.comment.spot needs a part".into());
        }
        wb.comment_ui = Some(c.clone());
        said.push("comment box request queued".into());
    }
    if let Some(c) = u.comments {
        wb.show_comments = c;
        said.push(format!("comments {}", if c { "open" } else { "closed" }));
    }
    if let (Some(sh), Some(s)) = (shading, &u.shading) {
        wb.shading = sh;
        said.push(format!("shading {s}"));
    }
    if let (Some(cw), Some(c)) = (capture, &u.capture) {
        wb.capture_window = cw;
        said.push(format!("screenshot captures the {c}"));
    }
    Ok(format!("ui: {}", said.join(", ")))
}

/// One drawn part of the window's scene.
pub(crate) struct WindowSlot {
    part: std::sync::Arc<Part>,
    /// What shows now: the finished entity, or a flat placeholder while the
    /// bake runs.
    entity: Option<Entity>,
    /// The entity of the part this one replaced (same name), kept on screen
    /// until this one's finished entity exists.
    old: Option<Entity>,
    finished: bool,
    /// Distance LODs attached (or none needed).
    lods_done: bool,
    has_lods: bool,
}

impl WindowSlot {
    fn despawn(&self, commands: &mut Commands) {
        for e in self.entity.iter().chain(&self.old) {
            commands.entity(*e).despawn();
        }
    }
}

/// Rebuilds the window's part entities whenever the model version moves,
/// and re-frames the orbit camera around the model's bounds. Meshes and
/// bakes stream in ([`super::stream`]): until a changed part's entity is
/// ready, the old one stays (or a flat placeholder shows).
#[allow(clippy::too_many_arguments)]
pub(crate) fn rebuild_scene(
    mut commands: Commands,
    mut modeler: ResMut<Modeler>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    (mut pbr_cache, mut streams): (ResMut<PbrCache>, ResMut<super::stream::Streams>),
    mut decal_mats: ResMut<Assets<bevy::pbr::decal::ForwardDecalMaterial<StandardMaterial>>>,
    mut orbit: ResMut<Orbit>,
    mut built: ResMut<SceneBuilt>,
    lights: Query<Entity, Or<(With<SceneLight>, With<super::decals::SceneDecal>)>>,
    layers: Option<Res<LayerVisibility>>,
    mut built_layers: Local<Option<LayerVisibility>>,
    mut slots: Local<std::collections::HashMap<usize, WindowSlot>>,
    mut previews: Local<Vec<Entity>>,
) {
    let _profile = crate::preview::profile::span("rebuild_scene");
    if modeler.session.viewer_away() {
        return;
    }
    // Map mode's eye toggles (view only).
    let layers = layers.map(|l| *l).unwrap_or_default();
    let version = modeler.session.model.version;
    if built.version != Some(version) || *built_layers != Some(layers) {
        built.version = Some(version);
        built.doc = Some(modeler.session.model.name.clone());
        *built_layers = Some(layers);
        pbr_cache.tick();
        for e in &lights {
            commands.entity(e).despawn();
        }
        if layers.lights {
            spawn_lights(&mut commands, &modeler.session.model);
        }
        if layers.decals {
            super::decals::spawn_decals(
                &mut commands,
                &modeler.session.model,
                &mut images,
                &mut pbr_cache,
                &mut decal_mats,
                None,
            );
        }
        // The model changed under any pending pin; drop it so no stale comment lands.
        modeler.session.pending_pin = None;
        let is_drawn = {
            let model = &modeler.session.model;
            let base = drawn(model);
            move |p: &Part| base(p) && layers.shows_part(model, &p.name)
        };
        // The cosmetic variant the window shows (set_camera variant / the chip).
        let variant_parts = modeler.session.window_variant_parts();
        // Parts whose shared `Arc` is unchanged keep their entity (an edit
        // to one part of a scene doesn't respawn the other 800).
        let mut previous = std::mem::take(&mut *slots);
        let mut fresh: Vec<usize> = Vec::new();
        for part in variant_parts
            .as_ref()
            .unwrap_or(&modeler.session.model.parts)
        {
            if part.verts.is_empty() || part.indices.is_empty() || !is_drawn(part) {
                continue;
            }
            let ptr = std::sync::Arc::as_ptr(part) as usize;
            if let Some(slot) = previous.remove(&ptr) {
                if let Some(e) = slot.entity.filter(|_| slot.finished) {
                    // Playback may have posed it: back to the part's own place.
                    commands.entity(e).insert(part_transform(part));
                }
                slots.insert(ptr, slot);
                continue;
            }
            fresh.push(ptr);
            slots.insert(
                ptr,
                WindowSlot {
                    part: std::sync::Arc::clone(part),
                    entity: None,
                    old: None,
                    finished: false,
                    lods_done: false,
                    has_lods: false,
                },
            );
        }
        // What the gone parts showed stands in for a changed part of the
        // same name until its own entity is ready.
        let mut by_name: std::collections::HashMap<String, Entity> = Default::default();
        for (_, slot) in previous.drain() {
            let shown = if slot.finished { slot.entity } else { slot.old };
            let spare = if slot.finished { slot.old } else { slot.entity };
            if let Some(e) = spare {
                commands.entity(e).despawn();
            }
            if let Some(e) = shown
                && let Some(other) = by_name.insert(slot.part.name.clone(), e)
            {
                commands.entity(other).despawn();
            }
        }
        for ptr in fresh {
            if let Some(slot) = slots.get_mut(&ptr) {
                slot.old = by_name.remove(&slot.part.name);
            }
        }
        for (_, e) in by_name {
            commands.entity(e).despawn();
        }
        for e in previews.drain(..) {
            commands.entity(e).despawn();
        }
        let marker_parts = if layers.game_built {
            marker_previews(&modeler.session.model)
        } else {
            Vec::new()
        };
        for (at, parts) in marker_parts {
            let root = commands
                .spawn((at, Visibility::default(), PreviewPart))
                .id();
            for part in &parts {
                let child = commands
                    .spawn(part_bundle(
                        part,
                        &mut meshes,
                        &mut materials,
                        &mut images,
                        &mut pbr_cache,
                    ))
                    .id();
                commands.entity(root).add_child(child);
            }
            previews.push(root);
        }
        if let Some((min, max)) = modeler.session.model.bounds() {
            let radius = (max - min).length() * 0.5;
            orbit.scene_radius = radius.max(0.01);
            // A new model (or the first build) re-frames the view and hands
            // control back to the drift; edits keep a camera the human steered.
            let name = modeler.session.model.name.clone();
            if !orbit.user || built.framed.as_deref() != Some(name.as_str()) {
                orbit.target = (min + max) * 0.5;
                orbit.dist = (radius * 2.4).max(1.0);
                orbit.user = false;
            }
            built.framed = Some(name);
        }
    }
    // Stream the parts in: whatever finished since last frame spawns now.
    let mut pending = 0;
    let mut has_lods = false;
    for slot in slots.values_mut() {
        if slot.finished && slot.lods_done {
            has_lods |= slot.has_lods;
            continue;
        }
        if slot.part.rig.is_some() {
            // Rigged parts keep their own mesh with CPU data (playback
            // poses it in place): built here, as before.
            let e = commands
                .spawn((
                    part_bundle(
                        &slot.part,
                        &mut meshes,
                        &mut materials,
                        &mut images,
                        &mut pbr_cache,
                    ),
                    PreviewPart,
                ))
                .id();
            slot.despawn(&mut commands);
            slot.entity = Some(e);
            slot.old = None;
            slot.finished = true;
            slot.lods_done = true;
            continue;
        }
        let Some(b) = super::stream::try_bundle(
            &slot.part,
            super::stream::Detail::Lod,
            &mut streams,
            &mut pbr_cache,
            &mut materials,
        ) else {
            pending += 1;
            continue;
        };
        let bundle = |m: &Handle<StandardMaterial>| {
            (
                Name::new(slot.part.name.clone()),
                Mesh3d(b.mesh.clone()),
                MeshMaterial3d(m.clone()),
                part_transform(&slot.part),
                PreviewPart,
            )
        };
        if !slot.finished {
            if b.final_material {
                let e = commands.spawn(bundle(&b.material)).id();
                slot.despawn(&mut commands);
                slot.entity = Some(e);
                slot.old = None;
                slot.finished = true;
            } else {
                pending += 1;
                if slot.entity.is_none() && slot.old.is_none() {
                    // Nothing to keep showing: the part in its flat colour
                    // until the bake lands.
                    slot.entity = Some(commands.spawn(bundle(&b.material)).id());
                }
                continue;
            }
        }
        if let (Some(lods), Some(e)) = (&b.lods, slot.entity) {
            super::stream::attach_lods(&mut commands, e, &b.material, lods);
            slot.lods_done = true;
            slot.has_lods = !lods.is_empty();
            has_lods |= slot.has_lods;
        }
    }
    built.pending = pending;
    built.lods = has_lods;
}

/// Uploads one RGBA8 texture with a box-filtered mip chain and a repeating,
/// anisotropic sampler.
pub(crate) fn pbr_image(
    w: usize,
    h: usize,
    px: &[u8],
    srgb: bool,
    cutoff: Option<f32>,
    images: &mut Assets<Image>,
) -> Handle<Image> {
    images.add(pbr_image_cpu(w, h, px, srgb, cutoff))
}

/// [`pbr_image`]'s image, built on any thread (the mip chain is the dear
/// part), for the main thread to add.
pub(crate) fn pbr_image_cpu(
    w: usize,
    h: usize,
    px: &[u8],
    srgb: bool,
    cutoff: Option<f32>,
) -> Image {
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    use bevy::render::render_resource::{Extent3d, TextureDimension};
    let (data, levels) = modeler::material::pbr::mips_with(w, h, px, srgb, cutoff);
    // Built from level 0 (Image::new checks the size), then the whole chain.
    let mut img = Image::new(
        Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        px.to_vec(),
        if srgb {
            TextureFormat::Rgba8UnormSrgb
        } else {
            TextureFormat::Rgba8Unorm
        },
        RenderAssetUsages::RENDER_WORLD,
    );
    img.data = Some(data);
    img.texture_descriptor.mip_level_count = levels;
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    img
}

/// The key a part's textured material is cached under: the bake plus the
/// factors that tint it (a colour token change must reach a part whose
/// texture is already cached) and whether the mesh is a closed solid
/// (cutouts are one-sided on closed solids, double on open cards).
pub(crate) fn material_key(part: &Part, closed: bool) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let r = part.material.pbr.as_ref()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    modeler::material::pbr::bake_key(r, Some(part)).hash(&mut h);
    closed.hash(&mut h);
    let m = &part.material;
    for x in m.base_color.iter().chain([&m.roughness, &m.metalness]) {
        x.to_bits().hash(&mut h);
    }
    Some(h.finish())
}

/// Whether a part with a procedural material is a closed solid (counting
/// open edges walks every triangle).
pub(crate) fn closed_solid(part: &Part) -> bool {
    !part.verts.is_empty()
        && !part.indices.is_empty()
        && modeler::verify::open_edge_count(part) == 0
}

/// A part's material bake as CPU images, ready for the main thread to add
/// ([`finish_material`]). Runs on any thread. `None` for plain parts or a
/// failed bake (reported on stderr; the part falls back to its factors).
pub(crate) struct PreparedMat {
    ch: Arc<modeler::material::bake::Channels>,
    base: Image,
    metal_rough: Image,
    normal: Image,
    occlusion: Option<Image>,
    emissive: Option<Image>,
    alpha: modeler::material::pbr::Alpha,
    double_sided: bool,
    color: [f32; 4],
    roughness: f32,
    metalness: f32,
}

pub(crate) fn prepare_material(part: &Part) -> Option<PreparedMat> {
    let ch = match modeler::material::pbr::bake_part(part) {
        Ok(Some(ch)) => ch,
        Ok(None) => return None,
        Err(e) => {
            eprintln!("{e}");
            return None;
        }
    };
    let t = modeler::material::pbr::textures(&ch);
    let cutoff = match t.alpha {
        modeler::material::pbr::Alpha::Mask(c) => Some(c),
        _ => None,
    };
    Some(PreparedMat {
        base: pbr_image_cpu(t.w, t.h, &t.base, true, cutoff),
        metal_rough: pbr_image_cpu(t.w, t.h, &t.metal_rough, false, None),
        normal: pbr_image_cpu(t.w, t.h, &t.normal, false, None),
        occlusion: t
            .occlusion
            .as_ref()
            .map(|ao| pbr_image_cpu(t.w, t.h, ao, false, None)),
        emissive: t
            .emissive
            .as_ref()
            .map(|e| pbr_image_cpu(t.w, t.h, e, true, None)),
        alpha: t.alpha,
        double_sided: modeler::material::pbr::double_sided_on(&t, part),
        color: part.material.base_color,
        roughness: part.material.roughness,
        metalness: part.material.metalness,
        ch,
    })
}

/// Adds a prepared bake's images and its material.
pub(crate) fn finish_material(
    p: PreparedMat,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) -> Handle<StandardMaterial> {
    let [cr, cg, cb, ca] = p.color;
    let mut m = StandardMaterial {
        base_color: Color::srgba(cr, cg, cb, ca),
        perceptual_roughness: p.roughness,
        metallic: p.metalness,
        base_color_texture: Some(images.add(p.base)),
        metallic_roughness_texture: Some(images.add(p.metal_rough)),
        normal_map_texture: Some(images.add(p.normal)),
        ..default()
    };
    if let Some(ao) = p.occlusion {
        m.occlusion_texture = Some(images.add(ao));
    }
    if let Some(e) = p.emissive {
        m.emissive_texture = Some(images.add(e));
        m.emissive = LinearRgba::WHITE;
    }
    match p.alpha {
        modeler::material::pbr::Alpha::Opaque => {}
        modeler::material::pbr::Alpha::Mask(c) => m.alpha_mode = AlphaMode::Mask(c),
        modeler::material::pbr::Alpha::Blend => m.alpha_mode = AlphaMode::Blend,
    }
    if p.double_sided {
        m.double_sided = true;
        m.cull_mode = None;
    }
    let x = &p.ch.extras;
    if let Some(v) = x.clearcoat {
        m.clearcoat = v;
    }
    if let Some(v) = x.clearcoat_roughness {
        m.clearcoat_perceptual_roughness = v;
    }
    if let Some(v) = x.anisotropy {
        m.anisotropy_strength = v;
    }
    if let Some(v) = x.anisotropy_rotation {
        m.anisotropy_rotation = v.to_radians();
    }
    if let Some(v) = x.transmission {
        m.diffuse_transmission = v;
    }
    if let Some(v) = x.specular_transmission {
        m.specular_transmission = v;
    }
    if let Some(v) = x.thickness {
        m.thickness = v;
    }
    if let Some(v) = x.ior {
        m.ior = v;
    }
    materials.add(m)
}

/// The textured material of a part with a procedural material (baked on
/// first use, then cached by bake key), on this thread: agent views need
/// it now. `None` for plain parts or when the bake fails.
fn pbr_material(
    part: &Part,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
) -> Option<Handle<StandardMaterial>> {
    part.material.pbr.as_ref()?;
    let closed = if part.verts.is_empty() || part.indices.is_empty() {
        false
    } else {
        let mh = mesh_hash(part, cache);
        *cache.closed.entry(mh).or_insert_with(|| closed_solid(part))
    };
    let key = material_key(part, closed)?;
    let generation = cache.generation;
    if let Some((h, used)) = cache.pbr.get_mut(&key) {
        *used = generation;
        return Some(h.clone());
    }
    let h = finish_material(prepare_material(part)?, materials, images);
    cache.pbr.insert(key, (h.clone(), generation));
    Some(h)
}

/// GPU meshes and flat materials shared between rebuilds, prefab
/// instances, scatter copies, grid tiles and agent views: a rebuild uploads
/// only geometry it hasn't seen (keyed by content), and parts with the same
/// flat look share one material so Bevy can batch them.
#[derive(Resource, Default)]
pub(crate) struct PbrCache {
    pub(crate) pbr: std::collections::HashMap<u64, (Handle<StandardMaterial>, u64)>,
    /// (verts ptr, lengths, sampled content) -> content hash, so unchanged
    /// parts aren't rehashed every rebuild.
    pub(crate) fingerprints: std::collections::HashMap<(usize, usize, usize, u64), u64>,
    pub(crate) meshes: std::collections::HashMap<(u64, bool), (Handle<Mesh>, u64)>,
    /// Mesh content hash -> closed solid (no open edges).
    pub(crate) closed: std::collections::HashMap<u64, bool>,
    pub(crate) flat: std::collections::HashMap<[u32; 6], Handle<StandardMaterial>>,
    /// A decal material's textures by bake key: base colour (alpha =
    /// opacity), normal, metallic-roughness, emissive.
    pub(crate) decals: std::collections::HashMap<u64, super::decals::DecalTextures>,
    pub(crate) generation: u64,
}

impl PbrCache {
    /// Called once per rebuild: drops meshes no rebuild used for a while.
    pub(crate) fn tick(&mut self) {
        self.generation += 1;
        let g = self.generation;
        self.meshes.retain(|_, (_, used)| g - *used < 32);
        // Textures are dear to re-bake: keep up to 256, the least recently
        // used go first.
        if self.pbr.len() > 256 {
            let mut ages: Vec<u64> = self.pbr.values().map(|(_, u)| *u).collect();
            ages.sort_unstable();
            let cut = ages[ages.len() - 256];
            self.pbr.retain(|_, (_, u)| *u >= cut);
        }
        if self.fingerprints.len() > 8192 {
            self.fingerprints.clear();
        }
        if self.closed.len() > 8192 {
            self.closed.clear();
        }
        if self.flat.len() > 4096 {
            self.flat.clear();
        }
    }
}

/// A cheap identity of a part's mesh buffers: their address, lengths and a
/// few sampled values (not the content).
pub(crate) fn fingerprint(part: &Part) -> (usize, usize, usize, u64) {
    use std::hash::{Hash, Hasher};
    let (nv, ni) = (part.verts.len(), part.indices.len());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if nv > 0 && ni > 0 {
        for k in 0..32 {
            part.verts[k * nv / 32].map(f32::to_bits).hash(&mut h);
            part.indices[k * ni / 32].hash(&mut h);
        }
    }
    (part.verts.as_ptr() as usize, nv, ni, h.finish())
}

/// The full content hash of a part's mesh (positions, normals, uvs,
/// indices). Any thread.
pub(crate) fn content_hash(part: &Part) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut b = std::collections::hash_map::DefaultHasher::new();
    for v in &part.verts {
        v.map(f32::to_bits).hash(&mut b);
    }
    for v in &part.normals {
        v.map(f32::to_bits).hash(&mut b);
    }
    for v in &part.uvs {
        v.map(f32::to_bits).hash(&mut b);
    }
    part.indices.hash(&mut b);
    b.finish()
}

fn mesh_hash(part: &Part, cache: &mut PbrCache) -> u64 {
    let fp = fingerprint(part);
    if let Some(h) = cache.fingerprints.get(&fp) {
        return *h;
    }
    let h = content_hash(part);
    cache.fingerprints.insert(fp, h);
    h
}

/// Whether a part's mesh carries tangents (a procedural material's normal
/// map needs them).
pub(crate) fn wants_tangents(part: &Part) -> bool {
    part.material.pbr.is_some()
        && !part.verts.is_empty()
        && part.normals.len() == part.verts.len()
        && part.uvs.len() == part.verts.len()
}

/// A part's render mesh, built on any thread. Main-world data stays:
/// picking ray-casts these meshes.
pub(crate) fn build_mesh(part: &Part, tangents: bool) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, part.verts.clone())
    .with_inserted_indices(Indices::U32(part.indices.clone()));
    if !part.normals.is_empty() {
        mesh = mesh.with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, part.normals.clone());
    }
    if !part.uvs.is_empty() {
        mesh = mesh.with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, part.uvs.clone());
    }
    if tangents && part.normals.len() == part.verts.len() && part.uvs.len() == part.verts.len() {
        // Bevy's mikktspace takes seconds on a 400k-triangle sculpt;
        // per-vertex accumulated tangents are a few milliseconds and
        // match it on smooth meshes.
        mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, fast_tangents(part));
    }
    mesh
}

/// The shared flat material of a part's factors.
pub(crate) fn flat_material(
    part: &Part,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut PbrCache,
) -> Handle<StandardMaterial> {
    let [r, g, b, a] = part.material.base_color;
    // A part wearing a procedural material has its factors set to 1 (they
    // multiply the bake); stand-in for a pending or failed bake, they would
    // read as a white mirror (metalness 1), so draw it matte instead.
    let (rough, metal) = if part.material.pbr.is_some() {
        (0.8, 0.0)
    } else {
        (part.material.roughness, part.material.metalness)
    };
    let fkey = [r, g, b, a, rough, metal].map(f32::to_bits);
    cache
        .flat
        .entry(fkey)
        .or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::srgba(r, g, b, a),
                perceptual_roughness: rough,
                metallic: metal,
                // A colour with alpha (glass, holograms) blends; Bevy
                // renders blended materials forward, over the deferred
                // opaque pass.
                alpha_mode: if a < 0.999 {
                    AlphaMode::Blend
                } else {
                    AlphaMode::Opaque
                },
                ..default()
            })
        })
        .clone()
}

/// A part's transform as Bevy's.
pub(crate) fn part_transform(part: &Part) -> Transform {
    Transform {
        translation: Vec3::from_array(part.translation),
        rotation: modeler::quat_of(part.rotation),
        scale: Vec3::from_array(part.scale),
    }
}

/// A part's entity, built on this thread (agent views, rigged parts):
/// bakes and mesh conversion block. The window and grid tiles stream
/// theirs instead ([`super::stream`]).
pub(crate) fn part_bundle(
    part: &Part,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
) -> impl Bundle {
    part_bundle_as(part, part, meshes, materials, images, cache)
}

/// [`part_bundle`] of `part`'s mesh wearing `look`'s material: a posed
/// copy of a rigged part keeps its rest part's bake (the game skins a mesh
/// textured at rest), so a filmstrip doesn't re-bake per-part materials
/// (`mesh_ao`, curvature wear) for every frame's posed geometry.
pub(crate) fn part_bundle_as(
    part: &Part,
    look: &Part,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
) -> impl Bundle {
    let mat_span = super::profile::span("part_bundle: material");
    let textured = pbr_material(look, materials, images, cache);
    drop(mat_span);
    let _mesh_span = super::profile::span("part_bundle: mesh");
    let tangents = wants_tangents(part);
    // Rigged parts are re-posed in place by playback: they keep their own
    // mesh with its data on the CPU side.
    let shared = part.rig.is_none();
    let key = shared.then(|| (mesh_hash(part, cache), tangents));
    let generation = cache.generation;
    let cached = key.and_then(|k| cache.meshes.get_mut(&k)).map(|(h, used)| {
        *used = generation;
        h.clone()
    });
    let mesh = cached.unwrap_or_else(|| {
        let h = meshes.add(build_mesh(part, tangents));
        if let Some(k) = key {
            cache.meshes.insert(k, (h.clone(), generation));
        }
        h
    });
    let material = textured.unwrap_or_else(|| flat_material(look, materials, cache));
    (
        Name::new(part.name.clone()),
        Mesh3d(mesh),
        MeshMaterial3d(material),
        part_transform(part),
    )
}

/// Per-vertex tangents (xyz + handedness in w) accumulated from each
/// triangle's uv gradient, then orthogonalized against the normal.
///
/// Convention (checked against Bevy 0.20 and `modeler::gltf::tangents`, which
/// the material bake encodes normal maps against): uvs are glTF's, v = 0 at
/// the image top, and Bevy neither flips v nor sets `flip_normal_map_y`. Its
/// shader builds the bitangent as `w * cross(N, T)` and maps the green
/// channel (image up = -v) onto it, so that bitangent must point along
/// `-dP/dv`: `w = -1` when `cross(N, T) . (-dP/dv) < 0`. (Bevy's own
/// mikktspace path gets there by negating mikktspace's w, which is defined
/// against `+dP/dv`.) `bit` below is `dP/dv`, hence the negation.
fn fast_tangents(part: &Part) -> Vec<[f32; 4]> {
    let n = part.verts.len();
    let mut tan = vec![Vec3::ZERO; n];
    let mut bit = vec![Vec3::ZERO; n];
    for t in part.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| i as usize);
        if a >= n || b >= n || c >= n {
            continue;
        }
        let (p0, p1, p2) = (
            Vec3::from_array(part.verts[a]),
            Vec3::from_array(part.verts[b]),
            Vec3::from_array(part.verts[c]),
        );
        let (u0, u1, u2) = (
            Vec2::from_array(part.uvs[a]),
            Vec2::from_array(part.uvs[b]),
            Vec2::from_array(part.uvs[c]),
        );
        let (e1, e2) = (p1 - p0, p2 - p0);
        let (d1, d2) = (u1 - u0, u2 - u0);
        let det = d1.x * d2.y - d2.x * d1.y;
        if det.abs() < 1e-20 {
            continue;
        }
        let r = 1.0 / det;
        let sdir = (e1 * d2.y - e2 * d1.y) * r;
        let tdir = (e2 * d1.x - e1 * d2.x) * r;
        for i in [a, b, c] {
            tan[i] += sdir;
            bit[i] += tdir;
        }
    }
    (0..n)
        .map(|i| {
            let nrm = Vec3::from_array(part.normals[i]);
            let t = (tan[i] - nrm * nrm.dot(tan[i])).normalize_or(nrm.any_orthonormal_vector());
            let w = if nrm.cross(t).dot(-bit[i]) < 0.0 {
                -1.0
            } else {
                1.0
            };
            [t.x, t.y, t.z, w]
        })
        .collect()
}

/// A light that belongs to the window camera alone (`WINDOW_LIGHT_LAYER`).
#[derive(Component)]
pub(crate) struct WindowLight;

/// The render layer of `WindowLight`s; only the window camera has it.
pub(crate) const WINDOW_LIGHT_LAYER: usize = 29;

/// Environment light strength (cd/m² per unit of studio radiance).
pub(crate) const STUDIO_INTENSITY: f32 = 900.0;

/// The procedural photo studio (`modeler::material::env`) as an environment
/// light: glossy parts reflect soft boxes and a cyclorama horizon instead of
/// a flat ambient, so roughness and normal maps read. Both maps are built on
/// the CPU, exactly and deterministically (SH irradiance, analytically
/// blurred specular mips), not by Bevy's GPU Monte-Carlo filter, whose
/// per-texel noise showed as mottled grain on smooth flat-coloured parts and
/// varied between renders. The same function lights the CPU material sphere.
pub(crate) fn studio_env(images: &mut Assets<Image>, intensity: f32) -> EnvironmentMapLight {
    use bevy::render::render_resource::{
        Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
    };
    let cube = |size: u32, data: Vec<u8>, mips: u32| {
        let mut img = Image::new(
            Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 6,
            },
            TextureDimension::D2,
            // `Image::new` checks the data against mip 0 only: give it that
            // size, then the whole chain.
            vec![0; (size * size * 6 * 8) as usize],
            TextureFormat::Rgba16Float,
            bevy::asset::RenderAssetUsages::default(),
        );
        img.data = Some(data);
        img.texture_descriptor.mip_level_count = mips;
        img.texture_view_descriptor = Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::Cube),
            ..default()
        });
        img
    };
    const DIFFUSE: u32 = 32;
    const SPECULAR: u32 = 256;
    let diffuse = modeler::material::env::diffuse_cubemap_rgba16f(DIFFUSE as usize);
    let (specular, mips) = modeler::material::env::specular_cubemap_rgba16f(SPECULAR as usize);
    EnvironmentMapLight {
        diffuse_map: images.add(cube(DIFFUSE, diffuse, 1)),
        specular_map: images.add(cube(SPECULAR, specular, mips)),
        intensity,
        ..default()
    }
}

/// Contact shading: screen-space ambient occlusion darkens creases and
/// contacts (a counter against its wall, chair legs on the floor), the main
/// cue that separates touching parts. SSAO needs MSAA off, so edges are
/// anti-aliased by SMAA instead (single-frame, so agent shots stay
/// deterministic, unlike TAA).
pub(crate) fn contact_shading() -> impl Bundle {
    (
        Msaa::Off,
        contact_occlusion(),
        bevy::anti_alias::smaa::Smaa::default(),
    )
}

/// The SSAO component alone (see `contact_shading`): perspective cameras
/// only, since Bevy's SSAO mis-handles orthographic projections.
pub(crate) fn contact_occlusion() -> bevy::pbr::ScreenSpaceAmbientOcclusion {
    bevy::pbr::ScreenSpaceAmbientOcclusion {
        radius: 0.5,
        ..default()
    }
}

/// Screen-space reflections: glossy surfaces reflect the other parts on
/// screen (a fridge in a polished floor), on top of the studio. Needs the
/// deferred renderer (the app default; cameras without a deferred prepass
/// still render forward) and MSAA off (set by `contact_shading`, which
/// must sit on the same camera).
pub(crate) fn screen_reflections() -> impl Bundle {
    (bevy::pbr::ScreenSpaceReflections {
        // Mirrors too (Bevy's default starts at 0.08).
        min_perceptual_roughness: 0.0..0.0,
        max_perceptual_roughness: 0.55..0.65,
        linear_steps: 32,
        thickness: 0.1,
        ..default()
    },)
}

/// The 3D view's render target: the human's camera renders here, and a UI
/// node shows it in the space the panes leave.
#[derive(Resource, Default)]
pub(crate) struct View3d {
    pub(crate) image: Handle<Image>,
    /// The 3D area in logical window pixels.
    pub(crate) rect: Rect,
    /// The image size in physical pixels.
    pub(crate) size: UVec2,
}

#[derive(Component)]
pub(crate) struct View3dNode;

/// Offscreen: the image the UI camera renders the whole window into.
#[derive(Resource, Default)]
pub(crate) struct UiCanvas(pub(crate) Option<Handle<Image>>);

pub(crate) fn view_target(w: u32, h: u32) -> Image {
    let mut img = Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None);
    // Screenshots copy it back.
    img.texture_descriptor.usage |= bevy::render::render_resource::TextureUsages::COPY_SRC;
    img
}

/// Camera and lights (no floor: the camera may orbit below the model).
pub(crate) fn setup_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    assets: Res<AssetServer>,
    offscreen: Option<Res<Offscreen>>,
) {
    // Offscreen there is no surface: the UI camera renders the whole
    // workbench into a 1280x800 image that window captures read.
    let canvas = offscreen
        .is_some_and(|o| o.0)
        .then(|| images.add(view_target(1280, 800)));
    commands.insert_resource(UiCanvas(canvas.clone()));
    // AmbientLight is a per-camera component in Bevy 0.20 (a standalone
    // entity does nothing), so faces turned from the sun went black. A dim
    // camera-aligned fill keeps every side readable as the human orbits.
    // The UI camera owns the window; the 3D camera renders to an image the
    // size of the 3D area, shown by a UI node (`View3d`). A camera viewport
    // inside the window broke screen-space reflections: their ray march maps
    // to UVs over the whole target, so hits landed in the wrong place.
    let ui_cam = Some(
        commands
            .spawn((
                Name::new("ui camera"),
                Camera2d,
                Camera {
                    order: 1,
                    ..default()
                },
                IsDefaultUiCamera,
            ))
            .id(),
    );
    if let Some(canvas) = canvas
        && let Some(cam) = ui_cam
    {
        commands
            .entity(cam)
            .insert(RenderTarget::Image(canvas.into()));
    }
    let view_image = images.add(view_target(16, 16));
    commands.spawn((
        Name::new("3d view"),
        View3dNode,
        ImageNode::new(view_image.clone()),
        Node {
            position_type: PositionType::Absolute,
            ..default()
        },
        Pickable::IGNORE,
        GlobalZIndex(-1),
    ));
    commands.insert_resource(View3d {
        image: view_image.clone(),
        ..default()
    });
    commands
        .spawn((
            Name::new("camera"),
            MainCamera,
            Camera3d::default(),
            // The scene, the bone gizmos and the window-only lights.
            RenderLayers::from_layers(&[
                0,
                WINDOW_LIGHT_LAYER,
                crate::preview::timeline::BONE_LAYER,
            ]),
            studio_env(&mut images, STUDIO_INTENSITY),
            Msaa::Off,
            // TAA, not SMAA: SSR's ray march is jittered per frame, and the
            // temporal history averages that noise out (and the edges too).
            bevy::anti_alias::taa::TemporalAntiAliasing::default(),
            screen_reflections(),
            // Deferred like every material (the app default), with SSR.
            // No SSAO here: on the window camera it froze presentation
            // (the view updated only on resize) on this Metal device, as
            // does a forward window camera over deferred materials.
            AmbientLight {
                brightness: 60.0,
                ..default()
            },
            Transform::from_translation(Vec3::new(4.0, 3.0, 6.0)).looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .insert(RenderTarget::Image(view_image.into()))
        .with_children(|c| {
            c.spawn((
                Name::new("view fill"),
                crate::preview::agent_rig::StudioLight,
                // It turns with the human's orbiting camera: on the layer
                // only that camera sees, so it never lights agent views
                // (which would then change with the window's drift).
                WindowLight,
                RenderLayers::layer(WINDOW_LIGHT_LAYER),
                DirectionalLight {
                    illuminance: 500.0,
                    ..default()
                },
                Transform::default(),
            ));
        });
    commands.spawn((
        Name::new("sun"),
        crate::preview::agent_rig::StudioLight,
        DirectionalLight {
            illuminance: 8000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.5, 0.0)),
    ));
    commands.spawn((
        Name::new("playback hud"),
        PlaybackHud,
        Text::new(String::new()),
        ui_text(&assets, 15.0, false),
        ui_color(false),
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(8.0),
            left: Val::Px(8.0),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        bevy::feathers::theme::ThemeBackgroundColor(bevy::feathers::tokens::PANE_HEADER_BG),
        GlobalZIndex(3),
    ));
    commands.spawn((
        Name::new("typing hud"),
        TypingHud,
        Text::new(String::new()),
        ui_text(&assets, 16.0, false),
        ui_color(false),
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(8.0),
            right: Val::Px(8.0),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        bevy::feathers::theme::ThemeBackgroundColor(bevy::feathers::tokens::PANE_HEADER_BG),
        GlobalZIndex(3),
    ));
}

/// Markers and colliders of the current model, re-collected when it changes.
#[derive(Default)]
pub(crate) struct GameGizmoCache {
    version: Option<u64>,
    markers: Vec<modeler::game::Marker>,
    colliders: Vec<modeler::game::Collider>,
}

/// Draws markers (a cross, a facing arrow, the volume box) and collider
/// boxes (green static, orange moving, blue sensors) in the 3D view and
/// agent views, so placement is checkable by eye.
pub(crate) fn draw_game_gizmos(
    modeler: Res<Modeler>,
    mut cache: Local<GameGizmoCache>,
    mut gizmos: Gizmos,
    layers: Option<Res<LayerVisibility>>,
) {
    let _profile = crate::preview::profile::span("draw_game_gizmos");
    let layers = layers.map(|l| *l).unwrap_or_default();
    let model = &modeler.session.model;
    if cache.version != Some(model.version) {
        cache.version = Some(model.version);
        cache.markers = modeler::game::markers(model).unwrap_or_default();
        cache.colliders = modeler::game::colliders(model).unwrap_or_default();
    }
    let marker_color = Color::srgb(1.0, 0.25, 0.85);
    for m in cache.markers.iter().filter(|_| layers.markers) {
        let p = m.position;
        let r = 0.15;
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            gizmos.line(p - axis * r, p + axis * r, marker_color);
        }
        let facing = m.rotation * Vec3::Z;
        gizmos.arrow(p, p + facing * 0.5, marker_color);
        if let Some(z) = m.size {
            gizmos.cube(
                Transform::from_translation(p)
                    .with_rotation(m.rotation)
                    .with_scale(Vec3::from(z)),
                marker_color.with_alpha(0.6),
            );
        }
    }
    for c in cache.colliders.iter().filter(|_| layers.colliders) {
        let color = if c.sensor {
            Color::srgba(0.3, 0.6, 1.0, 0.5)
        } else if c.body == "static" {
            Color::srgba(0.3, 1.0, 0.4, 0.35)
        } else {
            Color::srgba(1.0, 0.6, 0.2, 0.6)
        };
        gizmos.cube(
            Transform::from_translation(c.center)
                .with_rotation(c.rotation)
                .with_scale(c.half_extents * 2.0),
            color,
        );
    }
}

/// Whether the window (and the grid) draws a part: every visible part, and
/// a hidden terrain too. A map hides its ground so the export leaves it to
/// the game (which builds it from the heights file), but without it the
/// window showed only marker gizmos over empty space.
pub(crate) fn drawn(model: &modeler::Model) -> impl Fn(&modeler::Part) -> bool + '_ {
    let terrains: std::collections::HashSet<&str> = model
        .ops
        .iter()
        .filter_map(|o| match o {
            modeler::Op::Terrain { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    move |p| p.visible || terrains.contains(p.name.as_str())
}

/// The models markers name in `preview` (what game code builds there), each
/// with the marker's transform and the prefab's visible parts. Prefabs load
/// in the model's own project; one that fails to load is skipped.
pub(crate) fn marker_previews(
    model: &modeler::Model,
) -> Vec<(Transform, Vec<std::sync::Arc<modeler::Part>>)> {
    let Ok(markers) = modeler::game::markers(model) else {
        return Vec::new();
    };
    let mut loaded: std::collections::HashMap<String, Vec<std::sync::Arc<modeler::Part>>> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    // Previews stand on the ground (markers sit at y = 0; the game snaps to
    // the terrain), hidden terrain included.
    let terrains: std::collections::HashSet<&str> = model
        .ops
        .iter()
        .filter_map(|o| match o {
            modeler::Op::Terrain { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let ground: Vec<&modeler::Part> = model
        .parts
        .iter()
        .map(|p| &**p)
        .filter(|p| terrains.contains(p.name.as_str()))
        .collect();
    for mut m in markers {
        let Some(name) = m.preview.clone() else {
            continue;
        };
        let name = name.as_str();
        let top = Vec3::new(m.position.x, 10_000.0, m.position.z);
        if let Some(t) = ground
            .iter()
            .filter_map(|p| modeler::contact::ray_part(p, top, Vec3::NEG_Y))
            .min_by(f32::total_cmp)
        {
            m.position.y = top.y - t;
        }
        if !loaded.contains_key(name) {
            let parts = modeler::catalog::with_project(model.project.clone(), || {
                let Ok(src) = modeler::catalog::prefab_instance(name) else {
                    return Vec::new();
                };
                let src = if src.parts.is_empty() && !src.ops.is_empty() {
                    let mut m = (*src).clone();
                    if m.refresh().is_err() {
                        return Vec::new();
                    }
                    std::sync::Arc::new(m)
                } else {
                    src
                };
                src.parts
                    .iter()
                    .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty())
                    .cloned()
                    .collect()
            });
            loaded.insert(name.to_string(), parts);
        }
        let parts = loaded.get(name).cloned().unwrap_or_default();
        if !parts.is_empty() {
            out.push((
                Transform::from_translation(m.position).with_rotation(m.rotation),
                parts,
            ));
        }
    }
    out
}

/// Drops the window camera's screen-space reflections while frames run over
/// budget (a dense forest: SSR's ray march over a full-screen G-buffer is a
/// large share of the GPU frame) and brings them back once frames are cheap
/// again. Hysteresis and a hold time keep it from flickering.
/// `MODELER_SSR=0` / `1` pins it off / on.
pub(crate) fn adapt_reflections(
    time: Res<Time<Real>>,
    mut state: Local<(f32, f32, bool)>,
    cams: Query<(Entity, Has<bevy::pbr::ScreenSpaceReflections>), With<MainCamera>>,
    mut commands: Commands,
) {
    static MODE: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    let pinned = *MODE.get_or_init(|| match std::env::var("MODELER_SSR").as_deref() {
        Ok("0") => Some(false),
        Ok("1") => Some(true),
        _ => None,
    });
    let (ema, held, init) = &mut *state;
    let dt = time.delta_secs() * 1000.0;
    if !*init {
        *init = true;
        *ema = dt.min(16.0);
    }
    // Skip one-off stalls (a rebuild, a bake): they are not render cost.
    if dt < 250.0 {
        *ema += (dt - *ema) * 0.1;
    }
    *held += time.delta_secs();
    let Ok((cam, on)) = cams.single() else {
        return;
    };
    let want = pinned.unwrap_or(if on { *ema < 30.0 } else { *ema < 14.0 });
    if want != on && (pinned.is_some() || *held > 2.0) {
        *held = 0.0;
        if want {
            commands.entity(cam).insert(screen_reflections());
        } else {
            commands
                .entity(cam)
                .remove::<bevy::pbr::ScreenSpaceReflections>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window must decode normal maps with the handedness the bake
    /// (`modeler::gltf::tangents`) encoded them with.
    #[test]
    fn preview_tangents_match_the_bake_on_a_uv_sphere() {
        let mut part =
            modeler::primitives::primitive(modeler::PrimitiveKind::Sphere, [0.5, 0.0, 0.0], "s")
                .unwrap_or_else(|e| panic!("{e}"));
        modeler::uv::project(&mut part, "spherical").unwrap_or_else(|e| panic!("{e}"));
        let fast = fast_tangents(&part);
        let bake = modeler::gltf::tangents(&part);
        let (mut compared, mut agree) = (0, 0);
        for (i, (f, b)) in fast.iter().zip(&bake).enumerate() {
            if part.normals[i][1].abs() > 0.95 {
                continue; // poles: no uv gradient
            }
            let d = Vec3::new(f[0], f[1], f[2]).dot(Vec3::new(b[0], b[1], b[2]));
            if d < 0.999 {
                continue; // degenerate uv gradient (seam): fallbacks differ
            }
            compared += 1;
            if f[3] == b[3] {
                agree += 1;
            }
        }
        assert!(compared > 100, "{compared}");
        assert_eq!(agree, compared, "handedness differs from the bake");
    }
}
