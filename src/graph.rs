//! The procedural op graph: an ordered list of [`Op`] steps that derive the
//! model's parts from each other, evaluated top to bottom. An op may reference
//! earlier outputs by name; re-issuing an op with the same output name replaces
//! it (that is how parameters are "tweaked"). No game code.

use crate::{Op, Part, primitives, quat_of, uv};
use bevy::math::{Quat, Vec3};

/// Evaluates every op in order and returns the derived parts, in op order.
/// Errors name the offending step so the caller can point at the graph.
pub fn evaluate(ops: &[Op]) -> Result<Vec<Part>, String> {
    let mut named: Vec<(String, Part)> = Vec::new();
    for op in ops {
        match op {
            Op::Primitive {
                name,
                kind,
                size,
                translation,
            } => {
                let mut part =
                    primitives::primitive(kind, *size, name).map_err(|e| format!("{name}: {e}"))?;
                part.translation = *translation;
                upsert(&mut named, name, part)?;
            }
            Op::Transform {
                name,
                input,
                translation,
                rotation,
                scale,
            } => {
                let mut part = lookup(&named, input)?;
                part.name = name.clone();
                if let Some(t) = translation {
                    part.translation = *t;
                }
                if let Some(r) = rotation {
                    part.rotation = *r;
                }
                if let Some(s) = scale {
                    part.scale = *s;
                }
                upsert(&mut named, name, part)?;
            }
            Op::Boolean { name, op, a, b } => {
                // `csg::boolean` looks parts up on a model and bakes their
                // transforms into the verts, so hand it a scratch pair.
                let mut scratch = crate::Model::named("scratch");
                scratch.parts = vec![lookup(&named, a)?, lookup(&named, b)?];
                let part = crate::csg::boolean(&scratch, op, a, b, Some(name))?;
                upsert(&mut named, name, part)?;
            }
            Op::RadialArray {
                name,
                input,
                count,
                center,
                degrees,
                start_degrees,
            } => {
                let source = lookup(&named, input)?;
                if *count == 0 {
                    return Err(format!("{name}: count must be at least 1"));
                }
                let step = degrees / *count as f32;
                for i in 0..*count {
                    let yaw = start_degrees + i as f32 * step;
                    let mut part = source.clone();
                    part.name = format!("{name}_{i}");
                    let offset = Vec3::from(part.translation) - Vec3::from(*center);
                    part.translation = (Quat::from_rotation_y(yaw.to_radians()) * offset
                        + Vec3::from(*center))
                    .to_array();
                    part.rotation = (Quat::from_rotation_y(yaw.to_radians())
                        * quat_of(part.rotation))
                    .to_array();
                    uv::ensure_uvs(&mut part, Some("box"))
                        .map_err(|e| format!("{name}_{i}: {e}"))?;
                    upsert(&mut named, &format!("{name}_{i}"), part)?;
                }
            }
            Op::Material {
                name,
                input,
                base_color,
                roughness,
                metalness,
            } => {
                let mut part = lookup(&named, input)?;
                part.name = name.clone();
                if let Some(c) = base_color {
                    part.material.base_color = *c;
                }
                if let Some(r) = roughness {
                    part.material.roughness = *r;
                }
                if let Some(m) = metalness {
                    part.material.metalness = *m;
                }
                upsert(&mut named, name, part)?;
            }
            Op::LinearArray {
                name,
                input,
                count,
                offset,
            } => {
                let source = lookup(&named, input)?;
                if *count == 0 {
                    return Err(format!("{name}: count must be at least 1"));
                }
                // The source is copy 0; copies 1..count step away from it,
                // so nothing overlaps the input.
                for i in 1..*count {
                    let mut part = source.clone();
                    part.name = format!("{name}_{i}");
                    let i = i as f32;
                    part.translation = [
                        part.translation[0] + offset[0] * i,
                        part.translation[1] + offset[1] * i,
                        part.translation[2] + offset[2] * i,
                    ];
                    let part_name = part.name.clone();
                    upsert(&mut named, &part_name, part)?;
                }
            }
            Op::Mirror {
                name,
                input,
                axis,
                offset,
            } => {
                let ax = match axis.as_str() {
                    "x" => 0,
                    "y" => 1,
                    "z" => 2,
                    other => return Err(format!("{name}: unknown axis `{other}` (x, y, z)")),
                };
                let source = lookup(&named, input)?;
                let plane = offset[ax];
                let mut part = source.clone();
                part.name = name.clone();
                // Bake: world verts, reflect across the plane, flip winding.
                let rot = quat_of(part.rotation);
                let scale = Vec3::from(part.scale);
                let translation = Vec3::from(part.translation);
                let mut tris = Vec::with_capacity(part.tri_count() as usize);
                for t in part.indices.as_chunks::<3>().0 {
                    let world: Vec<Vec3> = t
                        .iter()
                        .map(|&i| {
                            let mut v =
                                rot * (Vec3::from(part.verts[i as usize]) * scale) + translation;
                            v[ax] = 2.0 * plane - v[ax];
                            v
                        })
                        .collect();
                    tris.push([world[0], world[1], world[2]]);
                }
                let mut verts = Vec::with_capacity(tris.len() * 3);
                let mut indices = Vec::with_capacity(tris.len() * 3);
                for (i, t) in tris.iter().enumerate() {
                    for v in t {
                        verts.push(v.to_array());
                    }
                    // Mirrored faces wind the other way.
                    let b = (i * 3) as u32;
                    indices.extend([b, b + 2, b + 1]);
                }
                part.verts = verts;
                part.indices = indices;
                part.normals.clear();
                crate::uv::ensure_normals(&mut part);
                crate::uv::ensure_uvs(&mut part, Some("box"))?;
                part.translation = [0.0; 3];
                part.rotation = Quat::IDENTITY.to_array();
                part.scale = [1.0; 3];
                upsert(&mut named, name, part)?;
            }
        }
    }
    Ok(named.into_iter().map(|(_, part)| part).collect())
}

fn lookup(named: &[(String, Part)], name: &str) -> Result<Part, String> {
    named
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p.clone())
        .ok_or_else(|| format!("no part `{name}` earlier in the graph"))
}

/// Inserts `part` under `name`, replacing an earlier op's output of the same
/// name in place (so output order stays stable across re-issues).
fn upsert(named: &mut Vec<(String, Part)>, name: &str, part: Part) -> Result<(), String> {
    match named.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = part,
        None => named.push((name.to_string(), part)),
    }
    Ok(())
}

/// The output part name an op writes, used for upsert matching.
pub fn output(op: &Op) -> &str {
    match op {
        Op::Primitive { name, .. }
        | Op::Transform { name, .. }
        | Op::Boolean { name, .. }
        | Op::RadialArray { name, .. }
        | Op::Material { name, .. }
        | Op::LinearArray { name, .. }
        | Op::Mirror { name, .. } => name,
    }
}

/// What kind of op this is; upserts only replace ops of the same kind, so an
/// in-place op (output name == an input name) never eats its own source.
pub fn kind(op: &Op) -> &'static str {
    match op {
        Op::Primitive { .. } => "primitive",
        Op::Transform { .. } => "transform",
        Op::Boolean { .. } => "boolean",
        Op::RadialArray { .. } => "radial",
        Op::Material { .. } => "material",
        Op::LinearArray { .. } => "linear",
        Op::Mirror { .. } => "mirror",
    }
}

/// The part names an op reads.
pub fn inputs(op: &Op) -> Vec<&str> {
    match op {
        Op::Primitive { .. } => Vec::new(),
        Op::Transform { input, .. }
        | Op::RadialArray { input, .. }
        | Op::Material { input, .. }
        | Op::LinearArray { input, .. }
        | Op::Mirror { input, .. } => {
            vec![input]
        }
        Op::Boolean { a, b, .. } => vec![a, b],
    }
}

/// One line per op, for summaries and the `ops` tool.
pub fn describe(ops: &[Op]) -> String {
    if ops.is_empty() {
        return "ops: none (baked parts)".into();
    }
    let mut lines = vec![format!("ops ({})", ops.len())];
    for (i, op) in ops.iter().enumerate() {
        lines.push(match op {
            Op::Primitive { name, kind, .. } => format!("#{i} primitive `{name}` ({kind})"),
            Op::Transform { name, input, .. } => format!("#{i} transform `{name}` <- `{input}`"),
            Op::Boolean { name, op, a, b } => {
                format!("#{i} boolean `{name}` = {op}(`{a}`, `{b}`)")
            }
            Op::RadialArray {
                name,
                input,
                count,
                degrees,
                ..
            } => format!("#{i} radial `{name}` = {count} x `{input}` over {degrees}°"),
            Op::Material { name, input, .. } => format!("#{i} material `{name}` <- `{input}`"),
            Op::LinearArray {
                name,
                input,
                count,
                offset,
                ..
            } => format!("#{i} linear `{name}` = {count} x `{input}` stepping {offset:?}"),
            Op::Mirror {
                name,
                input,
                axis,
                offset,
                ..
            } => format!(
                "#{i} mirror `{name}` = `{input}` across {} = {}",
                axis,
                offset["xyz".find(axis).unwrap_or(0)]
            ),
        });
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_then_transform_moves_the_part() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "box".into(),
                kind: "box".into(),
                size: [1.0, 1.0, 1.0],
                translation: [0.0, 0.0, 0.0],
            },
            Op::Transform {
                name: "lifted".into(),
                input: "box".into(),
                translation: Some([0.0, 2.0, 0.0]),
                rotation: None,
                scale: None,
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].name, "lifted");
        assert_eq!(parts[1].translation, [0.0, 2.0, 0.0]);
        assert_eq!(parts[0].translation, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn reissued_op_replaces_its_output_in_place() {
        let ops = [
            Op::Primitive {
                name: "a".into(),
                kind: "box".into(),
                size: [1.0, 1.0, 1.0],
                translation: [0.0, 0.0, 0.0],
            },
            Op::Primitive {
                name: "b".into(),
                kind: "sphere".into(),
                size: [0.5, 0.0, 0.0],
                translation: [0.0, 0.0, 0.0],
            },
            Op::Primitive {
                name: "a".into(),
                kind: "cylinder".into(),
                size: [0.1, 2.0, 0.0],
                translation: [0.0, 0.0, 0.0],
            },
        ];
        let parts = evaluate(&ops).unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(parts[0].verts.len() > 4);
    }

    #[test]
    fn radial_array_circles_around_the_center() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "petal".into(),
                kind: "sphere".into(),
                size: [0.1, 0.0, 0.0],
                translation: [0.0, 0.0, 0.5],
            },
            Op::RadialArray {
                name: "ring".into(),
                input: "petal".into(),
                count: 4,
                center: [0.0, 0.0, 0.0],
                degrees: 360.0,
                start_degrees: 0.0,
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.len(), 5);
        // ring_0 is the unrotated copy; 90° steps walk +x, -z, -x.
        let xs: Vec<_> = parts[1..].iter().map(|p| p.translation[0]).collect();
        for (got, want) in xs.iter().zip([0.0, 0.5, 0.0, -0.5]) {
            assert!((got - want).abs() < 1e-5, "x {got} vs {want}");
        }
        let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["petal", "ring_0", "ring_1", "ring_2", "ring_3"]);
    }

    #[test]
    fn linear_array_steps_in_a_line() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "post".into(),
                kind: "cylinder".into(),
                size: [0.05, 1.0, 0.0],
                translation: [0.0, 0.5, 0.0],
            },
            Op::LinearArray {
                name: "fence".into(),
                input: "post".into(),
                count: 4,
                offset: [1.0, 0.0, 0.0],
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        let xs: Vec<_> = parts.iter().map(|p| p.translation[0]).collect();
        // post (copy 0) + fence_1..fence_3
        assert_eq!(xs, [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(parts[3].name, "fence_3");
    }

    #[test]
    fn mirror_reflects_and_flips_winding() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "arm".into(),
                kind: "box".into(),
                size: [0.2, 0.2, 0.6],
                translation: [0.5, 0.0, 0.0],
            },
            Op::Mirror {
                name: "arm_m".into(),
                input: "arm".into(),
                axis: "x".into(),
                offset: [0.0, 0.0, 0.0],
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        let mirrored = &parts[1];
        // The translated +x box lands on -x.
        let min_x = mirrored.verts.iter().map(|v| v[0]).fold(f32::MAX, f32::min);
        assert!((min_x - -0.6).abs() < 1e-5, "min x {min_x}");
        // Normals still point outward: every normal must agree with the
        // direction from the box center to its vertex.
        for (v, n) in mirrored.verts.iter().zip(&mirrored.normals) {
            // Outward = from the mirrored box's center (-0.5, 0, 0).
            let outward = (Vec3::from(*v) - Vec3::new(-0.5, 0.0, 0.0)).normalize_or_zero();
            let dot = outward.dot(Vec3::from(*n));
            assert!(dot > 0.0, "normal points inward at {v:?}");
        }
    }

    #[test]
    fn mirror_rejects_unknown_axis() {
        let err = match evaluate(&[Op::Mirror {
            name: "m".into(),
            input: "ghost".into(),
            axis: "w".into(),
            offset: [0.0; 3],
        }]) {
            Err(e) => e,
            Ok(_) => panic!("bad axis must fail"),
        };
        assert!(err.contains('w'), "{err}");
    }

    #[test]
    fn missing_input_names_the_break() {
        let err = match evaluate(&[Op::Transform {
            name: "x".into(),
            input: "ghost".into(),
            translation: None,
            rotation: None,
            scale: None,
        }]) {
            Err(e) => e,
            Ok(_) => panic!("missing input must fail"),
        };
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn describe_lists_each_op() {
        let text = describe(&[Op::RadialArray {
            name: "ring".into(),
            input: "petal".into(),
            count: 8,
            center: [0.0; 3],
            degrees: 360.0,
            start_degrees: 45.0,
        }]);
        assert!(text.contains("8 x `petal`"), "{text}");
    }
}
