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
    /// The model the camera was last framed for.
    pub(crate) framed: Option<String>,
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
) {
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
                } else if offscreen.0 {
                    // No window: render the window camera's view (the orbit
                    // that set_camera and auto-framing steer) with the agent rig.
                    agent_shots.0.push((
                        AgentView::Custom(CustomCam {
                            yaw: 90.0 - orbit.yaw.to_degrees(),
                            pitch: orbit.pitch.to_degrees(),
                            dist: Some(orbit.dist),
                            target: Some(orbit.target.to_array()),
                            ortho: false,
                            fov: 45.0,
                        }),
                        1280,
                        800,
                        None,
                        // The window's own pose when it is playing a clip.
                        ShotStyle {
                            pose: playback.clip.clone().map(|c| (Some(c), playback.time)),
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
                    let mut msg = match play {
                        Some(r) => playback.request(&modeler.session.model, &r),
                        None => Ok(String::new()),
                    };
                    if let (Some(u), Ok(m)) = (ui, &mut msg) {
                        let applied = match (wb.as_deref_mut(), grid.as_deref_mut()) {
                            (Some(wb), Some(grid)) => {
                                apply_ui(&u, &modeler.session.model, wb, grid)
                            }
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

/// `set_camera(ui: ...)`: the window's UI as the human would set it.
fn apply_ui(
    u: &crate::server::UiArg,
    model: &Model,
    wb: &mut Workbench,
    grid: &mut Grid,
) -> Result<String, String> {
    let mut said = Vec::new();
    if let Some(m) = &u.mode {
        if m == "grid" {
            grid.on = true;
        } else {
            let mode = Mode::ALL
                .into_iter()
                .find(|x| x.label().eq_ignore_ascii_case(m))
                .ok_or_else(|| {
                    format!("ui.mode: `{m}` is not model / material / animate / map / grid")
                })?;
            mode_available(model, mode).map_err(|why| format!("ui.mode {m}: {why}"))?;
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
    if let Some(c) = u.comments {
        wb.show_comments = c;
        said.push(format!("comments {}", if c { "open" } else { "closed" }));
    }
    if let Some(s) = &u.shading {
        wb.shading = match s.as_str() {
            "lit" => Shading::Lit,
            "clay" => Shading::Clay,
            "parts" => Shading::Parts,
            _ => return Err(format!("ui.shading: `{s}` is not lit / clay / parts")),
        };
        said.push(format!("shading {s}"));
    }
    if let Some(c) = &u.capture {
        wb.capture_window = match c.as_str() {
            "window" => true,
            "view" => false,
            _ => return Err(format!("ui.capture: `{c}` is not window / view")),
        };
        said.push(format!("screenshot captures the {c}"));
    }
    Ok(format!("ui: {}", said.join(", ")))
}

/// Rebuilds one entity per part whenever the model version moves, and re-frames the
/// orbit camera around the model's bounds.
pub(crate) fn rebuild_scene(
    mut commands: Commands,
    mut modeler: ResMut<Modeler>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut pbr_cache: ResMut<PbrCache>,
    mut orbit: ResMut<Orbit>,
    mut built: ResMut<SceneBuilt>,
    existing: Query<Entity, With<PreviewPart>>,
    pins: Query<Entity, With<AnnotationPin>>,
    lights: Query<Entity, With<SceneLight>>,
    layers: Option<Res<LayerVisibility>>,
    mut built_layers: Local<Option<LayerVisibility>>,
) {
    // Map mode's eye toggles (view only).
    let layers = layers.map(|l| *l).unwrap_or_default();
    let version = modeler.session.model.version;
    if built.version == Some(version) && *built_layers == Some(layers) {
        return;
    }
    built.version = Some(version);
    *built_layers = Some(layers);
    pbr_cache.tick();
    for e in existing.iter().chain(&pins).chain(&lights) {
        commands.entity(e).despawn();
    }
    if layers.lights {
        spawn_lights(&mut commands, &modeler.session.model);
    }
    // The model changed under any pending pin; drop it so no stale comment lands.
    modeler.session.pending_pin = None;
    // Bake every part's material at once (in parallel) before spawning.
    let is_drawn = {
        let model = &modeler.session.model;
        let base = drawn(model);
        move |p: &Part| base(p) && layers.shows_part(model, &p.name)
    };
    modeler::material::pbr::prebake(
        modeler
            .session
            .model
            .parts
            .iter()
            .filter(|p| !p.verts.is_empty() && is_drawn(p))
            .map(|p| &**p),
    );
    for part in &modeler.session.model.parts {
        if part.verts.is_empty() || part.indices.is_empty() || !is_drawn(part) {
            continue;
        }
        commands.spawn((
            part_bundle(
                part,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut pbr_cache,
            ),
            PreviewPart,
        ));
    }
    let previews = if layers.game_built {
        marker_previews(&modeler.session.model)
    } else {
        Vec::new()
    };
    for (at, parts) in previews {
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

/// Uploads one RGBA8 texture with a box-filtered mip chain and a repeating,
/// anisotropic sampler.
fn pbr_image(
    w: usize,
    h: usize,
    px: &[u8],
    srgb: bool,
    cutoff: Option<f32>,
    images: &mut Assets<Image>,
) -> Handle<Image> {
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
    images.add(img)
}

/// The textured material of a part with a procedural material (baked on
/// first use, then cached by bake key). `None` for plain parts or when the
/// bake fails (reported on stderr; the part falls back to its factors).
fn pbr_material(
    part: &Part,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
) -> Option<Handle<StandardMaterial>> {
    let r = part.material.pbr.as_ref()?;
    // The bake plus the factors that tint it: a colour token change must
    // reach a part whose texture is already cached.
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        modeler::material::pbr::bake_key(r, Some(part)).hash(&mut h);
        let m = &part.material;
        for x in m.base_color.iter().chain([&m.roughness, &m.metalness]) {
            x.to_bits().hash(&mut h);
        }
        h.finish()
    };
    let generation = cache.generation;
    if let Some((h, used)) = cache.pbr.get_mut(&key) {
        *used = generation;
        return Some(h.clone());
    }
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
    let [cr, cg, cb, ca] = part.material.base_color;
    let mut m = StandardMaterial {
        base_color: Color::srgba(cr, cg, cb, ca),
        perceptual_roughness: part.material.roughness,
        metallic: part.material.metalness,
        base_color_texture: Some(pbr_image(t.w, t.h, &t.base, true, cutoff, images)),
        metallic_roughness_texture: Some(pbr_image(t.w, t.h, &t.metal_rough, false, None, images)),
        normal_map_texture: Some(pbr_image(t.w, t.h, &t.normal, false, None, images)),
        ..default()
    };
    if let Some(ao) = &t.occlusion {
        m.occlusion_texture = Some(pbr_image(t.w, t.h, ao, false, None, images));
    }
    if let Some(e) = &t.emissive {
        m.emissive_texture = Some(pbr_image(t.w, t.h, e, true, None, images));
        m.emissive = LinearRgba::WHITE;
    }
    match t.alpha {
        modeler::material::pbr::Alpha::Opaque => {}
        modeler::material::pbr::Alpha::Mask(c) => m.alpha_mode = AlphaMode::Mask(c),
        modeler::material::pbr::Alpha::Blend => m.alpha_mode = AlphaMode::Blend,
    }
    if t.double_sided {
        m.double_sided = true;
        m.cull_mode = None;
    }
    let x = &ch.extras;
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

    let h = materials.add(m);
    cache.pbr.insert(key, (h.clone(), generation));
    Some(h)
}

/// GPU meshes and flat materials shared between rebuilds, prefab
/// instances, scatter copies, grid tiles and agent views: a rebuild uploads
/// only geometry it hasn't seen (keyed by content), and parts with the same
/// flat look share one material so Bevy can batch them.
#[derive(Resource, Default)]
pub(crate) struct PbrCache {
    pbr: std::collections::HashMap<u64, (Handle<StandardMaterial>, u64)>,
    /// (verts ptr, lengths, sampled content) -> content hash, so unchanged
    /// parts aren't rehashed every rebuild.
    fingerprints: std::collections::HashMap<(usize, usize, usize, u64), u64>,
    meshes: std::collections::HashMap<(u64, bool), (Handle<Mesh>, u64)>,
    flat: std::collections::HashMap<[u32; 6], Handle<StandardMaterial>>,
    generation: u64,
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
        if self.flat.len() > 4096 {
            self.flat.clear();
        }
    }
}

fn mesh_hash(part: &Part, cache: &mut PbrCache) -> u64 {
    use std::hash::{Hash, Hasher};
    let (nv, ni) = (part.verts.len(), part.indices.len());
    let sample = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for k in 0..32 {
            part.verts[k * nv / 32].map(f32::to_bits).hash(&mut h);
            part.indices[k * ni / 32].hash(&mut h);
        }
        h.finish()
    };
    let fp = (part.verts.as_ptr() as usize, nv, ni, sample);
    if let Some(h) = cache.fingerprints.get(&fp) {
        return *h;
    }
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
    let h = b.finish();
    cache.fingerprints.insert(fp, h);
    h
}

pub(crate) fn part_bundle(
    part: &Part,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
) -> impl Bundle {
    let textured = pbr_material(part, materials, images, cache);
    let tangents = textured.is_some() && !part.normals.is_empty() && !part.uvs.is_empty();
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
        // Main-world data stays: picking ray-casts these meshes.
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
        if tangents && let Err(e) = mesh.generate_tangents() {
            eprintln!("tangents for `{}`: {e}", part.name);
        }
        let h = meshes.add(mesh);
        if let Some(k) = key {
            cache.meshes.insert(k, (h.clone(), generation));
        }
        h
    });
    let material = textured.unwrap_or_else(|| {
        let [r, g, b, a] = part.material.base_color;
        let fkey = [r, g, b, a, part.material.roughness, part.material.metalness].map(f32::to_bits);
        cache
            .flat
            .entry(fkey)
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: Color::srgba(r, g, b, a),
                    perceptual_roughness: part.material.roughness,
                    metallic: part.material.metalness,
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
    });
    (
        Name::new(part.name.clone()),
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform {
            translation: Vec3::from_array(part.translation),
            rotation: modeler::quat_of(part.rotation),
            scale: Vec3::from_array(part.scale),
        },
    )
}

/// Environment light strength (cd/m² per unit of studio radiance).
pub(crate) const STUDIO_INTENSITY: f32 = 900.0;

/// The procedural photo studio (`modeler::material::env`) as an environment
/// light, filtered on the GPU: glossy parts reflect soft boxes and a
/// cyclorama horizon instead of a flat ambient, so roughness and normal maps
/// read. The same function lights the CPU material sphere.
pub(crate) fn studio_env(
    images: &mut Assets<Image>,
    intensity: f32,
) -> GeneratedEnvironmentMapLight {
    use bevy::render::render_resource::{
        Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
    };
    const SIZE: u32 = 256;
    let mut img = Image::new(
        Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        modeler::material::env::cubemap_rgba16f(SIZE as usize),
        TextureFormat::Rgba16Float,
        bevy::asset::RenderAssetUsages::default(),
    );
    img.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    GeneratedEnvironmentMapLight {
        environment_map: images.add(img),
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
        bevy::pbr::ScreenSpaceAmbientOcclusion {
            radius: 0.5,
            ..default()
        },
        bevy::anti_alias::smaa::Smaa::default(),
    )
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
) {
    // AmbientLight is a per-camera component in Bevy 0.20 (a standalone
    // entity does nothing), so faces turned from the sun went black. A dim
    // camera-aligned fill keeps every side readable as the human orbits.
    // The UI camera owns the window; the 3D camera renders to an image the
    // size of the 3D area, shown by a UI node (`View3d`). A camera viewport
    // inside the window broke screen-space reflections: their ray march maps
    // to UVs over the whole target, so hits landed in the wrong place.
    commands.spawn((
        Name::new("ui camera"),
        Camera2d,
        Camera {
            order: 1,
            ..default()
        },
        IsDefaultUiCamera,
    ));
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
            Transform::from_translation(c.center).with_scale(c.half_extents * 2.0),
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
