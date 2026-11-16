//! Primitive mesh generators. Built on bevy's `Meshable` primitives (their UVs and normals
//! are well-tested), extracted back into a flat [`Part`] (verts/normals/uvs/indices).

use crate::{Part, PrimitiveKind};
use bevy::math::Vec3;
use bevy::mesh::{
    Indices, Mesh, VertexAttributeValues,
    primitives::{MeshBuilder, Meshable, SphereKind},
};
use bevy::shape::{Annulus, Capsule3d, Circle, Cone, Cuboid, Cylinder, Plane3d, Sphere, Torus};

const SECTORS: u32 = 32;
const STACKS: u32 = 16;

/// A named primitive. `size` is full extents for "box" (and the plane's x/z extents), and
/// `[radius, height, 0]` for everything else (torus: `[ring radius, tube radius, 0]`;
/// washer: `[inner radius, outer radius, 0]`).
pub fn primitive(
    kind: PrimitiveKind,
    size: [f32; 3],
    name: &str,
) -> Result<Part, crate::ModelerError> {
    let mesh = match kind {
        PrimitiveKind::Box => Cuboid::new(size[0], size[1], size[2]).mesh().build(),
        PrimitiveKind::Sphere => Sphere { radius: size[0] }
            .mesh()
            .kind(SphereKind::Uv {
                sectors: SECTORS,
                stacks: STACKS,
            })
            .build(),
        PrimitiveKind::Cylinder => Cylinder::new(size[0], size[1]).mesh().build(),
        PrimitiveKind::Cone => Cone::new(size[0], size[1]).mesh().build(),
        PrimitiveKind::Torus => {
            let ring = size[0];
            let tube = size[1];
            if tube <= 0.0 || tube >= ring {
                return Err(format!("torus needs 0 < tube ({tube}) < ring radius ({ring})").into());
            }
            Torus::new(ring - tube, ring + tube).mesh().build()
        }
        PrimitiveKind::Capsule => Capsule3d::new(size[0], size[1]).mesh().build(),
        PrimitiveKind::Plane => Plane3d::default().mesh().size(size[0], size[1]).build(),
        PrimitiveKind::Disc => Circle { radius: size[0] }.mesh().build(),
        PrimitiveKind::Washer => {
            let (inner, outer) = (size[0], size[1]);
            if inner <= 0.0 || outer <= inner {
                return Err(format!("washer needs 0 < inner ({inner}) < outer ({outer})").into());
            }
            Annulus {
                inner_circle: Circle { radius: inner },
                outer_circle: Circle { radius: outer },
            }
            .mesh()
            .build()
        }
        // A triangular prism: a ramp/roof profile (triangle in XY) extruded
        // along Z. Hand-built; bevy has no primitive for it.
        PrimitiveKind::Wedge => {
            let (w, h, d) = (size[0], size[1], size[2]);
            if w <= 0.0 || h <= 0.0 || d <= 0.0 {
                return Err("wedge size must be positive".into());
            }
            let (x, z) = (w / 2.0, d / 2.0);
            let verts: Vec<[f32; 3]> = vec![
                [-x, -h / 2.0, z],  // 0 front-bottom-left
                [x, -h / 2.0, z],   // 1 front-bottom-right
                [x, -h / 2.0, -z],  // 2 back-bottom-right
                [-x, -h / 2.0, -z], // 3 back-bottom-left
                [-x, h / 2.0, z],   // 4 front-top-left
                [x, h / 2.0, z],    // 5 front-top-right
            ];
            // Outward-wound: bottom, front rectangle, back slope, two sides.
            let indices: Vec<u32> = vec![
                0, 2, 1, 0, 3, 2, // bottom (-y)
                0, 1, 5, 0, 5, 4, // front face (+z)
                4, 5, 2, 4, 2, 3, // slope from top edge to back bottom
                0, 4, 3, // left side (-x)
                1, 2, 5, // right side (+x)
            ];
            let mut part = Part::new(name);
            part.verts = verts;
            part.indices = indices;
            crate::uv::ensure_normals(&mut part);
            crate::uv::ensure_uvs(&mut part, Some("box"))?;
            return Ok(part);
        }
    };
    let mut part = Part::new(name);
    extract(&mesh, &mut part)?;
    if part.verts.is_empty() || part.tri_count() == 0 {
        return Err(format!("primitive `{kind}` produced an empty mesh").into());
    }
    // Open surfaces are double-sided: append a reversed copy so neither
    // orientation back-face-culls (and welded edges pair up in `validate`).
    if kind.is_open_sheet() {
        make_double_sided(&mut part);
    }
    Ok(part)
}

/// A one-sided plane: one quad (4 verts, 2 triangles, counter-clockwise
/// seen from the front) centred on the origin, its normal along `facing`.
/// `size` is [width, height] across the face: [x, z] for up/down, [z, y]
/// for ±x, [x, y] for ±z. UVs are 0..1 over the quad, oriented like a box
/// face's metric uvs (u to the viewer's right, v down) so a texture reads
/// the same on it as on the matching face of a box.
pub fn one_sided_plane(
    size: [f32; 3],
    facing: crate::Facing,
    name: &str,
) -> Result<Part, crate::ModelerError> {
    use crate::Facing as F;
    let (w, h) = (size[0], size[1]);
    if w <= 0.0 || h <= 0.0 || w.is_nan() || h.is_nan() {
        return Err(format!("plane size must be positive, got [{w}, {h}]").into());
    }
    let n = match facing {
        F::Up => Vec3::Y,
        F::Down => Vec3::NEG_Y,
        F::PosX => Vec3::X,
        F::NegX => Vec3::NEG_X,
        F::PosZ => Vec3::Z,
        F::NegZ => Vec3::NEG_Z,
    };
    // Viewer-right (`r`) and image-down (`d`) as metric_uv lays them out.
    let (r, d) = match facing {
        F::Up => (Vec3::X, Vec3::Z),
        F::Down => (Vec3::X, Vec3::NEG_Z),
        F::PosX => (Vec3::NEG_Z, Vec3::NEG_Y),
        F::NegX => (Vec3::Z, Vec3::NEG_Y),
        F::PosZ => (Vec3::X, Vec3::NEG_Y),
        F::NegZ => (Vec3::NEG_X, Vec3::NEG_Y),
    };
    let (hr, hd) = (r * w / 2.0, d * h / 2.0);
    // top-left, top-right, bottom-right, bottom-left as seen from the front.
    let corners = [-hr - hd, hr - hd, hr + hd, -hr + hd];
    let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let mut part = Part::new(name);
    part.verts = corners.iter().map(|c| c.to_array()).collect();
    part.normals = vec![n.to_array(); 4];
    part.uvs = uvs.to_vec();
    // r x d = -n for every facing, so (tl, bl, br) is counter-clockwise
    // seen from the front.
    debug_assert!(r.cross(d).dot(n) < 0.0);
    part.indices = vec![0, 3, 2, 0, 2, 1];
    part.sheet = true;
    Ok(part)
}

/// Append a second, reversed copy of the triangle set so the surface renders
/// from both sides. Triangles are first split into per-triangle verts: the
/// generated fan/quad sheets share interior edges, and after doubling every
/// welded edge must be incident to exactly 2 triangles (the front face and its
/// reversed twin) for `Session::validate` to call the mesh manifold. Original
/// triangle geometry (positions, normals, uvs, winding) is unchanged.
fn make_double_sided(part: &mut Part) {
    let base_verts = std::mem::take(&mut part.verts);
    let base_normals = std::mem::take(&mut part.normals);
    let base_uvs = std::mem::take(&mut part.uvs);
    let base_tris: Vec<[u32; 3]> = part.indices.as_chunks::<3>().0.to_vec();
    part.indices.clear();
    let emit = |part: &mut Part, t: &[u32; 3], reversed: bool| {
        let order: [usize; 3] = if reversed { [0, 2, 1] } else { [0, 1, 2] };
        let start = part.verts.len() as u32;
        for k in order {
            let i = t[k] as usize;
            part.verts.push(base_verts[i]);
            let n = base_normals[i];
            part.normals
                .push(if reversed { [-n[0], -n[1], -n[2]] } else { n });
            part.uvs.push(base_uvs[i]);
        }
        part.indices.extend([start, start + 1, start + 2]);
    };
    for t in &base_tris {
        emit(part, t, false);
    }
    for t in &base_tris {
        emit(part, t, true);
    }
}

/// Pull POSITION/NORMAL/UV_0 and the index buffer out of a bevy mesh into a [`Part`].
pub fn extract(mesh: &Mesh, part: &mut Part) -> Result<(), crate::ModelerError> {
    for (attr, out3) in [
        (Mesh::ATTRIBUTE_POSITION, &mut part.verts),
        (Mesh::ATTRIBUTE_NORMAL, &mut part.normals),
    ] {
        let values = mesh.try_attribute(attr).map_err(|e| e.to_string())?;
        *out3 = match values {
            VertexAttributeValues::Float32x3(v) => v.clone(),
            other => return Err(format!("attribute {attr:?} is not f32x3: {other:?}").into()),
        };
    }
    // Renormalize: floating point drift on generated meshes, mostly the cone's apex.
    for n in &mut part.normals {
        let v = Vec3::from(*n);
        if v.length_squared() > 1e-12 {
            *n = v.normalize().to_array();
        }
    }
    let values = mesh
        .try_attribute(Mesh::ATTRIBUTE_UV_0)
        .map_err(|e| e.to_string())?;
    part.uvs = match values {
        VertexAttributeValues::Float32x2(v) => v.clone(),
        other => return Err(format!("attribute UV_0 is not f32x2: {other:?}").into()),
    };
    part.indices = match mesh.indices() {
        Some(Indices::U16(idx)) => idx.iter().map(|i| *i as u32).collect(),
        Some(Indices::U32(idx)) => idx.clone(),
        None => (0..part.verts.len() as u32).collect(),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uv::validate_uvs;

    /// Build `kind`, failing the test with the builder's message if it errors.
    fn build(kind: &str, size: [f32; 3]) -> Part {
        let Ok(part) = kind
            .parse()
            .map_err(|e: String| e)
            .and_then(|k| primitive(k, size, kind).map_err(|e| e.to_string()))
        else {
            panic!("{kind} failed to build");
        };
        part
    }

    fn check(kind: &str, size: [f32; 3]) {
        let part = build(kind, size);
        assert!(part.tri_count() > 0, "{kind} has no triangles");
        assert_eq!(part.verts.len(), part.normals.len(), "{kind} normals");
        assert_eq!(part.verts.len(), part.uvs.len(), "{kind} uvs");
        assert!(
            part.indices
                .iter()
                .all(|i| (*i as usize) < part.verts.len()),
            "{kind} index out of range"
        );
        for n in &part.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            // A true apex vertex (the cone tip) has no defined direction: zero is fine.
            assert!(
                len < 1e-6 || (len - 1.0).abs() < 1e-4,
                "{kind} normal not normalized: {n:?}"
            );
        }
        validate_uvs(&part, kind);
    }

    #[test]
    fn primitive_counts_and_uv_ranges() {
        for (kind, size) in [
            ("box", [1.0, 2.0, 3.0]),
            ("sphere", [1.0, 0.0, 0.0]),
            ("cylinder", [1.0, 3.0, 0.0]),
            ("cone", [1.0, 3.0, 0.0]),
            ("torus", [2.0, 0.5, 0.0]),
            ("capsule", [1.0, 2.0, 0.0]),
            ("plane", [4.0, 6.0, 0.0]),
        ] {
            check(kind, size);
        }
    }

    #[test]
    fn unknown_kind_errors() {
        assert!("dodecahedron".parse::<PrimitiveKind>().is_err());
    }

    #[test]
    fn wedge_is_closed_and_correctly_wound() {
        let part = build("wedge", [1.0, 0.5, 2.0]);
        // 8 triangles, every edge shared by exactly two (closed manifold).
        assert_eq!(part.tri_count(), 8);
        let mut edges = std::collections::HashMap::new();
        for t in part.indices.as_chunks::<3>().0 {
            for e in 0..3 {
                let (a, b) = (t[e], t[(e + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_insert(0u32) += 1;
            }
        }
        assert!(edges.values().all(|&c| c == 2), "wedge edges must pair up");
    }

    #[test]
    fn disc_and_washer_are_flat() {
        let disc = build("disc", [1.0, 0.0, 0.0]);
        assert!(disc.verts.iter().all(|v| v[2].abs() < 1e-6));
        let washer = build("washer", [0.5, 1.0, 0.0]);
        assert!(washer.verts.iter().all(|v| v[2].abs() < 1e-6));
        assert!(build("washer", [0.5, 1.0, 0.0]).tri_count() > 8);
    }

    #[test]
    fn washer_rejects_bad_radii() {
        assert!(primitive(PrimitiveKind::Washer, [1.0, 0.5, 0.0], "x").is_err());
        assert!(primitive(PrimitiveKind::Washer, [0.0, 1.0, 0.0], "x").is_err());
    }

    #[test]
    fn torus_rejects_bad_tube() {
        assert!(primitive(PrimitiveKind::Torus, [1.0, 2.0, 0.0], "x").is_err());
    }

    /// Open surfaces are double-sided: a reversed copy of every triangle rides
    /// along, so no welded edge (positions quantized to i64, per the `validate`
    /// convention in commands.rs) is "naked" — each appears an even number of
    /// times, at least twice (interior edges 4: two per sheet), and every
    /// front triangle has an exact reversed positional twin. (Interior edges
    /// cannot be exactly 2: a connected sheet already shares them between two
    /// front triangles.)
    #[test]
    fn open_primitives_are_double_sided() {
        for (kind, size) in [
            ("disc", [1.0, 0.0, 0.0]),
            ("washer", [0.5, 1.0, 0.0]),
            ("plane", [4.0, 6.0, 0.0]),
        ] {
            let part = build(kind, size);
            let quantize = |i: u32| -> (i64, i64, i64) {
                let v = part.verts[i as usize];
                (
                    (v[0] * 1e5).round() as i64,
                    (v[1] * 1e5).round() as i64,
                    (v[2] * 1e5).round() as i64,
                )
            };
            let mut edges: std::collections::HashMap<((i64, i64, i64), (i64, i64, i64)), u32> =
                std::collections::HashMap::new();
            let tris: Vec<[(i64, i64, i64); 3]> = part
                .indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| [quantize(t[0]), quantize(t[1]), quantize(t[2])])
                .collect();
            for q in &tris {
                for e in 0..3 {
                    let (a, b) = (q[e], q[(e + 1) % 3]);
                    *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
                }
            }
            assert_eq!(tris.len() % 2, 0, "{kind} triangle count must be even");
            assert!(
                edges.values().all(|&c| c % 2 == 0 && c >= 2),
                "{kind} has naked boundary edges: {edges:?}"
            );
            // Each front triangle pairs with a reversed twin over the same
            // welded positions, so the surface renders from both sides.
            for q in &tris {
                let twin = [q[0], q[2], q[1]];
                assert!(
                    tris.contains(&twin),
                    "{kind} triangle {q:?} has no reversed twin"
                );
            }
        }
    }

    #[test]
    fn torus_ring_radius_is_respected() {
        let part = build("torus", [2.0, 0.5, 0.0]);
        let (min, max) = part.bounds();
        // Outer radius = ring + tube on the ring plane, tube radius vertically.
        assert!((max[0] - 2.5).abs() < 1e-3, "outer x {}", max[0]);
        assert!((max[2] - 2.5).abs() < 1e-3, "outer z {}", max[2]);
        assert!((min[1] + 0.5).abs() < 1e-3, "tube y min {}", min[1]);
        assert!((max[1] - 0.5).abs() < 1e-3, "tube y max {}", max[1]);
    }

    #[test]
    fn sphere_bounds_match_radius() {
        let part = build("sphere", [2.0, 0.0, 0.0]);
        let (min, max) = part.bounds();
        for axis in [0, 1, 2] {
            assert!(
                (max[axis] - 2.0).abs() < 1e-3,
                "axis {axis} max {}",
                max[axis]
            );
            assert!(
                (min[axis] + 2.0).abs() < 1e-3,
                "axis {axis} min {}",
                min[axis]
            );
        }
    }

    #[test]
    fn solid_primitives_count_as_closed() {
        for &kind in PrimitiveKind::ALL {
            if kind.is_open_sheet() {
                continue;
            }
            let size = match kind {
                PrimitiveKind::Torus => [1.0, 0.3, 0.0],
                _ => [0.5, 1.0, 0.7],
            };
            let p = primitive(kind, size, "p").unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                crate::verify::open_edge_count(&p),
                0,
                "{kind} reads as open"
            );
        }
    }

    const FACINGS: [(crate::Facing, [f32; 3]); 6] = [
        (crate::Facing::Up, [0.0, 1.0, 0.0]),
        (crate::Facing::Down, [0.0, -1.0, 0.0]),
        (crate::Facing::PosX, [1.0, 0.0, 0.0]),
        (crate::Facing::NegX, [-1.0, 0.0, 0.0]),
        (crate::Facing::PosZ, [0.0, 0.0, 1.0]),
        (crate::Facing::NegZ, [0.0, 0.0, -1.0]),
    ];

    #[test]
    fn one_sided_plane_is_one_quad_facing_its_way() {
        for (f, n) in FACINGS {
            let p = one_sided_plane([6.0, 4.0, 0.0], f, "q").unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(p.verts.len(), 4, "{f:?}");
            assert_eq!(p.tri_count(), 2, "{f:?}");
            assert!(p.sheet);
            assert!(p.normals.iter().all(|m| *m == n), "{f:?} normals");
            let n = Vec3::from(n);
            for t in p.indices.as_chunks::<3>().0 {
                let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]));
                let g = (b - a).cross(c - a);
                assert!(g.normalize().dot(n) > 0.999, "{f:?} winding");
                assert!((g.length() - 24.0).abs() < 1e-4, "{f:?} area");
            }
            // Flat across the face, the size it was asked for.
            let ext = |axis: Vec3| {
                let d: Vec<f32> = p.verts.iter().map(|v| Vec3::from(*v).dot(axis)).collect();
                d.iter().cloned().fold(f32::MIN, f32::max)
                    - d.iter().cloned().fold(f32::MAX, f32::min)
            };
            assert!(ext(n).abs() < 1e-6, "{f:?} flat");
            let (w_axis, h_axis) = match f {
                crate::Facing::Up | crate::Facing::Down => (Vec3::X, Vec3::Z),
                crate::Facing::PosX | crate::Facing::NegX => (Vec3::Z, Vec3::Y),
                _ => (Vec3::X, Vec3::Y),
            };
            assert!((ext(w_axis) - 6.0).abs() < 1e-5, "{f:?} width");
            assert!((ext(h_axis) - 4.0).abs() < 1e-5, "{f:?} height");
            validate_uvs(&p, "one-sided plane");
        }
        assert!(one_sided_plane([0.0, 1.0, 0.0], crate::Facing::Up, "q").is_err());
    }

    /// A one-sided plane laid on a box face gets the same metric (tile) uvs
    /// as that face, and its own 0..1 uvs run the same way as the face's.
    #[test]
    fn one_sided_plane_uvs_match_a_box_face() {
        for (f, n) in FACINGS {
            let n = Vec3::from(n);
            let size = match f {
                crate::Facing::Up | crate::Facing::Down => [6.0, 4.0],
                crate::Facing::PosX | crate::Facing::NegX => [4.0, 2.0],
                _ => [6.0, 2.0],
            };
            let mut quad =
                one_sided_plane([size[0], size[1], 0.0], f, "q").unwrap_or_else(|e| panic!("{e}"));
            // The box face with normal n is half the box's extent along n.
            let half = Vec3::new(3.0, 1.0, 2.0);
            for v in &mut quad.verts {
                *v = (Vec3::from(*v) + n * half).to_array();
            }
            let own: Vec<([f32; 3], [f32; 2])> = quad
                .verts
                .iter()
                .copied()
                .zip(quad.uvs.iter().copied())
                .collect();
            let mut cube = build("box", [6.0, 2.0, 4.0]);
            crate::uv::metric_box(&mut cube);
            crate::uv::metric_box(&mut quad);
            for (k, v) in quad.verts.iter().enumerate() {
                let Some(j) = cube.verts.iter().enumerate().position(|(j, c)| {
                    Vec3::from(*c).distance(Vec3::from(*v)) < 1e-5
                        && Vec3::from(cube.normals[j]).dot(n) > 0.999
                }) else {
                    panic!("{f:?}: no box corner at {v:?}");
                };
                let (a, b) = (quad.uvs[k], cube.uvs[j]);
                assert!(
                    (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5,
                    "{f:?}: quad uv {a:?} != box uv {b:?}"
                );
            }
            // The quad's own uvs are the metric ones fitted to 0..1.
            let (lo, hi) = quad
                .uvs
                .iter()
                .fold(([f32::MAX; 2], [f32::MIN; 2]), |(l, h), u| {
                    (
                        [l[0].min(u[0]), l[1].min(u[1])],
                        [h[0].max(u[0]), h[1].max(u[1])],
                    )
                });
            for (p, o) in &own {
                let Some(k) = quad.verts.iter().position(|v| v == p) else {
                    panic!("{f:?}: corner lost");
                };
                let m = quad.uvs[k];
                let fit = [
                    (m[0] - lo[0]) / (hi[0] - lo[0]),
                    (m[1] - lo[1]) / (hi[1] - lo[1]),
                ];
                assert!(
                    (o[0] - fit[0]).abs() < 1e-5 && (o[1] - fit[1]).abs() < 1e-5,
                    "{f:?}"
                );
            }
        }
    }

    #[test]
    fn facing_plane_op_parses_builds_and_validates() {
        let item = serde_json::json!({"type": "primitive", "name": "floor", "kind": "plane",
            "size": [6, 4], "facing": "up"});
        let Ok(crate::commands::BatchItem::Op(op)) = crate::spec::parse_item(0, &item) else {
            panic!("plane with facing should parse");
        };
        assert!(matches!(
            op,
            crate::Op::Primitive {
                facing: Some(crate::Facing::Up),
                ..
            }
        ));
        let bad = serde_json::json!({"type": "primitive", "name": "b", "kind": "box",
            "size": 1, "facing": "up"});
        assert!(crate::spec::parse_item(0, &bad).is_err());
        let wrong = serde_json::json!({"type": "primitive", "name": "b", "kind": "plane",
            "size": 1, "facing": "sideways"});
        assert!(crate::spec::parse_item(0, &wrong).is_err());
        let mut model = crate::Model::default();
        model.ops.push(op);
        model.refresh().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(model.parts[0].verts.len(), 4);
        assert!(model.parts[0].sheet);
        let v = crate::verify::validate(&model);
        assert!(v.contains("manifold, outward-wound"), "{v}");
        // Saved op stays small and parametric.
        let saved = serde_json::to_string(&model.ops[0]).unwrap_or_default();
        assert!(
            saved.contains("\"facing\":\"up\"") && saved.len() < 200,
            "{saved}"
        );
        let back: crate::Op = serde_json::from_str(&saved).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            back,
            crate::Op::Primitive {
                facing: Some(crate::Facing::Up),
                ..
            }
        ));
        // Exports as one quad whose material is not double-sided.
        let dir = std::env::temp_dir().join(format!("modeler-sheet-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sheet.gltf");
        crate::gltf::export_gltf(&model, &path).unwrap_or_else(|e| panic!("{e}"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(!text.contains("doubleSided"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
