//! Boolean operations on parts via csgrs. Both parts are transformed into model space
//! (translation/rotation/scale baked in), the op runs on triangle soup, and the result
//! comes back as a plain triangle `Part` with box-projected UVs and part `a`'s material.

use crate::{BoolOp, Model, ModelerError, Part, quat_of};
use bevy::math::{Quat, Vec3};
use csgrs::CSG;
use std::collections::HashMap;

fn run_op(op: BoolOp, a: &CSG<()>, b: &CSG<()>) -> CSG<()> {
    match op {
        BoolOp::Union => a.union(b),
        BoolOp::Difference => a.subtract(b),
        BoolOp::Intersect => a.intersect(b),
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
        if !n.is_finite() || n.length_squared() < 1e-12 {
            continue;
        }
        faces.push(i.to_vec());
    }
    // `polyhedron` panics on out-of-range indices, so the validation above must hold.
    debug_assert!(faces.iter().flatten().all(|k| *k < points.len()));
    CSG::polyhedron(&points, &faces)
}

/// csgrs emits polygon soup; the weld merges cut-boundary duplicates. Remaining seams
/// are possible and are reported by validate — acceptable.
/// Returns a flat triangle part (three verts per triangle), welded and sliver-free.
fn csg_to_part(csg: &CSG<()>, name: &str, material: crate::Material) -> Part {
    let mut part = triangle_soup(csg, name, material);
    weld_and_drop_slivers(&mut part);
    part
}

/// The unwelded csgrs output: one vertex triple per triangle.
fn triangle_soup(csg: &CSG<()>, name: &str, material: crate::Material) -> Part {
    let mut part = Part::new(name);
    part.material = material;
    for poly in csg.to_polygons() {
        for tri in poly.triangulate() {
            let base = part.verts.len() as u32;
            for v in &tri {
                let p = &v.pos;
                part.verts.push([p.x as f32, p.y as f32, p.z as f32]);
            }
            part.indices.extend([base, base + 1, base + 2]);
        }
    }
    part
}

/// Quantise a position onto the 1e-4 weld grid (the tolerance `validate` uses).
fn grid_key(v: &[f32; 3]) -> [i64; 3] {
    [
        (v[0] as f64 * 1e4).round() as i64,
        (v[1] as f64 * 1e4).round() as i64,
        (v[2] as f64 * 1e4).round() as i64,
    ]
}

/// Snap a quantised position back to its grid-cell centre as f32, so a vertex's
/// position agrees exactly with its key.
fn snap_to_grid(v: &[f32; 3]) -> [f32; 3] {
    let k = grid_key(v);
    [
        (k[0] as f32) / 1e4,
        (k[1] as f32) / 1e4,
        (k[2] as f32) / 1e4,
    ]
}

/// Weld near-duplicate vertices left behind by csgrs's triangle-soup output and drop
/// degenerate triangles. Vertices are merged on a 1e-4 quantisation grid (the
/// tolerance `validate` uses) and snapped to its centres, so a vertex's position
/// agrees exactly with its key. Normals and UVs are cleared — callers must
/// regenerate them (see `boolean`). O(n); cannot hang.
fn weld_and_drop_slivers(part: &mut Part) {
    let mut remap: HashMap<[i64; 3], u32> = HashMap::with_capacity(part.verts.len());
    let mut welded = Vec::with_capacity(part.verts.len());
    let mut to_welded = vec![0u32; part.verts.len()];
    for (i, v) in part.verts.iter().enumerate() {
        match remap.entry(grid_key(v)) {
            std::collections::hash_map::Entry::Occupied(e) => to_welded[i] = *e.get(),
            std::collections::hash_map::Entry::Vacant(e) => {
                to_welded[i] = welded.len() as u32;
                e.insert(welded.len() as u32);
                welded.push(snap_to_grid(v));
            }
        }
    }
    let mut seen: HashMap<[u32; 3], ()> = HashMap::with_capacity(part.indices.len());
    let mut indices = Vec::with_capacity(part.indices.len());
    for tri in part.indices.as_chunks::<3>().0 {
        let i = [
            to_welded[tri[0] as usize],
            to_welded[tri[1] as usize],
            to_welded[tri[2] as usize],
        ];
        // Drop triangles collapsed by welding, exact duplicates, and slivers
        // (near-zero area, computed on world-scale coords: verts are already in
        // local/model space).
        if i[0] == i[1] || i[1] == i[2] || i[0] == i[2] {
            continue;
        }
        let mut sorted = i;
        sorted.sort_unstable();
        if seen.insert(sorted, ()).is_some() {
            continue;
        }
        let (va, vb, vc) = (
            Vec3::from(welded[i[0] as usize]),
            Vec3::from(welded[i[1] as usize]),
            Vec3::from(welded[i[2] as usize]),
        );
        if (vb - va).cross(vc - va).length() < 1e-9 {
            continue;
        }
        indices.extend(i);
    }
    part.verts = welded;
    part.indices = indices;
    part.normals = vec![[0.0; 3]; part.verts.len()];
    part.uvs.clear();
}

/// Canonical, quantised plane keys of a part's world-space triangles:
/// (unit normal with a fixed sign, plane offset), 2 mm cells. Two faces with the
/// same key are coplanar (either orientation).
fn plane_keys(part: &Part) -> std::collections::HashSet<[i32; 4]> {
    let rot = quat_of(part.rotation);
    let scale = Vec3::from(part.scale);
    let t = Vec3::from(part.translation);
    let world = |i: u32| rot * (Vec3::from(part.verts[i as usize]) * scale) + t;
    let mut keys = std::collections::HashSet::new();
    for tri in part.indices.as_chunks::<3>().0 {
        if tri.iter().any(|&i| i as usize >= part.verts.len()) {
            continue;
        }
        let (a, b, c) = (world(tri[0]), world(tri[1]), world(tri[2]));
        let n = (b - a).cross(c - a);
        if !n.is_finite() || n.length_squared() < 1e-12 {
            continue;
        }
        let mut n = n.normalize();
        let mut d = n.dot(a);
        // Canonical sign: first significant component positive.
        let lead = if n.x.abs() > 1e-3 {
            n.x
        } else if n.y.abs() > 1e-3 {
            n.y
        } else {
            n.z
        };
        if lead < 0.0 {
            n = -n;
            d = -d;
        }
        const Q: f32 = 500.0;
        keys.insert([
            (n.x * 1000.0).round() as i32,
            (n.y * 1000.0).round() as i32,
            (n.z * 1000.0).round() as i32,
            (d * Q).round() as i32,
        ]);
    }
    keys
}

fn shares_plane(a: &Part, b: &Part) -> bool {
    let ka = plane_keys(a);
    plane_keys(b).iter().any(|k| ka.contains(k))
}

/// A copy of `b` translated by a tiny, deliberately irrational-looking offset
/// (about 2 mm) until none of its planes coincide with `a`'s.
fn nudge_off_shared_planes(a: &Part, b: &Part) -> Part {
    let mut out = b.clone();
    for attempt in 1..=4 {
        let k = attempt as f32;
        out = b.clone();
        out.translation = [
            b.translation[0] + 0.00231 * k,
            b.translation[1] + 0.00173 * k,
            b.translation[2] + 0.00197 * k,
        ];
        if !shares_plane(a, &out) {
            break;
        }
    }
    out
}

/// Boolean `op` on the named parts of `model`, returning a new part named
/// `name.unwrap_or("{a}-{op}-{b}")` with `a`'s material. Errors on unknown parts or ops
/// and on empty results (no overlap, coincident faces, ...).
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
    // csgrs drops faces when a face of `b` lies in the same plane as a
    // face of `a` without overlapping it (typical: two cutters of equal
    // depth cut into one wall). Nudge `b` a hair off such shared planes.
    let nudged_b;
    let pb = if shares_plane(pa, pb) {
        nudged_b = nudge_off_shared_planes(pa, pb);
        &nudged_b
    } else {
        pb
    };
    let csg_a = part_to_csg(pa);
    let csg_b = part_to_csg(pb);
    // csgrs panics on some degenerate inputs; surface that as an error instead
    // of taking the whole server down.
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_op(op, &csg_a, &csg_b)))
            .map_err(|_| {
                ModelerError::Csg(format!("csg panicked on {op}({a},{b}): degenerate input"))
            })?;
    let default_name = format!("{a}-{op}-{b}");
    let out_name = name.unwrap_or(&default_name);
    let mut part = csg_to_part(&result, out_name, pa.material.clone());
    if part.verts.is_empty() || part.tri_count() == 0 {
        return Err(ModelerError::Csg(format!(
            "{op}({a}, {b}) produced an empty mesh (no overlap?)"
        )));
    }
    // The transform is baked into the verts, so the part sits at the model root.
    part.translation = [0.0; 3];
    part.rotation = Quat::IDENTITY.to_array();
    part.scale = [1.0; 3];
    // Welding invalidated per-vertex data: rebuild normals first (the box UV
    // projection reads them), then re-project the UVs.
    crate::uv::crease_normals(&mut part, 40.0);
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
    fn weld_dedupes_verts_and_drops_degenerates() {
        // A partial overlap difference produces cut-boundary duplicates in the raw
        // soup; the weld must merge them and leave a clean indexed mesh.
        let mut model = model_with_box("big", 2.0);
        let mut cutter =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [2.0, 1.0, 1.0], "cutter")
                .unwrap_or_else(|_| panic!("cutter failed"));
        cutter.translation = [1.0, 0.0, 1.5]; // bites a corner off the big box
        model.parts.push(cutter.into());
        let out = match boolean(&model, crate::BoolOp::Difference, "big", "cutter", None) {
            Ok(out) => out,
            Err(e) => panic!("partial difference failed: {e}"),
        };
        let soup = triangle_soup(
            &run_op(
                crate::BoolOp::Difference,
                &part_to_csg(model.part("big").unwrap_or_else(|| panic!("big missing"))),
                &part_to_csg(
                    model
                        .part("cutter")
                        .unwrap_or_else(|| panic!("cutter missing")),
                ),
            ),
            "soup",
            out.material.clone(),
        );
        // Welding dedupes: strictly fewer verts than the per-triangle soup.
        assert!(
            out.verts.len() < soup.verts.len(),
            "welded {} verts vs unwelded {}",
            out.verts.len(),
            soup.verts.len()
        );
        // No two verts share a quantized key.
        // Hard edges split verts on purpose (crease normals), but a split pair
        // must differ in normal: (position, normal) is unique.
        let mut keys: Vec<([i64; 3], [i32; 3])> = out
            .verts
            .iter()
            .zip(&out.normals)
            .map(|(v, n)| {
                (
                    grid_key(v),
                    [
                        (n[0] * 100.0).round() as i32,
                        (n[1] * 100.0).round() as i32,
                        (n[2] * 100.0).round() as i32,
                    ],
                )
            })
            .collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "duplicate (position, normal) verts");
        // No degenerate triangles: distinct indices and real area.
        assert!(out.tri_count() > 0, "result must be non-empty");
        for tri in out.indices.as_chunks::<3>().0 {
            assert_ne!(tri[0], tri[1]);
            assert_ne!(tri[1], tri[2]);
            assert_ne!(tri[0], tri[2]);
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
        // Bounds: the corner bite stays within the big box's original extents.
        let (min, max) = out.bounds();
        assert!(min.x > -2.0 - 1e-5 && max.x < 2.0 + 1e-5);
        assert!(min.y > -2.0 - 1e-5 && max.y < 2.0 + 1e-5);
        assert!(min.z > -2.0 - 1e-5 && max.z < 2.0 + 1e-5);
        validate(&out, "partial-difference");
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
    /// first pocket's end face (coplanar but disjoint faces).
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
