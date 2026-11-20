//! Bevy runtime for glTF files exported by the modeler.
//!
//! Add [`ModelerPlugin`] and spawn the exported scene as usual. When glTF
//! extras are loaded (Bevy's `GltfExtras` component, on by default), the
//! plugin reads them:
//!
//! - `extras.sway` on a skeleton's root node (from a `sway` item): a
//!   [`Sway`] component that runs the same wind or spring simulation the
//!   modeler baked, live, every frame. It runs after `AnimationPlayer`, so a
//!   spring tail layers over a keyframed walk. Each instance gets its own
//!   phase, so a forest doesn't move in lockstep. The [`Wind`] resource
//!   steers every rig at once. The file also holds the baked clip
//!   (`Sway::rig.clip`): play that clip *or* keep the live sway, not both.
//! - `extras.collider` becomes [`ModelerCollider`] and `extras.marker`
//!   becomes [`ModelerMarker`], for game code to map onto its physics engine
//!   and spawn points.
//! - `extras.decal` (a `decal` item) on a textured quad node: the plugin
//!   replaces the quad with a projected decal of the same textures, a
//!   [`ForwardDecal`] (any platform; the camera needs a `DepthPrepass`) or,
//!   for `kind: "clustered"` where [`ModelerDecals::clustered`] allows, a
//!   [`ClusteredDecal`] (needs bindless textures: not on Metal / WebGL).
//!   Without the plugin the quad still draws as a flat sticker.
//! - `extras.variants` (cosmetic variants, `variant` items) on every part
//!   node: a [`ModelerVariants`] component ({variant: glTF material
//!   index}). Insert [`ModelerVariant`] (`ModelerVariant::new("red_team")`)
//!   on the scene root, or any ancestor, and the plugin swaps each part's
//!   `MeshMaterial3d<StandardMaterial>` to that variant's material from the
//!   same glTF (Bevy's loader ignores `KHR_materials_variants`); change it
//!   to switch looks at runtime. A `.bsn` map writes it on prefab instances.
//! - `extras.variants_visible` (outfits: variants with `show` / `hide`)
//!   on the nodes of parts some variant shows or hides: a
//!   [`ModelerVariantVisibility`] component ({variant: shown}); the same
//!   [`ModelerVariant`] sets each such node's `Visibility`.

use bevy::app::AnimationSystems;
use bevy::gltf::GltfExtras;
use bevy::light::ClusteredDecal;
use bevy::mesh::morph::MorphWeights;
use bevy::pbr::decal::{ForwardDecal, ForwardDecalMaterial, ForwardDecalMaterialExt};
use bevy::prelude::*;
use bevy::transform::TransformSystems;
pub use modeler_anim::{
    CorrectiveDriver, Spring, SpringSim, SwayKind, SwayRig, Wind as WindSettings, conj, unconj,
};

pub struct ModelerPlugin;

impl Plugin for ModelerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wind>()
            .init_resource::<ModelerDecals>()
            .register_type::<ModelerVariant>()
            .add_systems(
                Update,
                (make_decals, apply_variants, apply_variant_visibility),
            )
            .add_systems(
                PostUpdate,
                (read_extras, run_sway, drive_correctives)
                    .chain()
                    .after(AnimationSystems)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// Live control of every swaying rig. `None` fields keep each file's own
/// settings.
#[derive(Resource, Clone, Debug)]
pub struct Wind {
    pub enabled: bool,
    /// Where the wind blows to (horizontal part).
    pub direction: Option<Vec3>,
    /// 0 still, 0.5 a breeze, 1 strong, 2 a gale.
    pub strength: Option<f32>,
    pub gust: Option<f32>,
}

impl Default for Wind {
    fn default() -> Self {
        Self {
            enabled: true,
            direction: None,
            strength: None,
            gust: None,
        }
    }
}

impl Wind {
    fn apply(&self, w: &WindSettings) -> WindSettings {
        let mut w = w.clone();
        if let Some(d) = self.direction {
            w.direction = d.to_array();
        }
        if let Some(s) = self.strength {
            w.strength = s;
        }
        if let Some(g) = self.gust {
            w.gust = g;
        }
        w
    }
}

/// A rig that sways live. Inserted on the skeleton's root node.
#[derive(Component)]
pub struct Sway {
    pub rig: SwayRig,
    /// The joint entity of each bone (`None` if the scene lacked it).
    pub joints: Vec<Option<Entity>>,
    /// Seconds added to the clock for this instance.
    pub phase: f32,
    /// Off: the plugin leaves the joints alone (e.g. while the baked clip
    /// plays instead).
    pub live: bool,
    rest: Vec<Vec3>,
    /// Joint rest rotations relative to the parent joint, and the world rest
    /// rotation of each joint's parent frame; both empty when no joint has a
    /// rest rotation (the common case: world-aligned bones).
    rest_rot: Vec<Quat>,
    parent_rest: Vec<Quat>,
    base: Vec<Quat>,
    written: Vec<Option<Quat>>,
    sim: SpringSim,
    out: Vec<Quat>,
}

impl Sway {
    pub fn new(rig: SwayRig, joints: Vec<Option<Entity>>, phase: f32) -> Self {
        let rest = Self::head_offsets(&rig);
        let n = rig.bones.len();
        Self {
            rig,
            joints,
            phase,
            live: true,
            rest,
            rest_rot: Vec::new(),
            parent_rest: Vec::new(),
            base: vec![Quat::IDENTITY; n],
            written: vec![None; n],
            sim: SpringSim::new(),
            out: Vec::new(),
        }
    }

    /// Tells the sway each joint's rest rotation relative to its parent
    /// joint (the joint's rest `Transform::rotation`; identity for bones
    /// without one). Bones with rest rotations are simulated in world-aligned
    /// frames and converted into the joints' frames, as the modeler's baked
    /// sway does. All identity (or empty) keeps the plain path.
    pub fn set_rest_rotations(&mut self, local: &[Quat]) {
        if local.iter().all(|q| *q == Quat::IDENTITY) {
            self.rest_rot.clear();
            self.parent_rest.clear();
            self.rest = Self::head_offsets(&self.rig);
            return;
        }
        let n = self.rig.bones.len();
        self.rest_rot = (0..n)
            .map(|i| local.get(i).copied().unwrap_or(Quat::IDENTITY))
            .collect();
        self.parent_rest = self.rig.rest_parent_frames(&self.rest_rot);
        // A joint's rest translation is its head offset in the parent's frame.
        let offsets = Self::head_offsets(&self.rig);
        self.rest = offsets
            .iter()
            .zip(&self.parent_rest)
            .map(|(d, p)| p.inverse() * *d)
            .collect();
    }

    fn head_offsets(rig: &SwayRig) -> Vec<Vec3> {
        rig.bones
            .iter()
            .map(|b| {
                let head = Vec3::from(b.head);
                match b.parent.and_then(|p| rig.bones.get(p as usize)) {
                    Some(p) => head - Vec3::from(p.head),
                    None => head,
                }
            })
            .collect()
    }
}

/// Collision shape exported for a part (`collider` items).
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ModelerCollider {
    pub shape: ColliderShape,
    /// "static", "dynamic" or "kinematic".
    pub body: String,
    pub sensor: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColliderShape {
    Cuboid {
        half_extents: Vec3,
    },
    ConvexHull {
        points: Vec<Vec3>,
    },
    /// The node's own mesh.
    Trimesh,
}

/// How exported decals are drawn.
#[derive(Resource, Clone, Debug)]
pub struct ModelerDecals {
    /// Draw `kind: "clustered"` decals as [`ClusteredDecal`]s. Off on macOS
    /// (Bevy disables them on Metal) and the web; forward decals otherwise.
    pub clustered: bool,
}

impl Default for ModelerDecals {
    fn default() -> Self {
        Self {
            clustered: !cfg!(any(
                target_os = "macos",
                target_os = "ios",
                target_arch = "wasm32"
            )),
        }
    }
}

/// A decal exported by the modeler (`extras.decal`), before and after the
/// plugin turned its quad into a projected decal.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ModelerDecal {
    /// Width, height (the image) and depth (the projection), meters.
    pub size: Vec3,
    /// "clustered" or "forward".
    pub kind: String,
}

/// Parses a node's decal extras.
pub fn decal_of(v: &serde_json::Value) -> Option<ModelerDecal> {
    let d = v.get("decal")?;
    Some(ModelerDecal {
        size: vec3(d.get("size")?)?,
        kind: d
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("clustered")
            .to_string(),
    })
}

#[derive(Component)]
struct DecalDone;

/// Swaps each decal node's quad for a projected decal once its material
/// has loaded. The node's frame has +Z into the surface and +Y the image's
/// up, centred on the surface.
fn make_decals(
    mut commands: Commands,
    decals: Query<(Entity, &ModelerDecal, Option<&Children>), Without<DecalDone>>,
    quads: Query<&MeshMaterial3d<StandardMaterial>>,
    std_mats: Option<Res<Assets<StandardMaterial>>>,
    fwd_mats: Option<ResMut<Assets<ForwardDecalMaterial<StandardMaterial>>>>,
    settings: Res<ModelerDecals>,
) {
    // Without a renderer (headless tests) there is nothing to draw.
    let (Some(std_mats), Some(mut fwd_mats)) = (std_mats, fwd_mats) else {
        return;
    };
    for (e, d, children) in &decals {
        // Bevy puts a node's mesh on a child entity (one per primitive).
        let Some((quad, mat)) = children
            .into_iter()
            .flatten()
            .chain(std::iter::once(&e))
            .find_map(|c| quads.get(*c).ok().map(|m| (*c, m)))
        else {
            continue;
        };
        let Some(m) = std_mats.get(&mat.0) else {
            continue;
        };
        let mut ec = commands.spawn(ChildOf(e));
        if d.kind == "clustered" && settings.clustered {
            ec.insert((
                ClusteredDecal {
                    base_color_texture: m.base_color_texture.clone(),
                    normal_map_texture: m.normal_map_texture.clone(),
                    metallic_roughness_texture: m.metallic_roughness_texture.clone(),
                    emissive_texture: m.emissive_texture.clone(),
                    tag: 0,
                },
                // The clustered box projects along -Z, u along +x, v down +y.
                Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI))
                    .with_scale(d.size),
            ));
        } else {
            let mut base = m.clone();
            base.alpha_mode = AlphaMode::Blend;
            ec.insert((
                ForwardDecal,
                MeshMaterial3d(fwd_mats.add(ForwardDecalMaterial {
                    base,
                    extension: ForwardDecalMaterialExt {
                        depth_fade_factor: (d.size.z * 0.5).max(0.01),
                    },
                })),
                // The forward quad lies in local xz projecting down -y.
                Transform::from_rotation(
                    Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)
                        * Quat::from_rotation_y(std::f32::consts::PI),
                )
                .with_scale(Vec3::new(d.size.x, d.size.z, d.size.y)),
            ));
        }
        if quad == e {
            commands
                .entity(e)
                .remove::<(Mesh3d, MeshMaterial3d<StandardMaterial>)>();
        } else {
            commands.entity(quad).despawn();
        }
        commands.entity(e).insert(DecalDone);
    }
}

/// A named point for game code (`marker` items): spawns, waypoints, zones.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ModelerMarker {
    pub kind: String,
    pub size: Option<Vec3>,
    pub data: serde_json::Value,
}

fn vec3(v: &serde_json::Value) -> Option<Vec3> {
    Some(Vec3::new(
        v.get(0)?.as_f64()? as f32,
        v.get(1)?.as_f64()? as f32,
        v.get(2)?.as_f64()? as f32,
    ))
}

/// Parses a node's collider extras.
pub fn collider_of(v: &serde_json::Value) -> Option<ModelerCollider> {
    let c = v.get("collider")?;
    let shape = match c.get("shape")?.as_str()? {
        "cuboid" => ColliderShape::Cuboid {
            half_extents: vec3(c.get("half_extents")?)?,
        },
        "convex_hull" => ColliderShape::ConvexHull {
            points: c
                .get("points")?
                .as_array()?
                .iter()
                .filter_map(vec3)
                .collect(),
        },
        _ => ColliderShape::Trimesh,
    };
    Some(ModelerCollider {
        shape,
        body: c
            .get("body")
            .and_then(|b| b.as_str())
            .unwrap_or("static")
            .to_string(),
        sensor: c.get("sensor").and_then(|s| s.as_bool()).unwrap_or(false),
    })
}

/// Parses a node's marker extras.
pub fn marker_of(v: &serde_json::Value) -> Option<ModelerMarker> {
    Some(ModelerMarker {
        kind: v.get("marker")?.as_str()?.to_string(),
        size: v.get("size").and_then(vec3),
        data: v.get("data").cloned().unwrap_or(serde_json::Value::Null),
    })
}

/// A part's cosmetic variants: variant name -> glTF material index (from
/// `extras.variants`; "default" is the base look).
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct ModelerVariants(pub std::collections::BTreeMap<String, usize>);

impl ModelerVariants {
    /// The material index for `variant`, falling back to "default".
    pub fn material(&self, variant: &str) -> Option<usize> {
        self.0
            .get(variant)
            .or_else(|| self.0.get("default"))
            .copied()
    }
}

/// The cosmetic variant an exported model wears. Put it on the scene root
/// (or any ancestor of the parts); every part below switches to that
/// variant's material, and changing it switches again.
#[derive(Component, Clone, Debug, Default, PartialEq, Reflect)]
#[reflect(Component, Default)]
pub struct ModelerVariant {
    pub name: String,
}

impl ModelerVariant {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// `extras.variants` as a component.
pub fn variants_of(v: &serde_json::Value) -> Option<ModelerVariants> {
    let m = v.get("variants")?.as_object()?;
    Some(ModelerVariants(
        m.iter()
            .filter_map(|(k, i)| Some((k.clone(), i.as_u64()? as usize)))
            .collect(),
    ))
}

/// Outfits: whether a part is shown in each variant (from
/// `extras.variants_visible`; "default" is the base model). Parts no
/// variant shows or hides have none.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct ModelerVariantVisibility(pub std::collections::BTreeMap<String, bool>);

impl ModelerVariantVisibility {
    /// Whether the part shows in `variant` (falling back to "default",
    /// then visible).
    pub fn visible(&self, variant: &str) -> bool {
        self.0
            .get(variant)
            .or_else(|| self.0.get("default"))
            .copied()
            .unwrap_or(true)
    }
}

/// `extras.variants_visible` as a component.
pub fn variant_visibility_of(v: &serde_json::Value) -> Option<ModelerVariantVisibility> {
    let m = v.get("variants_visible")?.as_object()?;
    Some(ModelerVariantVisibility(
        m.iter()
            .filter_map(|(k, b)| Some((k.clone(), b.as_bool()?)))
            .collect(),
    ))
}

/// Shows or hides outfit parts for the [`ModelerVariant`] above them (the
/// default variant when there is none).
fn apply_variant_visibility(
    mut parts: Query<(Entity, &ModelerVariantVisibility, &mut Visibility)>,
    chosen: Query<&ModelerVariant>,
    parents: Query<&ChildOf>,
) {
    for (e, vis, mut v) in &mut parts {
        let name = std::iter::once(e)
            .chain(parents.iter_ancestors(e))
            .find_map(|a| chosen.get(a).ok())
            .map_or("default", |c| c.name.as_str());
        let want = if vis.visible(name) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
}

/// The label of glTF material `index`'s `StandardMaterial` (Bevy's loader
/// adds one per material, used by a primitive or not).
pub fn variant_material_label(index: usize) -> String {
    format!("Material{index}/std")
}

/// Swaps part materials to the chosen variant: for parts under a changed
/// [`ModelerVariant`], and for parts that just spawned under one.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn apply_variants(
    assets: Option<Res<AssetServer>>,
    changed: Query<(Entity, &ModelerVariant), Changed<ModelerVariant>>,
    added: Query<Entity, Added<ModelerVariants>>,
    chosen: Query<&ModelerVariant>,
    parts: Query<&ModelerVariants>,
    parents: Query<&ChildOf>,
    children: Query<&Children>,
    mut mats: Query<&mut MeshMaterial3d<StandardMaterial>>,
) {
    // Without an asset server (headless tests) there is nothing to swap.
    let Some(assets) = assets else { return };
    let mut todo: Vec<(Entity, String)> = Vec::new();
    for (root, v) in &changed {
        for e in std::iter::once(root).chain(children.iter_descendants(root)) {
            if parts.contains(e) {
                todo.push((e, v.name.clone()));
            }
        }
    }
    for e in &added {
        if let Some(v) = std::iter::once(e)
            .chain(parents.iter_ancestors(e))
            .find_map(|a| chosen.get(a).ok())
        {
            todo.push((e, v.name.clone()));
        }
    }
    for (node, variant) in todo {
        let Some(index) = parts.get(node).ok().and_then(|p| p.material(&variant)) else {
            continue;
        };
        // The node's own mesh, or its primitives (children).
        let targets =
            std::iter::once(node).chain(children.get(node).into_iter().flatten().copied());
        for t in targets {
            let Ok(mut m) = mats.get_mut(t) else {
                continue;
            };
            let Some(path) = assets.get_path(m.0.id()) else {
                continue;
            };
            let want = path
                .without_label()
                .clone_owned()
                .with_label(variant_material_label(index));
            if assets.get_path(m.0.id()).as_ref() != Some(&want) {
                m.0 = assets.load(want);
            }
        }
    }
}

/// Pose-driven corrective morph targets of a skinned mesh node (from its
/// `extras.correctives`): every frame, after animation and sway,
/// [`drive_correctives`] sets their `MorphWeights` from the joints' local
/// rotations, so the mesh deforms as in the modeler. Targets the clip keys
/// (a human's `shapes`) keep their own slots: only the driven indices are
/// written.
#[derive(Component, Clone, Debug)]
pub struct ModelerCorrectives {
    pub drivers: Vec<CorrectiveDriver>,
    /// Each driver's joint entity, found on the first frame.
    joints: Option<Vec<Option<Entity>>>,
}

impl ModelerCorrectives {
    pub fn new(drivers: Vec<CorrectiveDriver>) -> Self {
        Self {
            drivers,
            joints: None,
        }
    }
}

/// `extras.correctives` of a mesh node, if any.
pub fn correctives_of(v: &serde_json::Value) -> Option<ModelerCorrectives> {
    let d: Vec<CorrectiveDriver> = serde_json::from_value(v.get("correctives")?.clone()).ok()?;
    (!d.is_empty()).then(|| ModelerCorrectives::new(d))
}

/// Sets the driven morph weights (see [`ModelerCorrectives`]).
pub fn drive_correctives(
    mut q: Query<(Entity, &mut ModelerCorrectives, Option<&mut MorphWeights>)>,
    children: Query<&Children>,
    skinned: Query<&bevy::mesh::skinning::SkinnedMesh>,
    names: Query<&Name>,
    transforms: Query<&Transform>,
) {
    for (e, mut c, w) in &mut q {
        let Some(mut w) = w else { continue };
        if c.joints.is_none() {
            // The joints of the skin on this node or a primitive child.
            let mut stack = vec![e];
            let mut joints: Vec<Entity> = Vec::new();
            while let Some(x) = stack.pop() {
                if let Ok(s) = skinned.get(x) {
                    joints.extend(s.joints.iter().copied());
                    break;
                }
                if let Ok(ch) = children.get(x) {
                    stack.extend(ch.iter());
                }
            }
            if joints.is_empty() {
                continue; // not spawned yet
            }
            let found = c
                .drivers
                .iter()
                .map(|d| {
                    joints
                        .iter()
                        .copied()
                        .find(|j| names.get(*j).is_ok_and(|n| n.as_str() == d.bone))
                })
                .collect();
            c.joints = Some(found);
        }
        let c = &*c;
        let weights = w.weights_mut();
        for (d, j) in c.drivers.iter().zip(c.joints.iter().flatten()) {
            let Some(tf) = j.and_then(|j| transforms.get(j).ok()) else {
                continue;
            };
            if let Some(x) = weights.get_mut(d.target) {
                *x = d.weight(tf.rotation);
            }
        }
    }
}

/// A per-instance clock offset from the entity, in 0..17 s.
fn phase_of(e: Entity) -> f32 {
    let mut x = e.to_bits().wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 29;
    (x % 1000) as f32 / 1000.0 * 17.0
}

fn read_extras(
    mut commands: Commands,
    added: Query<(Entity, &GltfExtras), Added<GltfExtras>>,
    children: Query<&Children>,
    names: Query<&Name>,
    local: Query<&Transform>,
) {
    for (e, extras) in &added {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&extras.value) else {
            continue;
        };
        if let Some(c) = collider_of(&v) {
            commands.entity(e).insert(c);
        }
        if let Some(m) = marker_of(&v) {
            commands.entity(e).insert(m);
        }
        if let Some(d) = decal_of(&v) {
            commands.entity(e).insert(d);
        }
        if let Some(vs) = variants_of(&v) {
            commands.entity(e).insert(vs);
        }
        if let Some(vv) = variant_visibility_of(&v) {
            commands.entity(e).insert(vv);
        }
        if let Some(c) = correctives_of(&v) {
            commands.entity(e).insert(c);
        }
        let Some(rig) = v
            .get("sway")
            .and_then(|s| serde_json::from_value::<SwayRig>(s.clone()).ok())
        else {
            continue;
        };
        // The joints are this node's descendants, by name.
        let mut found: Vec<(String, Entity)> = Vec::new();
        let mut stack = vec![e];
        while let Some(x) = stack.pop() {
            if let Ok(n) = names.get(x) {
                found.push((n.as_str().to_string(), x));
            }
            if let Ok(c) = children.get(x) {
                stack.extend(c.iter());
            }
        }
        let joints: Vec<Option<Entity>> = rig
            .bones
            .iter()
            .map(|b| found.iter().find(|(n, _)| *n == b.name).map(|(_, x)| *x))
            .collect();
        // Rest rotations: the joints' own rotations as spawned.
        let rest_rot: Vec<Quat> = joints
            .iter()
            .map(|j| {
                j.and_then(|j| local.get(j).ok())
                    .map_or(Quat::IDENTITY, |t| t.rotation)
            })
            .collect();
        let mut sway = Sway::new(rig, joints, phase_of(e));
        sway.set_rest_rotations(&rest_rot);
        commands.entity(e).insert(sway);
    }
}

fn run_sway(
    time: Res<Time>,
    wind: Res<Wind>,
    mut rigs: Query<&mut Sway>,
    mut transforms: Query<&mut Transform>,
) {
    let t = time.elapsed_secs();
    let dt = time.delta_secs().min(0.1);
    for mut sway in &mut rigs {
        if !sway.live {
            continue;
        }
        let sway = &mut *sway;
        let n = sway.rig.bones.len();
        // The animated pose underneath: what an AnimationPlayer wrote this
        // frame, else the pose before the sway's own last write.
        let mut anim: Vec<(Vec3, Quat)> = Vec::with_capacity(n);
        for i in 0..n {
            let Some(tf) = sway.joints[i].and_then(|j| transforms.get(j).ok()) else {
                anim.push((Vec3::ZERO, sway.base[i]));
                continue;
            };
            if sway.written[i] != Some(tf.rotation) {
                sway.base[i] = tf.rotation;
            }
            if sway.parent_rest.is_empty() {
                anim.push((tf.translation - sway.rest[i], sway.base[i]));
            } else {
                // World-aligned frames: offset and rotation seen from the
                // world rest orientation (see `modeler_anim::conj`).
                let p = sway.parent_rest[i];
                let rl = sway.rest_rot[i];
                let spring = matches!(sway.rig.kind, SwayKind::Spring(_));
                let rot = if spring {
                    conj(p, sway.base[i] * rl.inverse())
                } else {
                    sway.base[i]
                };
                anim.push((p * (tf.translation - sway.rest[i]), rot));
            }
        }
        match &sway.rig.kind {
            SwayKind::Wind(w) => {
                if wind.enabled {
                    let w = wind.apply(w);
                    sway.rig.wind_rotations(&w, t, sway.phase, &mut sway.out);
                    for (i, (q, a)) in sway.out.iter_mut().zip(&anim).enumerate() {
                        if let Some(p) = sway.parent_rest.get(i) {
                            *q = unconj(*p, *q);
                        }
                        *q = (*q * a.1).normalize();
                    }
                } else {
                    sway.out = anim.iter().map(|a| a.1).collect();
                }
            }
            SwayKind::Spring(s) => {
                let mut s = s.clone();
                if let Some(w) = &s.wind {
                    s.wind = wind.enabled.then(|| wind.apply(w));
                }
                sway.sim.step(
                    &sway.rig,
                    &s,
                    &anim,
                    t + sway.phase,
                    dt.max(1e-4),
                    &mut sway.out,
                );
                // Back from world-aligned frames into the joints' own.
                for (i, q) in sway.out.iter_mut().enumerate() {
                    if let (Some(p), Some(rl)) = (sway.parent_rest.get(i), sway.rest_rot.get(i)) {
                        *q = (unconj(*p, *q) * *rl).normalize();
                    }
                }
            }
        }
        for i in 0..n {
            let (Some(j), Some(q)) = (sway.joints[i], sway.out.get(i).copied()) else {
                continue;
            };
            if let Ok(mut tf) = transforms.get_mut(j) {
                tf.rotation = q;
                sway.written[i] = Some(q);
            }
        }
    }
}

#[cfg(test)]
mod variant_tests {
    use super::*;

    #[test]
    fn variants_come_from_extras_and_fall_back_to_default() {
        let v = serde_json::json!({"part": "body", "variants": {"default": 0, "red_team": 3}});
        let Some(vs) = variants_of(&v) else {
            panic!("no variants read")
        };
        assert_eq!(vs.material("red_team"), Some(3));
        assert_eq!(vs.material("blue_team"), Some(0));
        assert!(variants_of(&serde_json::json!({"part": "x"})).is_none());
        let vv = variant_visibility_of(
            &serde_json::json!({"variants_visible": {"default": false, "casual": true}}),
        );
        assert!(
            vv.as_ref()
                .is_some_and(|v| !v.visible("default") && v.visible("casual"))
        );
        assert!(vv.is_some_and(|v| !v.visible("other")));
        assert_eq!(variant_material_label(3), "Material3/std");
    }
}

#[cfg(test)]
mod corrective_tests {
    use super::*;

    #[test]
    fn correctives_come_from_extras() {
        let v = serde_json::json!({"part": "eve", "correctives": [
            {"target": 3, "name": "fix_elbow_l_x+120", "bone": "elbow_l", "axis": [1, 0, 0],
             "degrees": 120, "from": 80, "to": null, "rest_local": [0, 0, 0, 1]}]});
        let Some(c) = correctives_of(&v) else {
            panic!("no correctives read")
        };
        assert_eq!(c.drivers.len(), 1);
        assert_eq!(c.drivers[0].target, 3);
        let q = Quat::from_rotation_x(100f32.to_radians());
        assert!((c.drivers[0].weight(q) - 0.5).abs() < 1e-3);
        assert!(correctives_of(&serde_json::json!({"part": "x"})).is_none());
    }
}
