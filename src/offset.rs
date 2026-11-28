//! Offset surfaces: every vertex moved along its normal by a distance, kept
//! even at bends (the move is divided by how far the vertex normal leans
//! off its faces, as a solidify's "even thickness" does), with vertices
//! split at seams moved together. For dense smooth meshes (a subdivided car
//! body), where a ball erosion is far too slow; `shell` uses it above its
//! exact limit and `deform` mode `offset` exposes it.

use std::collections::HashMap;

use bevy::math::Vec3;

/// How far a vertex may move relative to `d` where faces meet steeply
/// (a sharp edge would otherwise send it off to infinity).
const MAX_STRETCH: f32 = 2.5;

/// `verts` moved by `d` along their normals (+ along the outward normal of
/// a counter-clockwise-wound surface, - into it). `indices` are triangles.
pub fn offset_positions(verts: &[Vec3], indices: &[u32], d: f32) -> Vec<Vec3> {
    if verts.is_empty() || d == 0.0 {
        return verts.to_vec();
    }
    let (lo, hi) = verts.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(a, b), p| (a.min(*p), b.max(*p)),
    );
    let inv = 1.0 / ((hi - lo).max_element() * 1e-6).max(1e-9);
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let mut pos: Vec<Vec3> = Vec::new();
    let weld: Vec<u32> = verts
        .iter()
        .map(|p| {
            let k = [
                (p.x * inv).round() as i64,
                (p.y * inv).round() as i64,
                (p.z * inv).round() as i64,
            ];
            *ids.entry(k).or_insert_with(|| {
                pos.push(*p);
                (pos.len() - 1) as u32
            })
        })
        .collect();
    let tris: Vec<[usize; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|t| t.iter().all(|&i| (i as usize) < verts.len()))
        .map(|t| t.map(|i| weld[i as usize] as usize))
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    // Angle-weighted vertex normals, and each face's normal and corner
    // angles for the even-thickness factor.
    let mut n = vec![Vec3::ZERO; pos.len()];
    let mut faces: Vec<(Vec3, [f32; 3])> = Vec::with_capacity(tris.len());
    for t in &tris {
        let [a, b, c] = t.map(|i| pos[i]);
        let fnrm = (b - a).cross(c - a).normalize_or_zero();
        let ang = |p: Vec3, q: Vec3, r: Vec3| (q - p).angle_between(r - p);
        let w =
            [ang(a, b, c), ang(b, c, a), ang(c, a, b)].map(|x| if x.is_finite() { x } else { 0.0 });
        for k in 0..3 {
            n[t[k]] += fnrm * w[k];
        }
        faces.push((fnrm, w));
    }
    for v in &mut n {
        *v = v.normalize_or_zero();
    }
    let mut lean = vec![0.0f32; pos.len()];
    let mut wsum = vec![0.0f32; pos.len()];
    for (t, (fnrm, w)) in tris.iter().zip(&faces) {
        for k in 0..3 {
            lean[t[k]] += n[t[k]].dot(*fnrm) * w[k];
            wsum[t[k]] += w[k];
        }
    }
    let moved: Vec<Vec3> = (0..pos.len())
        .map(|i| {
            let c = if wsum[i] > 0.0 {
                lean[i] / wsum[i]
            } else {
                1.0
            };
            let s = (1.0 / c.max(1e-3)).min(MAX_STRETCH);
            pos[i] + n[i] * d * s
        })
        .collect();
    weld.iter().map(|&w| moved[w as usize]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sphere_offsets_to_a_sphere() {
        let p = crate::primitives::primitive(crate::PrimitiveKind::Sphere, [1.0, 1.0, 1.0], "s")
            .unwrap_or_else(|e| panic!("{e}"));
        let v: Vec<Vec3> = p.verts.iter().map(|v| Vec3::from(*v)).collect();
        let r0: f32 = v.iter().map(|p| p.length()).sum::<f32>() / v.len() as f32;
        for d in [-0.05, 0.05] {
            let o = offset_positions(&v, &p.indices, d);
            for (a, b) in v.iter().zip(&o) {
                let got = b.length() - a.length();
                assert!((got - d).abs() < 0.01 * r0, "{got} vs {d}");
            }
        }
    }

    #[test]
    fn a_box_corner_keeps_its_faces_even() {
        // Faces of a cube move by about d (even thickness), not d / sqrt(3)
        // along the corner normal.
        let p = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0, 1.0, 1.0], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let v: Vec<Vec3> = p.verts.iter().map(|v| Vec3::from(*v)).collect();
        let o = offset_positions(&v, &p.indices, -0.1);
        for q in &o {
            // A corner (0.5, 0.5, 0.5) moves inward; with the stretch the
            // move along each axis is near 0.1.
            assert!(q.abs().max_element() < 0.5, "{q}");
            assert!(q.abs().min_element() > 0.3, "{q}");
        }
        // Copies of one corner (split for normals) move together.
        let mut by: HashMap<[i32; 3], Vec3> = HashMap::new();
        for (a, b) in v.iter().zip(&o) {
            let k = a.to_array().map(|c| (c * 1000.0) as i32);
            if let Some(prev) = by.insert(k, *b) {
                assert!((prev - *b).length() < 1e-6);
            }
        }
    }

    fn dense_body() -> crate::Part {
        // A subdivided box: 6 x 4^5 quads, over the exact limit.
        let b = crate::primitives::primitive(crate::PrimitiveKind::Box, [2.0, 1.0, 1.0], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let def = crate::smooth::Smooth {
            mode: "subdiv".into(),
            levels: Some(5),
            ..Default::default()
        };
        let mut p = crate::smooth::smooth("body", &b, &def).unwrap_or_else(|e| panic!("{e}"));
        // Two more levels' worth of triangles: subdivide the result again.
        let def = crate::smooth::Smooth {
            mode: "subdiv".into(),
            levels: Some(1),
            ..Default::default()
        };
        p = crate::smooth::smooth("body", &p, &def).unwrap_or_else(|e| panic!("{e}"));
        p
    }

    #[test]
    fn shell_offsets_a_dense_body_evenly() {
        let p = dense_body();
        assert!(p.tri_count() > 20_000, "{}", p.tri_count());
        let m = crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}"));
        let t = 0.015;
        let s = crate::csg::shell("skin", &p, t, &[], None).unwrap_or_else(|e| panic!("{e}"));
        let sm = crate::csg::part_to_manifold(&s).unwrap_or_else(|e| panic!("{e}"));
        let want = m.surface_area() * t as f64;
        let got = sm.volume();
        assert!((got - want).abs() < 0.05 * want, "wall {got} vs {want}");
        // Two surfaces: the outer one and the cavity's.
        assert_eq!(sm.decompose().len(), 2, "a solid with a cavity");
        // Open faces need the exact method.
        let e = crate::csg::shell("skin", &p, t, &["top".into()], None).err();
        assert!(e.is_some_and(|e| e.to_string().contains("method exact")));
        // A wall thicker than the body folds and is refused.
        let e = crate::csg::shell("skin", &p, 0.6, &[], Some("offset")).err();
        assert!(e.is_some_and(|e| e.to_string().contains("folds")));
    }

    #[test]
    fn deform_offset_grows_a_body_outward() {
        let p = dense_body();
        let def = crate::deform::Deform {
            mode: "offset".into(),
            amount: Some(0.003),
            ..Default::default()
        };
        let o = crate::deform::deform("proud", &p, &def).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(o.tri_count(), p.tri_count(), "no refinement");
        let (a, b) = (
            crate::csg::part_to_manifold(&p).unwrap_or_else(|e| panic!("{e}")),
            crate::csg::part_to_manifold(&o).unwrap_or_else(|e| panic!("{e}")),
        );
        let want = a.surface_area() * 0.003;
        let got = b.volume() - a.volume();
        assert!((got - want).abs() < 0.05 * want, "{got} vs {want}");
    }
}
