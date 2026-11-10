//! Boolean operations on parts via csgrs. Both parts are transformed into model space
//! (translation/rotation/scale baked in), the op runs on triangle soup, and the result
//! comes back as a plain triangle `Part` with box-projected UVs and part `a`'s material.

use crate::{Model, Part, quat_of};
use bevy::math::{Quat, Vec3};
use csgrs::CSG;

/// The supported ops: "union", "difference"/"subtract", "intersect".
fn run_op(op: &str, a: &CSG<()>, b: &CSG<()>) -> Result<CSG<()>, String> {
    match op {
        "union" => Ok(a.union(b)),
        "difference" | "subtract" => Ok(a.subtract(b)),
        "intersect" => Ok(a.intersect(b)),
        other => Err(format!("unknown boolean op `{other}`")),
    }
}

/// A part's triangles in model space, as a csgrs mesh: translation, rotation and scale
/// baked in. Each triangle becomes its own face; csgrs derives the plane normals.
fn part_to_csg(part: &Part) -> CSG<()> {
    let rot = quat_of(part.rotation);
    let scale = Vec3::from(part.scale);
    let to_model = |v: &[f32; 3]| -> [f64; 3] {
        let p = rot * (Vec3::from(*v) * scale) + Vec3::from(part.translation);
        [p.x as f64, p.y as f64, p.z as f64]
    };
    let count = part.verts.len();
    let index_at =
        |i: usize| -> usize { part.indices.get(i).copied().unwrap_or(i as u32) as usize };
    let tris = if part.indices.is_empty() {
        count / 3
    } else {
        part.indices.len() / 3
    };
    let points: Vec<[f64; 3]> = part.verts.iter().map(to_model).collect();
    let mut faces: Vec<Vec<usize>> = Vec::with_capacity(tris);
    for t in 0..tris {
        let i = [index_at(t * 3), index_at(t * 3 + 1), index_at(t * 3 + 2)];
        if i.iter().any(|k| *k >= count) {
            continue;
        }
        // Skip degenerate triangles: csgrs panics on planes through collinear points.
        let (va, vb, vc) = (
            Vec3::from(part.verts[i[0]]),
            Vec3::from(part.verts[i[1]]),
            Vec3::from(part.verts[i[2]]),
        );
        let n = (vb - va).cross(vc - va);
        if n.length_squared() < 1e-12 {
            continue;
        }
        faces.push(i.to_vec());
    }
    // `polyhedron` panics on out-of-range indices, so the validation above must hold.
    debug_assert!(faces.iter().flatten().all(|k| *k < points.len()));
    CSG::polyhedron(&points, &faces)
}

/// A csgrs polygon soup back into a flat triangle part (three verts per triangle).
fn csg_to_part(csg: &CSG<()>, name: &str, material: crate::Material) -> Part {
    let mut part = Part::new(name);
    part.material = material;
    for poly in csg.to_polygons() {
        for tri in poly.triangulate() {
            let base = part.verts.len() as u32;
            for v in &tri {
                let (p, n) = (&v.pos, &v.normal);
                part.verts.push([p.x as f32, p.y as f32, p.z as f32]);
                let n = Vec3::new(n.x as f32, n.y as f32, n.z as f32);
                part.normals.push(if n.length_squared() > 1e-12 {
                    n.normalize().to_array()
                } else {
                    [0.0; 3]
                });
            }
            part.indices.extend([base, base + 1, base + 2]);
        }
    }
    part
}

/// Boolean `op` on the named parts of `model`, returning a new part named
/// `name.unwrap_or("{a}-{op}-{b}")` with `a`'s material. Errors on unknown parts or ops
/// and on empty results (no overlap, coincident faces, ...).
pub fn boolean(
    model: &Model,
    op: &str,
    a: &str,
    b: &str,
    name: Option<&str>,
) -> Result<Part, String> {
    let pa = model.part(a).ok_or_else(|| format!("no part `{a}`"))?;
    let pb = model.part(b).ok_or_else(|| format!("no part `{b}`"))?;
    let csg_a = part_to_csg(pa);
    let csg_b = part_to_csg(pb);
    let result = run_op(op, &csg_a, &csg_b)?;
    let default_name = format!("{a}-{op}-{b}");
    let out_name = name.unwrap_or(&default_name);
    let mut part = csg_to_part(&result, out_name, pa.material.clone());
    if part.verts.is_empty() || part.tri_count() == 0 {
        return Err(format!(
            "{op}({a}, {b}) produced an empty mesh (no overlap?)"
        ));
    }
    // The transform is baked into the verts, so the part sits at the model root.
    part.translation = [0.0; 3];
    part.rotation = Quat::IDENTITY.to_array();
    part.scale = [1.0; 3];
    let _ = crate::uv::ensure_uvs(&mut part, Some("box"));
    Ok(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_with_box(name: &str, half: f32) -> Model {
        let mut model = Model {
            name: "m".into(),
            ..Default::default()
        };
        let part = crate::primitives::primitive("box", [half * 2.0; 3], name)
            .unwrap_or_else(|_| panic!("{name} failed"));
        model.parts.push(part);
        model
    }

    #[test]
    fn box_minus_smaller_box_has_more_triangles() {
        let mut model = model_with_box("big", 2.0);
        let small = crate::primitives::primitive("box", [1.0; 3], "small")
            .unwrap_or_else(|_| panic!("small failed"));
        model.parts.push(small);
        let before = model.part("big").map_or(0, |p| p.tri_count());
        let out = match boolean(&model, "difference", "big", "small", None) {
            Ok(out) => out,
            Err(e) => panic!("difference failed: {e}"),
        };
        assert!(
            out.tri_count() > before,
            "{} tris vs {before}",
            out.tri_count()
        );
        assert_eq!(out.name, "big-difference-small");
        // The boolean is pure: both original parts still exist on the model.
        assert!(model.part("big").is_some());
        assert!(model.part("small").is_some());
        validate(&out, "difference");
    }

    #[test]
    fn union_and_intersect_smoke() {
        let mut model = model_with_box("a", 2.0);
        model.parts.push(
            crate::primitives::primitive("sphere", [1.0; 3], "b")
                .unwrap_or_else(|_| panic!("b failed")),
        );
        for op in ["union", "intersect", "subtract"] {
            let out = match boolean(&model, op, "a", "b", None) {
                Ok(out) => out,
                Err(e) => panic!("{op} failed: {e}"),
            };
            validate(&out, op);
        }
    }

    #[test]
    fn disjoint_difference_errs() {
        let mut model = model_with_box("a", 1.0);
        let mut far = crate::primitives::primitive("box", [1.0; 3], "b")
            .unwrap_or_else(|_| panic!("b failed"));
        far.translation = [100.0, 0.0, 0.0];
        model.parts.push(far);
        assert!(boolean(&model, "intersect", "a", "b", None).is_err());
    }

    #[test]
    fn transform_is_applied_before_the_boolean() {
        // A small box rotated 45° cutting a bigger box still carves a hole.
        let mut model = model_with_box("big", 2.0);
        let mut cutter = crate::primitives::primitive("box", [1.0; 3], "cutter")
            .unwrap_or_else(|_| panic!("cutter failed"));
        cutter.rotation = [0.0, 0.0, 0.3826834, 0.9238795]; // 45° around z
        model.parts.push(cutter);
        let out = match boolean(&model, "subtract", "big", "cutter", Some("hole")) {
            Ok(out) => out,
            Err(e) => panic!("rotated subtract failed: {e}"),
        };
        assert_eq!(out.name, "hole");
        validate(&out, "rotated-subtract");
    }

    #[test]
    fn unknown_part_and_op_err() {
        let model = model_with_box("a", 1.0);
        assert!(boolean(&model, "union", "a", "ghost", None).is_err());
        assert!(boolean(&model, "xor", "a", "a", None).is_err());
    }

    fn validate(part: &Part, what: &str) {
        assert_eq!(part.uvs.len(), part.verts.len(), "{what} uv count");
        assert_eq!(part.normals.len(), part.verts.len(), "{what} normal count");
        assert!(
            part.indices
                .iter()
                .all(|i| (*i as usize) < part.verts.len())
        );
        for uv in &part.uvs {
            assert!(
                (0.0..=1.0).contains(&uv[0]),
                "{what} u out of range: {uv:?}"
            );
            assert!(
                (0.0..=1.0).contains(&uv[1]),
                "{what} v out of range: {uv:?}"
            );
        }
    }
}
