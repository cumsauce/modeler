//! Decals: `decal` items in the op graph, collected after evaluation into
//! world-space [`Decal`]s for the preview (Bevy clustered decals), the glTF
//! export (a node with `extras.decal`) and map scenes. A decal is a box
//! that projects its material onto whatever is inside it, so it follows
//! curved and uneven surfaces with no geometry of its own. Prefab instances
//! bring their prefab's decals (`instance.decal`); `on` sticks a decal to a
//! part's surface, `attach` makes it ride a part's moves.

use bevy::math::{Quat, Vec3};

use crate::{Model, ModelerError, Op, quat_of};

#[derive(Clone, Debug)]
pub struct Decal {
    pub name: String,
    pub pbr: crate::material::PbrRef,
    /// Width, height (the image on the surface) and depth (along the
    /// projection), meters.
    pub size: Vec3,
    /// The box's centre.
    pub position: Vec3,
    /// Local +Z is the projection direction (into the surface), +Y the
    /// image's up.
    pub rotation: Quat,
    pub kind: String,
}

impl Decal {
    pub fn direction(&self) -> Vec3 {
        self.rotation * Vec3::Z
    }
}

/// The travel direction for `toward` (as `place` names them).
pub fn toward_dir(t: &str) -> Option<Vec3> {
    Some(match t {
        "down" => Vec3::NEG_Y,
        "up" => Vec3::Y,
        "back" => Vec3::NEG_Z,
        "front" => Vec3::Z,
        "left" => Vec3::NEG_X,
        "right" => Vec3::X,
        _ => return None,
    })
}

/// The rotation taking +Z to `dir` with the image upright: on walls its up
/// is world up; on floors and ceilings it points to the back (-z).
pub fn orient(dir: Vec3, roll_degrees: f32) -> Quat {
    let z = dir.normalize_or(Vec3::NEG_Y);
    let hint = if z.y.abs() > 0.9 {
        Vec3::NEG_Z
    } else {
        Vec3::Y
    };
    let y = (hint - z * hint.dot(z)).normalize_or(Vec3::Y);
    let x = y.cross(z);
    let q = Quat::from_mat3(&bevy::math::Mat3::from_cols(x, y, z));
    q * Quat::from_rotation_z(roll_degrees.to_radians())
}

/// Every decal of the model in world space, after moves.
pub fn collect(model: &Model) -> Result<Vec<Decal>, ModelerError> {
    let raw = unmoved(model, 0)?;
    if raw.iter().all(|(_, a)| a.is_none()) {
        return Ok(raw.into_iter().map(|(d, _)| d).collect());
    }
    let deltas = crate::light::move_deltas(model)?;
    Ok(raw
        .into_iter()
        .map(|(mut d, attach)| {
            if let Some((r, t)) = attach.and_then(|a| deltas.get(&a).copied()) {
                d.position = r * d.position + t;
                d.rotation = r * d.rotation;
            }
            d
        })
        .collect())
}

/// Where a decal stuck `on` a part lands: a ray along `dir` through the
/// cross coordinates `at` (default the part's centre) hits the part's
/// surface. Errors say when it misses.
fn stick(
    model: &Model,
    name: &str,
    on: &str,
    toward: &str,
    at: Option<[f32; 2]>,
) -> Result<Vec3, ModelerError> {
    let dir = toward_dir(toward).ok_or_else(|| {
        ModelerError::from(format!(
            "decal `{name}`: toward `{toward}`: use down, up, left, right, back or front"
        ))
    })?;
    let part =
        model.parts.iter().find(|p| p.name == on).ok_or_else(|| {
            ModelerError::from(format!("decal `{name}`: no part `{on}` to stick to"))
        })?;
    let (min, max) = part.bounds();
    let centre = (min + max) * 0.5;
    // The travel axis and the two cross axes `at` names (place's order).
    let (k, (u, w)) = match toward {
        "down" | "up" => (1, (0, 2)),
        "back" | "front" => (2, (0, 1)),
        _ => (0, (2, 1)),
    };
    let mut o = centre;
    if let Some([a, b]) = at {
        o[u] = a;
        o[w] = b;
    }
    // Start outside the part, against the travel direction.
    let reach = (max - min).length() + 1.0;
    o[k] = centre[k] - dir[k] * reach;
    let t = crate::contact::ray_part(part, o, dir).ok_or_else(|| {
        ModelerError::from(format!(
            "decal `{name}`: going {toward} at {:?} misses `{on}` (it spans {:.2}..{:.2} and {:.2}..{:.2} on the cross axes)",
            at.unwrap_or([o[u], o[w]]),
            min[u],
            max[u],
            min[w],
            max[w]
        ))
    })?;
    Ok(o + dir * t)
}

/// Decals before moves, with the part each rides on.
fn unmoved(model: &Model, depth: u32) -> Result<Vec<(Decal, Option<String>)>, ModelerError> {
    if depth > 8 {
        return Err("decals: prefabs nested more than 8 deep".into());
    }
    let mut out = Vec::new();
    for op in &model.ops {
        match op {
            Op::Decal {
                name,
                pbr,
                size,
                translation,
                direction,
                roll,
                on,
                at,
                toward,
                attach,
                kind,
            } => {
                let (position, dir, attach) = match on {
                    Some(on) => {
                        let t = toward.as_deref().unwrap_or("down");
                        let p = stick(model, name, on, t, *at)?;
                        // On a part: its moves are already in the hit.
                        (p, toward_dir(t).unwrap_or(Vec3::NEG_Y), None)
                    }
                    None => (
                        Vec3::from(*translation),
                        Vec3::from(direction.unwrap_or([0.0, -1.0, 0.0])),
                        attach.clone(),
                    ),
                };
                out.push((
                    Decal {
                        name: name.clone(),
                        pbr: pbr.clone(),
                        size: Vec3::from(*size),
                        position,
                        rotation: orient(dir, *roll),
                        kind: kind.clone(),
                    },
                    attach,
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
                let Ok(src) = crate::catalog::prefab_instance(prefab) else {
                    continue;
                };
                let (t, r) = (
                    Vec3::from(*translation),
                    quat_of(rotation.unwrap_or([0.0, 0.0, 0.0, 1.0])),
                );
                let s = Vec3::from(scale.unwrap_or([1.0; 3]));
                for (mut d, attach) in unmoved(&src, depth + 1)? {
                    d.name = format!("{name}.{}", d.name);
                    d.position = t + r * (s * d.position);
                    d.rotation = r * d.rotation;
                    d.size *= s.abs();
                    out.push((d, attach.map(|a| format!("{name}.{a}"))));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The decal as a flat textured quad on the surface (1 mm out, facing out
/// of it), for exports: a game without the runtime plugin still shows it as
/// a sticker, and the plugin swaps it for a real projected decal.
pub fn quad_part(d: &Decal) -> crate::Part {
    let (w, h) = (d.size.x * 0.5, d.size.y * 0.5);
    // Seen from outside (looking along +Z), right is -X and up is +Y.
    let z = -0.001;
    let mut p = crate::Part::new(d.name.clone());
    p.verts = vec![[w, -h, z], [-w, -h, z], [-w, h, z], [w, h, z]];
    p.normals = vec![[0.0, 0.0, -1.0]; 4];
    p.uvs = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    p.indices = vec![0, 1, 2, 0, 2, 3];
    p.translation = d.position.to_array();
    p.rotation = d.rotation.to_array();
    p.material.pbr = Some(d.pbr.clone());
    p.material.base_color = [1.0; 4];
    p.cards = true;
    p
}

/// What an exported decal node carries in `extras.decal`.
pub fn extras(d: &Decal) -> serde_json::Value {
    serde_json::json!({"size": d.size.to_array(), "kind": d.kind})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Session;
    use serde_json::json;

    fn with_material(domain: &str) -> Session {
        let mut s = Session::default();
        s.materials
            .ops(
                "sticker",
                vec![
                    json!({"type": "circle", "name": "c", "center": [0.5, 0.5], "radius": 0.4}),
                    json!({"type": "mask", "name": "m", "sdf": "c"}),
                    json!({"type": "output", "name": "o", "base_color": "#c33", "opacity": "m"}),
                ],
                None,
                Some(domain.into()),
                Some(1.0),
                Some(32),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn orient_keeps_the_image_upright() {
        let q = orient(Vec3::NEG_Z, 0.0);
        assert!((q * Vec3::Z - Vec3::NEG_Z).length() < 1e-5);
        assert!((q * Vec3::Y - Vec3::Y).length() < 1e-5);
        // On a floor the image's up points to the back.
        let q = orient(Vec3::NEG_Y, 0.0);
        assert!((q * Vec3::Y - Vec3::NEG_Z).length() < 1e-5);
        assert_eq!(toward_dir("left"), Some(Vec3::NEG_X));
        assert_eq!(toward_dir("sideways"), None);
    }

    #[test]
    fn decals_stick_to_surfaces_and_export_as_quads() {
        let mut s = with_material("decal");
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "wall", "kind": "box", "size": [3, 2, 0.2],
                       "translation": [0, 1, -1]}),
                json!({"type": "decal", "name": "tag", "pbr": "sticker", "size": [0.8, 0.6],
                       "on": "wall", "toward": "back", "at": [0.5, 1.2]}),
                json!({"type": "decal", "name": "floor", "pbr": "sticker", "size": [0.5, 0.5],
                       "translation": [0, 0, 1]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let ds = collect(&s.model).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ds.len(), 2);
        // Stuck on the wall's front face (z = -0.9) at x 0.5, y 1.2.
        assert!(
            (ds[0].position - Vec3::new(0.5, 1.2, -0.9)).length() < 1e-3,
            "{:?}",
            ds[0].position
        );
        assert!((ds[0].direction() - Vec3::NEG_Z).length() < 1e-5);
        // Default depth: half the smaller side.
        assert!((ds[0].size.z - 0.3).abs() < 1e-5);
        // Not stuck: projects down.
        assert!((ds[1].direction() - Vec3::NEG_Y).length() < 1e-5);
        // A quad facing out of the wall.
        let q = quad_part(&ds[0]);
        let n = Quat::from_array(q.rotation) * Vec3::from(q.normals[0]);
        assert!((n - Vec3::Z).length() < 1e-5);
        let (doc, _, _) = crate::gltf::build_with(
            &s.model,
            Some("t.bin".into()),
            &crate::export_opt::Options::none(),
            None,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let decal_nodes = doc["nodes"].as_array().map_or(0, |ns| {
            ns.iter()
                .filter(|n| n["extras"]["decal"].is_object())
                .count()
        });
        assert_eq!(decal_nodes, 2);
    }

    #[test]
    fn decals_need_a_decal_or_card_material() {
        let mut s = with_material("tile");
        let e = s
            .apply_values(
                None,
                vec![json!({"type": "decal", "name": "tag", "pbr": "sticker", "size": [1, 1]})],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("decal"), "{e}");
    }
}
