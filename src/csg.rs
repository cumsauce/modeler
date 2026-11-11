//! Boolean operations on parts via Manifold (`manifold-csg`, bindings to the C++
//! kernel OpenSCAD and Blender use). Both parts are transformed into model space
//! (translation/rotation/scale baked in) and carry their UVs as vertex properties;
//! the result is a closed, manifold triangle `Part` with interpolated UVs, creased
//! normals and part `a`'s material. Coplanar faces are resolved exactly by the
//! kernel, so no nudging or welding is needed.

use crate::{BoolOp, Model, ModelerError, Part, quat_of};
use bevy::math::{Quat, Vec3};
use manifold_csg::{Manifold, MeshGL64};

/// Vertex properties per vertex: position (3) + UV (2).
const N_PROPS: usize = 5;

fn run_op(op: BoolOp, a: &Manifold, b: &Manifold) -> Manifold {
    match op {
        BoolOp::Union => a.union(b),
        BoolOp::Difference => a.difference(b),
        BoolOp::Intersect => a.intersection(b),
    }
}

/// A part as a Manifold: transform baked into the positions, UVs carried as
/// properties. Vertices split only for UV/normal seams are re-joined by
/// `MeshGL64::merge` (by position), which is what makes a primitive's
/// seam-split mesh a closed solid to Manifold. Errors when the part is not a
/// closed solid (open sheets like disc/plane/washer, torn meshes).
fn part_to_manifold(part: &Part) -> Result<Manifold, ModelerError> {
    let rot = quat_of(part.rotation);
    let scale = Vec3::from(part.scale);
    let t = Vec3::from(part.translation);
    // Parts without UVs get a box projection first so every vertex has one.
    let projected;
    let src = if part.uvs.len() == part.verts.len() {
        part
    } else {
        let mut p = part.clone();
        let _ = crate::uv::ensure_uvs(&mut p, Some("box"));
        projected = p;
        &projected
    };
    let mut props = Vec::with_capacity(src.verts.len() * N_PROPS);
    for (i, v) in src.verts.iter().enumerate() {
        let p = rot * (Vec3::from(*v) * scale) + t;
        let uv = src.uvs.get(i).copied().unwrap_or([0.0; 2]);
        props.extend([
            p.x as f64,
            p.y as f64,
            p.z as f64,
            uv[0] as f64,
            uv[1] as f64,
        ]);
    }
    let count = src.verts.len();
    let tris: Vec<usize> = if src.indices.is_empty() {
        (0..count - count % 3).collect()
    } else {
        src.indices.iter().map(|&i| i as usize).collect()
    };
    if tris.iter().any(|&i| i >= count) {
        return Err(ModelerError::Csg(format!(
            "`{}` has out-of-range triangle indices",
            part.name
        )));
    }
    // Weld by position (1e-5 grid): verts split only for UV/normal seams become
    // merge pairs, so Manifold sees one topological vertex but keeps both UVs.
    let key = |i: usize| -> [i64; 3] {
        [
            (props[i * N_PROPS] * 1e5).round() as i64,
            (props[i * N_PROPS + 1] * 1e5).round() as i64,
            (props[i * N_PROPS + 2] * 1e5).round() as i64,
        ]
    };
    let mut first: std::collections::HashMap<[i64; 3], usize> = Default::default();
    let mut rep = vec![0usize; count];
    let (mut merge_from, mut merge_to) = (Vec::new(), Vec::new());
    for (i, r) in rep.iter_mut().enumerate() {
        let f = *first.entry(key(i)).or_insert(i);
        *r = f;
        if f != i {
            merge_from.push(i as u64);
            merge_to.push(f as u64);
        }
    }
    // Drop triangles that collapse once welded (e.g. UV-sphere pole fans).
    let mut kept: Vec<u64> = Vec::with_capacity(tris.len());
    for t in tris.as_chunks::<3>().0 {
        let r = [rep[t[0]], rep[t[1]], rep[t[2]]];
        if r[0] != r[1] && r[1] != r[2] && r[0] != r[2] {
            kept.extend(t.iter().map(|&i| i as u64));
        }
    }
    let not_solid = |e: String| {
        ModelerError::Csg(format!(
            "`{}` is not a closed solid ({e}); booleans need closed meshes \
             (disc/plane/washer are open sheets)",
            part.name
        ))
    };
    let opts = manifold_csg::MeshGL64Options::new().merge_vertices(&merge_from, &merge_to);
    let mesh = MeshGL64::new_with_options(&props, N_PROPS, &kept, opts)
        .map_err(|e| not_solid(e.to_string()))?;
    let m = Manifold::from_meshgl64(&mesh).map_err(|e| not_solid(e.to_string()))?;
    m.status().map_err(|e| not_solid(e.to_string()))?;
    // A double-sided sheet is "closed" topologically but encloses nothing.
    if m.volume().abs() < 1e-9 {
        return Err(not_solid("zero volume".into()));
    }
    Ok(m)
}

/// A Manifold back into a triangle `Part`: positions and interpolated UVs from
/// the properties, creased normals (40°, so cut edges stay hard), `material`.
fn manifold_to_part(m: &Manifold, name: &str, material: crate::Material) -> Part {
    let mesh = m.to_meshgl64();
    let n = mesh.num_prop().max(3);
    let props = mesh.vert_properties();
    let mut part = Part::new(name);
    part.material = material;
    for v in props.chunks(n) {
        part.verts.push([v[0] as f32, v[1] as f32, v[2] as f32]);
        if n >= N_PROPS {
            part.uvs.push([v[3] as f32, v[4] as f32]);
        }
    }
    part.indices = mesh.tri_verts().iter().map(|&i| i as u32).collect();
    if part.uvs.len() != part.verts.len() {
        part.uvs.clear();
    }
    crate::uv::crease_normals(&mut part, 40.0);
    if part.uvs.len() != part.verts.len() {
        let _ = crate::uv::ensure_uvs(&mut part, Some("box"));
    }
    part
}

/// Boolean `op` on the named parts of `model`, returning a new part named
/// `name.unwrap_or("{a}-{op}-{b}")` with `a`'s material. Errors on unknown parts,
/// non-solid inputs and empty results (no overlap, ...).
pub fn boolean(
    model: &Model,
    op: BoolOp,
    a: &str,
    b: &str,
    name: Option<&str>,
) -> Result<Part, ModelerError> {
    let pa = model
        .part(a)
        .ok_or_else(|| ModelerError::NoPart(a.to_string()))?;
    let pb = model
        .part(b)
        .ok_or_else(|| ModelerError::NoPart(b.to_string()))?;
    let result = run_op(op, &part_to_manifold(pa)?, &part_to_manifold(pb)?);
    result
        .status()
        .map_err(|e| ModelerError::Csg(format!("{op}({a}, {b}) failed: {e}")))?;
    if result.is_empty() {
        return Err(ModelerError::Csg(format!(
            "{op}({a}, {b}) produced an empty mesh (no overlap?)"
        )));
    }
    let default_name = format!("{a}-{op}-{b}");
    let mut part = manifold_to_part(&result, name.unwrap_or(&default_name), pa.material.clone());
    // The transform is baked into the verts, so the part sits at the model root.
    part.translation = [0.0; 3];
    part.rotation = Quat::IDENTITY.to_array();
    part.scale = [1.0; 3];
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
        let part = crate::primitives::primitive(crate::PrimitiveKind::Box, [half * 2.0; 3], name)
            .unwrap_or_else(|_| panic!("{name} failed"));
        model.parts.push(part.into());
        model
    }

    #[test]
    fn box_minus_smaller_box_has_more_triangles() {
        let mut model = model_with_box("big", 2.0);
        let small = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "small")
            .unwrap_or_else(|_| panic!("small failed"));
        model.parts.push(small.into());
        let before = model.part("big").map_or(0, |p| p.tri_count());
        let out = match boolean(&model, crate::BoolOp::Difference, "big", "small", None) {
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
            crate::primitives::primitive(crate::PrimitiveKind::Sphere, [1.0; 3], "b")
                .unwrap_or_else(|_| panic!("b failed"))
                .into(),
        );
        for op in [
            crate::BoolOp::Union,
            crate::BoolOp::Intersect,
            crate::BoolOp::Difference,
        ] {
            let out = match boolean(&model, op, "a", "b", None) {
                Ok(out) => out,
                Err(e) => panic!("{op} failed: {e}"),
            };
            validate(&out, op.as_str());
        }
    }

    #[test]
    fn disjoint_difference_errs() {
        let mut model = model_with_box("a", 1.0);
        let mut far = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "b")
            .unwrap_or_else(|_| panic!("b failed"));
        far.translation = [100.0, 0.0, 0.0];
        model.parts.push(far.into());
        assert!(boolean(&model, crate::BoolOp::Intersect, "a", "b", None).is_err());
    }

    #[test]
    fn transform_is_applied_before_the_boolean() {
        // A small box rotated 45° cutting a bigger box still carves a hole.
        let mut model = model_with_box("big", 2.0);
        let mut cutter =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "cutter")
                .unwrap_or_else(|_| panic!("cutter failed"));
        cutter.rotation = [0.0, 0.0, 0.3826834, 0.9238795]; // 45° around z
        model.parts.push(cutter.into());
        let out = match boolean(
            &model,
            crate::BoolOp::Difference,
            "big",
            "cutter",
            Some("hole"),
        ) {
            Ok(out) => out,
            Err(e) => panic!("rotated subtract failed: {e}"),
        };
        assert_eq!(out.name, "hole");
        validate(&out, "rotated-subtract");
    }

    #[test]
    fn unknown_part_and_op_err() {
        let model = model_with_box("a", 1.0);
        assert!(boolean(&model, crate::BoolOp::Union, "a", "ghost", None).is_err());
        assert!("xor".parse::<crate::BoolOp>().is_err());
    }

    #[test]
    fn partial_difference_is_closed_and_clean() {
        // A corner bite: the output must be a closed manifold with no
        // degenerate triangles, inside the big box's extents.
        let mut model = model_with_box("big", 2.0);
        let mut cutter =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [2.0, 1.0, 1.0], "cutter")
                .unwrap_or_else(|_| panic!("cutter failed"));
        cutter.translation = [1.0, 0.0, 1.5];
        model.parts.push(cutter.into());
        let out = match boolean(&model, crate::BoolOp::Difference, "big", "cutter", None) {
            Ok(out) => out,
            Err(e) => panic!("partial difference failed: {e}"),
        };
        assert_eq!(crate::verify::open_edge_count(&out), 0, "open edges");
        assert!(out.tri_count() > 0, "result must be non-empty");
        for tri in out.indices.as_chunks::<3>().0 {
            let (va, vb, vc) = (
                Vec3::from(out.verts[tri[0] as usize]),
                Vec3::from(out.verts[tri[1] as usize]),
                Vec3::from(out.verts[tri[2] as usize]),
            );
            assert!(
                (vb - va).cross(vc - va).length() >= 1e-9,
                "degenerate triangle {tri:?}"
            );
        }
        let (min, max) = out.bounds();
        assert!(min.x > -2.0 - 1e-5 && max.x < 2.0 + 1e-5);
        assert!(min.y > -2.0 - 1e-5 && max.y < 2.0 + 1e-5);
        assert!(min.z > -2.0 - 1e-5 && max.z < 2.0 + 1e-5);
        validate(&out, "partial-difference");
    }

    /// Curved-on-curved used to come out as noise under csgrs.
    #[test]
    fn curved_on_curved_is_manifold() {
        let mut model = Model::named("m");
        let mk = |kind, size, t: [f32; 3], r: [f32; 4], name: &str| {
            let mut p =
                crate::primitives::primitive(kind, size, name).unwrap_or_else(|e| panic!("{e}"));
            p.translation = t;
            p.rotation = r;
            p
        };
        let id = [0.0, 0.0, 0.0, 1.0];
        let x90 = [0.70710677, 0.0, 0.0, 0.70710677];
        model.parts.push(
            mk(
                crate::PrimitiveKind::Cylinder,
                [1.0, 2.0, 0.0],
                [0.0; 3],
                id,
                "c1",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Cylinder,
                [0.4, 3.0, 0.0],
                [0.0; 3],
                x90,
                "c2",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Sphere,
                [1.2, 0.0, 0.0],
                [0.0; 3],
                id,
                "s",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Torus,
                [1.0, 0.3, 0.0],
                [0.0; 3],
                id,
                "t",
            )
            .into(),
        );
        for (a, b) in [("c1", "c2"), ("s", "c1"), ("c1", "t"), ("s", "t")] {
            for op in [BoolOp::Difference, BoolOp::Union, BoolOp::Intersect] {
                let out = boolean(&model, op, a, b, None)
                    .unwrap_or_else(|e| panic!("{op}({a},{b}): {e}"));
                // Closed, and a valid solid for the next boolean (chained cuts).
                assert_eq!(
                    crate::verify::open_edge_count(&out),
                    0,
                    "{op}({a},{b}) has open edges"
                );
                part_to_manifold(&out).unwrap_or_else(|e| panic!("{op}({a},{b}) re-import: {e}"));
                validate(&out, op.as_str());
            }
        }
    }

    #[test]
    fn open_sheet_operand_errs_cleanly() {
        let mut model = model_with_box("a", 1.0);
        model.parts.push(
            crate::primitives::primitive(crate::PrimitiveKind::Disc, [0.5, 0.0, 0.0], "d")
                .unwrap_or_else(|e| panic!("{e}"))
                .into(),
        );
        let err = match boolean(&model, BoolOp::Difference, "a", "d", None) {
            Ok(_) => panic!("disc operand should error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("closed solid"), "{err}");
    }

    /// UVs ride through the boolean: the untouched faces of a box keep their
    /// own UV corners instead of being box-reprojected.
    #[test]
    fn input_uvs_survive_the_boolean() {
        let mut model = model_with_box("big", 2.0);
        let mut cutter = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "c")
            .unwrap_or_else(|e| panic!("{e}"));
        cutter.translation = [0.0, 0.0, 2.0]; // pocket into the +z face only
        model.parts.push(cutter.into());
        let big = model.part("big").unwrap_or_else(|| panic!("big"));
        // UV of the -z face's corner (-2,-2,-2) in the input.
        let want: Vec<[f32; 2]> = big
            .verts
            .iter()
            .zip(&big.uvs)
            .zip(&big.normals)
            .filter(|((v, _), n)| {
                (Vec3::from(**v) - Vec3::splat(-2.0)).length() < 1e-4 && n[2] < -0.5
            })
            .map(|((_, uv), _)| *uv)
            .collect();
        assert!(!want.is_empty());
        let out =
            boolean(&model, BoolOp::Difference, "big", "c", None).unwrap_or_else(|e| panic!("{e}"));
        let got: Vec<[f32; 2]> = out
            .verts
            .iter()
            .zip(&out.uvs)
            .zip(&out.normals)
            .filter(|((v, _), n)| {
                (Vec3::from(**v) - Vec3::splat(-2.0)).length() < 1e-4 && n[2] < -0.5
            })
            .map(|((_, uv), _)| *uv)
            .collect();
        for uv in &want {
            assert!(
                got.iter()
                    .any(|g| (g[0] - uv[0]).abs() < 1e-4 && (g[1] - uv[1]).abs() < 1e-4),
                "input uv {uv:?} lost; got {got:?}"
            );
        }
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

    /// Two cutters of equal depth into one wall used to make csgrs drop the
    /// first pocket's end face (coplanar but disjoint faces). Manifold handles
    /// it exactly, with no nudge.
    #[test]
    fn equal_depth_cutters_keep_both_pockets() {
        let mut model = Model::named("m");
        let mk = |kind, size, t: [f32; 3], name: &str| {
            let mut p =
                crate::primitives::primitive(kind, size, name).unwrap_or_else(|e| panic!("{e}"));
            p.translation = t;
            p
        };
        model.parts.push(
            mk(
                crate::PrimitiveKind::Cylinder,
                [0.8, 3.0, 0.0],
                [0.0, 1.7, 0.0],
                "tower",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Box,
                [0.5, 1.0, 0.7],
                [0.0, 0.7, 0.8],
                "door",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Box,
                [0.3, 0.4, 0.7],
                [0.0, 2.5, 0.8],
                "win",
            )
            .into(),
        );
        let d1 = boolean(&model, BoolOp::Difference, "tower", "door", Some("d1"))
            .unwrap_or_else(|e| panic!("{e}"));
        model.parts.push(d1.into());
        let out = boolean(&model, BoolOp::Difference, "d1", "win", Some("o"))
            .unwrap_or_else(|e| panic!("{e}"));
        // Ray down -z through each pocket must hit the pocket's end face
        // (z ~ 0.45) before the tower's back wall (z = -0.8).
        for y in [0.7f32, 2.5] {
            let first = first_hit_z(&out, y);
            assert!(
                first.is_some_and(|z| (z - 0.45).abs() < 0.02),
                "pocket at y={y} is open: first hit z = {first:?}"
            );
        }
    }

    /// z of the first triangle hit by a ray from (0, y, 2.5) along -z.
    fn first_hit_z(part: &Part, y: f32) -> Option<f32> {
        let (o, d) = (Vec3::new(0.0, y, 2.5), Vec3::new(0.0, 0.0, -1.0));
        let mut best: Option<f32> = None;
        for t in part.indices.as_chunks::<3>().0 {
            let p: Vec<Vec3> = t
                .iter()
                .map(|&i| Vec3::from(part.verts[i as usize]))
                .collect();
            let (e1, e2) = (p[1] - p[0], p[2] - p[0]);
            let h = d.cross(e2);
            let a = e1.dot(h);
            if a.abs() < 1e-9 {
                continue;
            }
            let f = 1.0 / a;
            let s = o - p[0];
            let u = f * s.dot(h);
            let q = s.cross(e1);
            let v = f * d.dot(q);
            if !(0.0..=1.0).contains(&u) || v < 0.0 || u + v > 1.0 {
                continue;
            }
            let tt = f * e2.dot(q);
            if tt > 1e-6 && best.is_none_or(|b| tt < b) {
                best = Some(tt);
            }
        }
        best.map(|t| o.z - t)
    }
}
