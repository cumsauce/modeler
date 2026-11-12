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

use bevy::app::AnimationSystems;
use bevy::gltf::GltfExtras;
use bevy::light::ClusteredDecal;
use bevy::pbr::decal::{ForwardDecal, ForwardDecalMaterial, ForwardDecalMaterialExt};
use bevy::prelude::*;
use bevy::transform::TransformSystems;
pub use modeler_anim::{Spring, SpringSim, SwayKind, SwayRig, Wind as WindSettings};

pub struct ModelerPlugin;

impl Plugin for ModelerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wind>()
            .init_resource::<ModelerDecals>()
            .add_systems(Update, make_decals)
            .add_systems(
                PostUpdate,
                (read_extras, run_sway)
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
    base: Vec<Quat>,
    written: Vec<Option<Quat>>,
    sim: SpringSim,
    out: Vec<Quat>,
}

impl Sway {
    pub fn new(rig: SwayRig, joints: Vec<Option<Entity>>, phase: f32) -> Self {
        let rest = rig
            .bones
            .iter()
            .map(|b| {
                let head = Vec3::from(b.head);
                match b.parent.and_then(|p| rig.bones.get(p as usize)) {
                    Some(p) => head - Vec3::from(p.head),
                    None => head,
                }
            })
            .collect();
        let n = rig.bones.len();
        Self {
            rig,
            joints,
            phase,
            live: true,
            rest,
            base: vec![Quat::IDENTITY; n],
            written: vec![None; n],
            sim: SpringSim::new(),
            out: Vec::new(),
        }
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
        let joints = rig
            .bones
            .iter()
            .map(|b| found.iter().find(|(n, _)| *n == b.name).map(|(_, x)| *x))
            .collect();
        commands
            .entity(e)
            .insert(Sway::new(rig, joints, phase_of(e)));
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
            anim.push((tf.translation - sway.rest[i], sway.base[i]));
        }
        match &sway.rig.kind {
            SwayKind::Wind(w) => {
                if wind.enabled {
                    let w = wind.apply(w);
                    sway.rig.wind_rotations(&w, t, sway.phase, &mut sway.out);
                    for (q, a) in sway.out.iter_mut().zip(&anim) {
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
