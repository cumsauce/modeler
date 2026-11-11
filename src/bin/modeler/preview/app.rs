use crate::prelude::*;

#[derive(Resource)]
pub(crate) struct Modeler {
    pub(crate) session: Session,
    pub(crate) bridge: Arc<Bridge>,
}

#[derive(Resource)]
pub(crate) struct Headless(pub(crate) bool);

pub(crate) fn windowed(headless: Option<Res<Headless>>) -> bool {
    !headless.map(|h| h.0).unwrap_or(false)
}

#[derive(Component)]
pub(crate) struct PreviewPart;

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
) -> impl FnMut(Res<Modeler>, MessageWriter<AppExit>) {
    move |modeler, mut exit| {
        if headless && modeler.bridge.quit() {
            exit.write(AppExit::Success);
        }
    }
}

/// Drains MCP commands every frame and executes them against the session.
pub(crate) fn drain_commands(
    headless: Res<Headless>,
    mut modeler: ResMut<Modeler>,
    mut pending: ResMut<PendingShots>,
    mut agent_shots: ResMut<PendingAgentShots>,
    mut orbit: ResMut<Orbit>,
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
                } else {
                    pending.0.push(Mutex::new(Some(reply)));
                }
            }
            Cmd::SetCamera {
                yaw,
                pitch,
                dist,
                reply,
            } => {
                if headless.0 {
                    let _ = reply.send(Err("headless: no camera to move".into()));
                } else {
                    // Same convention as agent custom cameras: yaw 0 = on +z looking -z.
                    orbit.yaw = (90.0 - yaw).to_radians();
                    orbit.pitch = pitch.clamp(-89.0, 89.0).to_radians();
                    orbit.dist = dist.clamp(0.2, 200.0);
                    orbit.user = true;
                    let _ = reply.send(Ok(vec![]));
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
                    let _ = reply.send(Err("headless: no renderer, no agent view".into()));
                } else {
                    agent_shots
                        .0
                        .push((view, width, height, focus, style, reply));
                }
            }
        }
    }
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
) {
    let version = modeler.session.model.version;
    if built.version == Some(version) {
        return;
    }
    built.version = Some(version);
    for e in existing.iter().chain(&pins) {
        commands.entity(e).despawn();
    }
    // The model changed under any pending pin; drop it so no stale comment lands.
    modeler.session.pending_pin = None;
    for part in &modeler.session.model.parts {
        if part.verts.is_empty() || part.indices.is_empty() || !part.visible {
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

/// Preview materials of procedurally textured parts, by bake key, so a
/// scene rebuild (every model change) reuses the GPU textures.
#[derive(Resource, Default)]
pub(crate) struct PbrCache(std::collections::HashMap<u64, Handle<StandardMaterial>>);

/// Uploads one RGBA8 texture with a box-filtered mip chain and a repeating,
/// anisotropic sampler.
fn pbr_image(
    w: usize,
    h: usize,
    px: &[u8],
    srgb: bool,
    images: &mut Assets<Image>,
) -> Handle<Image> {
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    use bevy::render::render_resource::{Extent3d, TextureDimension};
    let (data, levels) = modeler::material::pbr::mips(w, h, px, srgb);
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
    if let Some(h) = cache.0.get(&key) {
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
    let [cr, cg, cb, ca] = part.material.base_color;
    let mut m = StandardMaterial {
        base_color: Color::srgba(cr, cg, cb, ca),
        perceptual_roughness: part.material.roughness,
        metallic: part.material.metalness,
        base_color_texture: Some(pbr_image(t.w, t.h, &t.base, true, images)),
        metallic_roughness_texture: Some(pbr_image(t.w, t.h, &t.metal_rough, false, images)),
        normal_map_texture: Some(pbr_image(t.w, t.h, &t.normal, false, images)),
        ..default()
    };
    if let Some(ao) = &t.occlusion {
        m.occlusion_texture = Some(pbr_image(t.w, t.h, ao, false, images));
    }
    if let Some(e) = &t.emissive {
        m.emissive_texture = Some(pbr_image(t.w, t.h, e, true, images));
        m.emissive = LinearRgba::WHITE;
    }
    if t.blend {
        m.alpha_mode = AlphaMode::Blend;
    }
    if cache.0.len() > 256 {
        cache.0.clear();
    }
    let h = materials.add(m);
    cache.0.insert(key, h.clone());
    Some(h)
}

pub(crate) fn part_bundle(
    part: &Part,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
) -> impl Bundle {
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
    let textured = pbr_material(part, materials, images, cache);
    if textured.is_some()
        && !part.normals.is_empty()
        && !part.uvs.is_empty()
        && let Err(e) = mesh.generate_tangents()
    {
        eprintln!("tangents for `{}`: {e}", part.name);
    }
    let material = textured.unwrap_or_else(|| {
        let [r, g, b, a] = part.material.base_color;
        materials.add(StandardMaterial {
            base_color: Color::srgba(r, g, b, a),
            perceptual_roughness: part.material.roughness,
            metallic: part.material.metalness,
            // A colour with alpha (glass, holograms) blends; Bevy renders
            // blended materials forward, over the deferred opaque pass.
            alpha_mode: if a < 0.999 {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            ..default()
        })
    });
    (
        Name::new(part.name.clone()),
        Mesh3d(meshes.add(mesh)),
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
                DirectionalLight {
                    illuminance: 500.0,
                    ..default()
                },
                Transform::default(),
            ));
        });
    commands.spawn((
        Name::new("sun"),
        DirectionalLight {
            illuminance: 8000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.5, 0.0)),
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
