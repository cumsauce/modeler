//! Decals in the preview: each `decal` item becomes a Bevy [`ForwardDecal`]
//! (a box projecting its baked material onto the surface under it), in the
//! window and in agent views. Clustered decals need bindless textures, which
//! Bevy turns off on Metal, so the preview always draws forward decals; the
//! export keeps the item's kind for the game to choose.

use bevy::camera::visibility::RenderLayers;
use bevy::pbr::decal::{ForwardDecal, ForwardDecalMaterial, ForwardDecalMaterialExt};
use bevy::pbr::{ExtendedMaterial, MeshMaterial3d};

use super::app::{PbrCache, pbr_image};
use crate::prelude::*;

#[derive(Component)]
pub(crate) struct SceneDecal;

#[derive(Clone)]
pub(crate) struct DecalTextures {
    base: Handle<Image>,
    normal: Handle<Image>,
    metal_rough: Handle<Image>,
    emissive: Option<Handle<Image>>,
}

fn textures(
    r: &modeler::material::PbrRef,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
) -> Option<DecalTextures> {
    let key = modeler::material::pbr::bake_key(r, None);
    if let Some(t) = cache.decals.get(&key) {
        return Some(t.clone());
    }
    let ch = match modeler::material::pbr::bake_ref(r) {
        Ok(ch) => ch,
        Err(e) => {
            eprintln!("decal: {e}");
            return None;
        }
    };
    let t = modeler::material::pbr::textures(&ch);
    let out = DecalTextures {
        base: pbr_image(t.w, t.h, &t.base, true, None, images),
        normal: pbr_image(t.w, t.h, &t.normal, false, None, images),
        metal_rough: pbr_image(t.w, t.h, &t.metal_rough, false, None, images),
        emissive: t
            .emissive
            .as_ref()
            .map(|e| pbr_image(t.w, t.h, e, true, None, images)),
    };
    cache.decals.insert(key, out.clone());
    Some(out)
}

/// Spawns the model's decals (on `layer` when given: agent-view stand-ins),
/// returning the entities.
pub(crate) fn spawn_decals(
    commands: &mut Commands,
    model: &modeler::Model,
    images: &mut Assets<Image>,
    cache: &mut PbrCache,
    mats: &mut Assets<ForwardDecalMaterial<StandardMaterial>>,
    layer: Option<usize>,
) -> Vec<Entity> {
    let decals = match modeler::decal::collect(model) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("decals: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for d in decals {
        let Some(t) = textures(&d.pbr, images, cache) else {
            continue;
        };
        let mut e = commands.spawn((
            Name::new(format!("decal {}", d.name)),
            SceneDecal,
            ForwardDecal,
            MeshMaterial3d(mats.add(ExtendedMaterial {
                base: StandardMaterial {
                    base_color_texture: Some(t.base),
                    normal_map_texture: Some(t.normal),
                    metallic_roughness_texture: Some(t.metal_rough),
                    emissive: if t.emissive.is_some() {
                        LinearRgba::WHITE
                    } else {
                        LinearRgba::BLACK
                    },
                    emissive_texture: t.emissive,
                    perceptual_roughness: 1.0,
                    metallic: 1.0,
                    alpha_mode: AlphaMode::Blend,
                    ..default()
                },
                extension: ForwardDecalMaterialExt {
                    depth_fade_factor: (d.size.z * 0.5).max(0.01),
                },
            })),
            // The quad lies in local xz projecting down -y; our frame
            // projects along +z with image up = +y; the quad's v runs along
            // local +z and u along +x, so turn local -y onto +z and flip both.
            Transform {
                translation: d.position,
                rotation: d.rotation
                    * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)
                    * Quat::from_rotation_y(std::f32::consts::PI),
                scale: Vec3::new(d.size.x, d.size.z, d.size.y),
            },
        ));
        if let Some(l) = layer {
            e.insert(RenderLayers::layer(l));
        }
        out.push(e.id());
    }
    out
}
