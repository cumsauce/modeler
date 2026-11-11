//! Lights: `light` items in the op graph, collected after evaluation into
//! world-space [`Light`]s for the preview and the glTF export
//! (KHR_lights_punctual). Prefab instances bring their prefab's lights
//! (named `instance.light`), and a light `attach`ed to a part follows that
//! part's moves and places.

use std::collections::HashMap;

use bevy::math::{Quat, Vec3};

use crate::{Model, ModelerError, Op, quat_of};

#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub name: String,
    /// point, spot or directional.
    pub kind: String,
    /// Linear RGB.
    pub color: [f32; 3],
    /// Lumens (point, spot) or lux (directional).
    pub intensity: f32,
    pub range: Option<f32>,
    pub position: Vec3,
    /// Unit aim (spot, directional).
    pub direction: Vec3,
    /// Spot half-angles in radians.
    pub inner: f32,
    pub outer: f32,
    pub shadows: bool,
}

impl Light {
    /// A rotation taking -Z (Bevy's and glTF's light forward) to `direction`.
    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_arc(Vec3::NEG_Z, self.direction.normalize_or(Vec3::NEG_Y))
    }
}

/// Default intensity per kind (lumens / lux).
pub fn default_intensity(kind: &str) -> f32 {
    if kind == "directional" {
        3000.0
    } else {
        2000.0
    }
}

/// Every light of the model in world space, after moves.
pub fn collect(model: &Model) -> Result<Vec<Light>, ModelerError> {
    let raw = unmoved(model, 0)?;
    if raw.is_empty() {
        return Ok(raw.into_iter().map(|(l, _)| l).collect());
    }
    // Each attached part's move delta: moved transform * unmoved^-1.
    let deltas = move_deltas(model)?;
    Ok(raw
        .into_iter()
        .map(|(mut l, attach)| {
            if let Some((r, t)) = attach.and_then(|a| deltas.get(&a).copied()) {
                l.position = r * l.position + t;
                l.direction = r * l.direction;
            }
            l
        })
        .collect())
}

/// Lights before moves, with the part each rides on (in this model's names).
fn unmoved(model: &Model, depth: u32) -> Result<Vec<(Light, Option<String>)>, ModelerError> {
    if depth > 8 {
        return Err("lights: prefabs nested more than 8 deep".into());
    }
    let mut out = Vec::new();
    for op in &model.ops {
        match op {
            Op::Light {
                name,
                kind,
                color,
                intensity,
                range,
                translation,
                direction,
                inner_degrees,
                outer_degrees,
                shadows,
                attach,
            } => {
                let outer = outer_degrees.unwrap_or(35.0).clamp(0.5, 89.0);
                let inner = inner_degrees.unwrap_or(outer * 0.8).clamp(0.0, outer);
                out.push((
                    Light {
                        name: name.clone(),
                        kind: kind.clone(),
                        color: [color[0], color[1], color[2]],
                        intensity: intensity.unwrap_or_else(|| default_intensity(kind)),
                        range: *range,
                        position: Vec3::from(*translation),
                        direction: Vec3::from(direction.unwrap_or([0.0, -1.0, 0.0])).normalize(),
                        inner: inner.to_radians(),
                        outer: outer.to_radians(),
                        shadows: *shadows,
                    },
                    attach.clone(),
                ));
            }
            Op::Prefab {
                name,
                prefab,
                translation,
                rotation,
                scale,
                ..
            } => {
                let Ok(src) = crate::catalog::load_prefab(prefab) else {
                    continue;
                };
                let (t, r) = (
                    Vec3::from(*translation),
                    quat_of(rotation.unwrap_or([0.0, 0.0, 0.0, 1.0])),
                );
                let s = Vec3::from(scale.unwrap_or([1.0; 3]));
                for (mut l, attach) in unmoved(&src, depth + 1)? {
                    l.name = format!("{name}.{}", l.name);
                    l.position = t + r * (s * l.position);
                    l.direction = (r * l.direction).normalize();
                    // Real units don't scale with the instance.
                    out.push((l, attach.map(|a| format!("{name}.{a}"))));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Per part: the rigid transform its moves and places applied.
pub(crate) fn move_deltas(model: &Model) -> Result<HashMap<String, (Quat, Vec3)>, ModelerError> {
    let mut map = HashMap::new();
    if !model.ops.iter().any(|o| matches!(o, Op::Move { .. })) {
        return Ok(map);
    }
    let still: Vec<Op> = model
        .ops
        .iter()
        .filter(|o| !matches!(o, Op::Move { .. }))
        .cloned()
        .collect();
    let before = crate::graph::evaluate(&still)?;
    for p in &model.parts {
        let Some(b) = before.iter().find(|q| q.name == p.name) else {
            continue;
        };
        let (rb, tb) = (quat_of(b.rotation), Vec3::from(b.translation));
        let (ra, ta) = (quat_of(p.rotation), Vec3::from(p.translation));
        // x_world = ra * (rb^-1 * (x - tb)) + ta
        let r = ra * rb.inverse();
        map.insert(p.name.clone(), (r, ta - r * tb));
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Session;
    use serde_json::json;

    fn session(items: serde_json::Value) -> Session {
        let mut s = Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("items must be an array");
        };
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn lights_follow_their_part_and_export() {
        let mut s = session(json!([
            {"type": "primitive", "name": "lamp", "kind": "box", "size": [0.2, 1.0, 0.2],
             "translation": [0, 0.5, 0]},
            {"type": "material", "name": "lamp", "color": "#888"},
            {"type": "light", "name": "bulb", "kind": "point", "translation": [0, 1.1, 0],
             "intensity": 800, "color": "#ffcc88", "attach": "lamp"},
            {"type": "light", "name": "sun", "kind": "directional", "direction": [0, -1, -1]},
            {"type": "light", "name": "spot", "kind": "spot", "translation": [1, 2, 0],
             "outer_degrees": 30, "shadows": true}
        ]));
        let ls = collect(&s.model).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ls.len(), 3);
        assert!((ls[0].position - Vec3::new(0.0, 1.1, 0.0)).length() < 1e-5);
        assert!((ls[1].direction - Vec3::new(0.0, -1.0, -1.0).normalize()).length() < 1e-5);
        assert!((ls[2].inner - 24f32.to_radians()).abs() < 1e-5);
        // Moving the lamp carries the bulb; the others stay.
        s.apply_values(
            None,
            vec![json!({"type": "move", "name": "shift", "parts": ["lamp"], "translation": [2, 0, 0]})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let ls = collect(&s.model).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            (ls[0].position - Vec3::new(2.0, 1.1, 0.0)).length() < 1e-4,
            "{:?}",
            ls[0].position
        );
        assert!((ls[2].position - Vec3::new(1.0, 2.0, 0.0)).length() < 1e-5);
        let err = s
            .apply_values(
                None,
                vec![json!({"type": "light", "name": "x", "kind": "area"})],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("kind"), "{err}");
        let (doc, _, report) = crate::gltf::build_with(
            &s.model,
            None,
            &crate::export_opt::Options::none(),
            None,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let lights = &doc["extensions"]["KHR_lights_punctual"]["lights"];
        assert_eq!(lights.as_array().map_or(0, Vec::len), 3);
        let cd = lights[0]["intensity"].as_f64().unwrap_or(0.0);
        assert!(
            (cd - 800.0 / (4.0 * std::f64::consts::PI)).abs() < 0.01,
            "{cd}"
        );
        let outer = lights[2]["spot"]["outerConeAngle"].as_f64().unwrap_or(0.0) as f32;
        assert!((outer - 30f32.to_radians()).abs() < 1e-5);
        assert!(report.contains("lights: 3"), "{report}");
        assert!(
            doc["extensionsUsed"]
                .to_string()
                .contains("KHR_lights_punctual")
        );
    }
}
