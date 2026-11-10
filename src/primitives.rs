//! Primitive mesh generators. Built on bevy's `Meshable` primitives (their UVs and normals
//! are well-tested), extracted back into a flat [`Part`] (verts/normals/uvs/indices).

use crate::Part;
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
pub fn primitive(kind: &str, size: [f32; 3], name: &str) -> Result<Part, String> {
    let mesh = match kind {
        "box" => Cuboid::new(size[0], size[1], size[2]).mesh().build(),
        "sphere" => Sphere { radius: size[0] }
            .mesh()
            .kind(SphereKind::Uv {
                sectors: SECTORS,
                stacks: STACKS,
            })
            .build(),
        "cylinder" => Cylinder::new(size[0], size[1]).mesh().build(),
        "cone" => Cone::new(size[0], size[1]).mesh().build(),
        "torus" => {
            let ring = size[0];
            let tube = size[1];
            if tube <= 0.0 || tube >= ring {
                return Err(format!(
                    "torus needs 0 < tube ({tube}) < ring radius ({ring})"
                ));
            }
            Torus::new(ring - tube, ring + tube).mesh().build()
        }
        "capsule" => Capsule3d::new(size[0], size[1]).mesh().build(),
        "plane" => Plane3d::default().mesh().size(size[0], size[1]).build(),
        "disc" => Circle { radius: size[0] }.mesh().build(),
        "washer" => {
            let (inner, outer) = (size[0], size[1]);
            if inner <= 0.0 || outer <= inner {
                return Err(format!(
                    "washer needs 0 < inner ({inner}) < outer ({outer})"
                ));
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
        "wedge" => {
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
        other => return Err(format!("unknown primitive kind `{other}`")),
    };
    let mut part = Part::new(name);
    extract(&mesh, &mut part)?;
    if part.verts.is_empty() || part.tri_count() == 0 {
        return Err(format!("primitive `{kind}` produced an empty mesh"));
    }
    Ok(part)
}

/// Pull POSITION/NORMAL/UV_0 and the index buffer out of a bevy mesh into a [`Part`].
pub fn extract(mesh: &Mesh, part: &mut Part) -> Result<(), String> {
    for (attr, out3) in [
        (Mesh::ATTRIBUTE_POSITION, &mut part.verts),
        (Mesh::ATTRIBUTE_NORMAL, &mut part.normals),
    ] {
        let values = mesh.try_attribute(attr).map_err(|e| e.to_string())?;
        *out3 = match values {
            VertexAttributeValues::Float32x3(v) => v.clone(),
            other => return Err(format!("attribute {attr:?} is not f32x3: {other:?}")),
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
        other => return Err(format!("attribute UV_0 is not f32x2: {other:?}")),
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
        let Ok(part) = primitive(kind, size, kind) else {
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
        assert!(primitive("dodecahedron", [1.0; 3], "x").is_err());
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
        assert!(primitive("washer", [1.0, 0.5, 0.0], "x").is_err());
        assert!(primitive("washer", [0.0, 1.0, 0.0], "x").is_err());
    }

    #[test]
    fn torus_rejects_bad_tube() {
        assert!(primitive("torus", [1.0, 2.0, 0.0], "x").is_err());
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
}
