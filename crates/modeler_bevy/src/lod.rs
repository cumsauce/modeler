//! Level-of-detail ranges from the exporter's `extras.lod`.
//!
//! `lods: [0.5, 0.2]` in `export_model` adds simplified copies of heavy
//! parts as sibling nodes, each with `extras.lod = {level, near, far}`
//! (camera distances in meters, `far` null for the last level). The plugin
//! turns that into a [`VisibilityRange`] on the node and on every mesh child
//! Bevy spawned for it, so exactly one level shows at a distance.

use bevy::camera::visibility::VisibilityRange;
use bevy::prelude::*;

/// A node's LOD level and distances, from `extras.lod`.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ModelerLod {
    pub level: u32,
    pub near: f32,
    /// `None` on the coarsest level (never hidden by distance).
    pub far: Option<f32>,
}

/// How levels hand over.
#[derive(Resource, Clone, Copy, Debug)]
pub struct ModelerLods {
    /// Meters of dithered crossfade centred on each switch distance
    /// (0 = an abrupt swap).
    pub crossfade: f32,
    /// Scales every distance (a game with a long view distance may set 2).
    pub distance_scale: f32,
    /// Off: leave `VisibilityRange` to the game ([`ModelerLod`] is still
    /// added).
    pub enabled: bool,
}

impl Default for ModelerLods {
    fn default() -> Self {
        Self {
            crossfade: 0.0,
            distance_scale: 1.0,
            enabled: true,
        }
    }
}

/// `extras.lod` as a component.
pub fn lod_of(v: &serde_json::Value) -> Option<ModelerLod> {
    let l = v.get("lod")?;
    Some(ModelerLod {
        level: l.get("level")?.as_u64()? as u32,
        near: l.get("near").and_then(|n| n.as_f64()).unwrap_or(0.0) as f32,
        far: l.get("far").and_then(|n| n.as_f64()).map(|n| n as f32),
    })
}

impl ModelerLod {
    /// The range for this level: abrupt, or crossfading by `crossfade`
    /// meters centred on the switch distances (a level's end margin equals
    /// the next level's start margin, as Bevy requires).
    pub fn range(&self, s: &ModelerLods) -> VisibilityRange {
        let half = s.crossfade * 0.5;
        let near = self.near * s.distance_scale;
        let start = if self.level == 0 || near <= 0.0 {
            0.0..0.0
        } else {
            (near - half).max(0.0)..(near + half)
        };
        let end = match self.far {
            Some(f) => {
                let f = f * s.distance_scale;
                (f - half).max(0.0)..(f + half)
            }
            None => f32::MAX..f32::MAX,
        };
        VisibilityRange {
            start_margin: start,
            end_margin: end,
            use_aabb: false,
        }
    }
}

#[derive(Component)]
pub(crate) struct LodRanged;

/// Puts the range on the node and on the mesh entities below it (Bevy
/// spawns a node's primitives as children unless there is only one).
#[allow(clippy::type_complexity)]
pub(crate) fn attach_lod_ranges(
    mut commands: Commands,
    settings: Res<ModelerLods>,
    nodes: Query<(Entity, &ModelerLod), Without<LodRanged>>,
    meshes: Query<(), With<Mesh3d>>,
    children: Query<&Children>,
) {
    if !settings.enabled {
        return;
    }
    for (e, lod) in &nodes {
        let targets: Vec<Entity> = std::iter::once(e)
            .chain(children.iter_descendants(e))
            .filter(|x| meshes.contains(*x))
            .collect();
        if targets.is_empty() {
            continue; // the scene's children have not spawned yet
        }
        let range = lod.range(&settings);
        for t in targets {
            commands.entity(t).insert(range.clone());
        }
        commands.entity(e).insert(LodRanged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_chain_levels() {
        let s = ModelerLods::default();
        let a = lod_of(&serde_json::json!({"lod": {"level": 0, "near": 0.0, "far": 20.0}}));
        let b = lod_of(&serde_json::json!({"lod": {"level": 1, "near": 20.0, "far": null}}));
        let (Some(a), Some(b)) = (a, b) else {
            panic!("lod")
        };
        let (ra, rb) = (a.range(&s), b.range(&s));
        assert_eq!(ra.end_margin, rb.start_margin);
        assert_eq!(ra.start_margin, 0.0..0.0);
        assert_eq!(rb.end_margin.start, f32::MAX);
        let c = ModelerLods {
            crossfade: 4.0,
            ..s
        };
        assert_eq!(a.range(&c).end_margin, 18.0..22.0);
        assert_eq!(a.range(&c).end_margin, b.range(&c).start_margin);
    }
}
