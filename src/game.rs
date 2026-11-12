//! Game data that is not geometry: collision shapes (`collider` rules) and
//! named points (`marker` items). Both are collected after the graph, from
//! the model and its prefab instances, and follow moves; the glTF export
//! writes them as node extras that a few lines of Bevy code turn into
//! physics colliders and spawn points (see AGENTS.md, Game data).

use bevy::math::{Quat, Vec3};
use serde_json::Value;

use crate::{Model, ModelerError, Op, Part, quat_of};

#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub name: String,
    pub kind: String,
    pub position: Vec3,
    pub rotation: Quat,
    pub size: Option<[f32; 3]>,
    pub data: Option<Value>,
    /// A catalog prefab to draw here in previews (see `Op::Marker`).
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Collider {
    /// The part it belongs to.
    pub part: String,
    /// box, hull or mesh.
    pub shape: String,
    pub body: String,
    pub sensor: bool,
    /// World centre of the part's box (the collider node's origin).
    pub center: Vec3,
    pub half_extents: Vec3,
    /// Hull points relative to `center` (hull only).
    pub points: Vec<[f32; 3]>,
}

/// One collider rule, with patterns in this model's part names.
struct Rule {
    parts: Vec<String>,
    shape: String,
    body: String,
    sensor: bool,
}

/// Every marker in world space, after moves.
pub fn markers(model: &Model) -> Result<Vec<Marker>, ModelerError> {
    let raw = unmoved_markers(model, 0)?;
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let deltas = crate::light::move_deltas(model)?;
    Ok(raw
        .into_iter()
        .map(|(mut m, attach)| {
            if let Some((r, t)) = attach.and_then(|a| deltas.get(&a).copied()) {
                m.position = r * m.position + t;
                m.rotation = r * m.rotation;
            }
            m
        })
        .collect())
}

fn unmoved_markers(
    model: &Model,
    depth: u32,
) -> Result<Vec<(Marker, Option<String>)>, ModelerError> {
    if depth > 8 {
        return Err("markers: prefabs nested more than 8 deep".into());
    }
    let mut out = Vec::new();
    for op in &model.ops {
        match op {
            Op::Marker {
                name,
                kind,
                translation,
                yaw,
                size,
                data,
                attach,
                preview,
                ..
            } => out.push((
                Marker {
                    name: name.clone(),
                    kind: kind.clone(),
                    position: Vec3::from(*translation),
                    rotation: Quat::from_rotation_y(yaw.unwrap_or(0.0).to_radians()),
                    size: *size,
                    data: data.clone(),
                    preview: preview.clone(),
                },
                attach.clone(),
            )),
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
                for (mut m, attach) in unmoved_markers(&src, depth + 1)? {
                    m.name = format!("{name}.{}", m.name);
                    m.position = t + r * (s * m.position);
                    m.rotation = r * m.rotation;
                    m.size = m.size.map(|z| (Vec3::from(z) * s.abs()).to_array());
                    out.push((m, attach.map(|a| format!("{name}.{a}"))));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn rules(model: &Model, depth: u32) -> Vec<Rule> {
    if depth > 8 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for op in &model.ops {
        match op {
            Op::Collider {
                parts,
                shape,
                body,
                sensor,
                ..
            } => out.push(Rule {
                parts: parts.clone(),
                shape: shape.clone(),
                body: body.clone(),
                sensor: *sensor,
            }),
            Op::Prefab { name, prefab, .. } => {
                let Ok(src) = crate::catalog::prefab_instance(prefab) else {
                    continue;
                };
                // The prefab's own rules apply to its parts in this scene;
                // they come first so the scene's rules override them.
                let mut inner = rules(&src, depth + 1);
                for r in &mut inner {
                    for p in &mut r.parts {
                        *p = format!("{name}.{p}");
                    }
                }
                out.splice(0..0, inner);
            }
            _ => {}
        }
    }
    out
}

/// The collider of every visible part some rule covers (the last matching
/// rule wins; `none` removes it).
pub fn colliders(model: &Model) -> Result<Vec<Collider>, ModelerError> {
    let rules = rules(model, 0);
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in model
        .parts
        .iter()
        .filter(|p| p.visible && !p.verts.is_empty())
    {
        let Some(rule) = rules.iter().rev().find(|r| {
            r.parts
                .iter()
                .any(|pat| crate::name_matches(pat, &part.name))
        }) else {
            continue;
        };
        if rule.shape == "none" {
            continue;
        }
        let world = world_verts(part);
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &world {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        let center = (lo + hi) * 0.5;
        let points = if rule.shape == "hull" {
            let refs = [part.as_ref()];
            let h = crate::csg::hull(&part.name, &refs)?;
            let mut pts: Vec<[f32; 3]> = world_verts(&h)
                .into_iter()
                .map(|v| (v - center).to_array())
                .collect();
            pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            pts.dedup();
            pts
        } else {
            Vec::new()
        };
        out.push(Collider {
            part: part.name.clone(),
            shape: rule.shape.clone(),
            body: rule.body.clone(),
            sensor: rule.sensor,
            center,
            half_extents: (hi - lo) * 0.5,
            points,
        });
    }
    Ok(out)
}

fn world_verts(p: &Part) -> Vec<Vec3> {
    let r = quat_of(p.rotation);
    let (s, t) = (Vec3::from(p.scale), Vec3::from(p.translation));
    p.verts
        .iter()
        .map(|v| r * (Vec3::from(*v) * s) + t)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Session;
    use serde_json::json;

    fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    fn apply(s: &mut Session, items: Value) {
        let Value::Array(items) = items else {
            panic!("array");
        };
        ok(s.apply_values(None, items));
    }

    #[test]
    fn colliders_and_markers_export_as_extras() {
        let mut s = Session::default();
        apply(
            &mut s,
            json!([
                {"type": "primitive", "name": "floor", "kind": "box", "size": [6, 0.1, 6],
                 "translation": [0, -0.05, 0]},
                {"type": "primitive", "name": "wall", "kind": "box", "size": [6, 2, 0.2],
                 "translation": [0, 1, -3]},
                {"type": "primitive", "name": "decal", "kind": "box", "size": [0.5, 0.5, 0.01],
                 "translation": [0, 1, -2.89]},
                {"type": "primitive", "name": "crate", "kind": "box", "size": 0.5,
                 "translation": [1, 0.25, 0]},
                {"type": "collider", "name": "level", "parts": ["*"], "shape": "box"},
                {"type": "collider", "name": "walls", "parts": ["wall"], "shape": "mesh"},
                {"type": "collider", "name": "no_decals", "parts": ["decal"], "shape": "none"},
                {"type": "collider", "name": "props", "parts": ["crate"], "shape": "hull",
                 "body": "dynamic"},
                {"type": "marker", "name": "spawn", "kind": "spawn", "translation": [0, 0, 2],
                 "yaw": 90, "data": {"team": 1}},
                {"type": "marker", "name": "pickup", "kind": "item", "translation": [1, 0.6, 0],
                 "attach": "crate", "size": [0.4, 0.4, 0.4]},
                {"type": "move", "name": "push", "parts": ["crate"], "translation": [0, 0, 1]}
            ]),
        );
        let cs = ok(colliders(&s.model));
        let by = |n: &str| cs.iter().find(|c| c.part == n);
        assert_eq!(cs.len(), 3, "decal exempted");
        assert!(by("decal").is_none());
        let Some(floor) = by("floor") else {
            panic!("floor")
        };
        assert_eq!(floor.shape, "box");
        assert!((floor.half_extents - Vec3::new(3.0, 0.05, 3.0)).length() < 1e-4);
        let Some(crate_c) = by("crate") else {
            panic!("crate")
        };
        assert_eq!(
            (crate_c.shape.as_str(), crate_c.body.as_str()),
            ("hull", "dynamic")
        );
        assert_eq!(crate_c.points.len(), 8);
        assert!(
            (crate_c.center - Vec3::new(1.0, 0.25, 1.0)).length() < 1e-4,
            "moved"
        );
        let ms = ok(markers(&s.model));
        assert!(
            (ms[1].position - Vec3::new(1.0, 0.6, 1.0)).length() < 1e-4,
            "rides the crate"
        );
        assert!(
            ((ms[0].rotation * Vec3::Z) - Vec3::X).length() < 1e-4,
            "yaw 90 faces +x"
        );
        // Parse errors name the problem.
        let err = s
            .apply_values(
                None,
                vec![json!({"type": "collider", "name": "x", "parts": ["crate"],
                            "shape": "mesh", "body": "dynamic"})],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("convex"), "{err}");

        let (doc, _, report) = ok(crate::gltf::build_with(
            &s.model,
            None,
            &crate::export_opt::Options::none(),
            None,
            false,
        ));
        let nodes = ok(doc["nodes"].as_array().ok_or("nodes"));
        let find = |n: &str| nodes.iter().position(|x| x["name"] == json!(n));
        let (Some(crate_i), Some(cc)) = (find("crate"), find("crate.collider")) else {
            panic!("crate nodes");
        };
        assert_eq!(nodes[cc]["children"], json!([crate_i]));
        assert_eq!(
            nodes[cc]["extras"]["collider"]["shape"],
            json!("convex_hull")
        );
        let roots = doc["scenes"][0]["nodes"].to_string();
        assert!(
            !roots.contains(&format!("[{crate_i},")) && !roots.contains(&format!(",{crate_i},"))
        );
        let Some(w) = find("wall") else {
            panic!("wall")
        };
        assert_eq!(nodes[w]["extras"]["collider"]["shape"], json!("trimesh"));
        let Some(sp) = find("spawn") else {
            panic!("spawn")
        };
        assert_eq!(nodes[sp]["extras"]["marker"], json!("spawn"));
        assert_eq!(nodes[sp]["extras"]["data"]["team"], json!(1));
        assert!(
            report.contains("colliders: 3 (1 box, 1 hull, 1 mesh)"),
            "{report}"
        );
        assert!(report.contains("markers: 2"), "{report}");
    }

    #[test]
    fn prefab_instances_bring_their_colliders_and_markers() {
        let mut s = Session::default();
        ok(s.new_model("gm_barrel"));
        apply(
            &mut s,
            json!([
                {"type": "primitive", "name": "body", "kind": "cylinder", "size": [0.3, 0.9],
                 "translation": [0, 0.45, 0]},
                {"type": "collider", "name": "solid", "parts": ["body"], "shape": "hull",
                 "body": "dynamic"},
                {"type": "marker", "name": "grab", "kind": "interact", "translation": [0, 1, 0]}
            ]),
        );
        ok(s.scene("open", Some("gm_yard")));
        apply(
            &mut s,
            json!([
                {"type": "prefab", "name": "b1", "prefab": "gm_barrel", "translation": [2, 0, 0]},
                {"type": "prefab", "name": "b2", "prefab": "gm_barrel", "translation": [-2, 0, 0]},
                {"type": "collider", "name": "b2_static", "parts": ["b2.*"], "shape": "box"}
            ]),
        );
        let cs = ok(colliders(&s.model));
        assert_eq!(cs.len(), 2);
        let b1 = cs.iter().find(|c| c.part == "b1.body");
        let b2 = cs.iter().find(|c| c.part == "b2.body");
        assert_eq!(b1.map(|c| c.body.as_str()), Some("dynamic"));
        assert_eq!(
            b2.map(|c| (c.shape.as_str(), c.body.as_str())),
            Some(("box", "static"))
        );
        let ms = ok(markers(&s.model));
        let names: Vec<&str> = ms.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["b1.grab", "b2.grab"]);
        assert!((ms[0].position - Vec3::new(2.0, 1.0, 0.0)).length() < 1e-4);
        ok(s.scene("close", Some("gm_barrel")));
    }
}
