//! Normal and UV repair: accumulate per-face normals, and project UVs planar / box /
//! cylindrical / spherical onto a mesh, with angle-seam handling that never wraps mid-triangle.

use crate::Part;
use bevy::math::Vec3;

/// Recompute per-vertex normals by accumulating the face normal of every triangle that
/// touches the vertex and normalizing. With no indices, verts are read as a triangle fan.
pub fn ensure_normals(part: &mut Part) {
    let count = part.verts.len();
    part.normals = vec![[0.0; 3]; count];
    let tris = if part.indices.is_empty() {
        count / 3
    } else {
        part.indices.len() / 3
    };
    let index_at =
        |i: usize| -> usize { part.indices.get(i).copied().unwrap_or(i as u32) as usize };
    for t in 0..tris {
        let (a, b, c) = (index_at(t * 3), index_at(t * 3 + 1), index_at(t * 3 + 2));
        if a >= count || b >= count || c >= count {
            continue;
        }
        let (va, vb, vc) = (
            Vec3::from(part.verts[a]),
            Vec3::from(part.verts[b]),
            Vec3::from(part.verts[c]),
        );
        let n = (vb - va).cross(vc - va);
        for i in [a, b, c] {
            let acc = Vec3::from(part.normals[i]) + n;
            part.normals[i] = acc.to_array();
        }
    }
    for n in &mut part.normals {
        let v = Vec3::from(*n);
        *n = if v.length_squared() > 1e-12 {
            v.normalize().to_array()
        } else {
            [0.0, 1.0, 0.0]
        };
    }
}

/// Project UVs with `projection` (default "box"). Keeps every UV in [0, 1].
pub fn ensure_uvs(part: &mut Part, projection: Option<&str>) -> Result<(), String> {
    project(part, projection.unwrap_or("box"))
}

/// UV-project a part in place. Returns Err for an unknown projection or a degenerate mesh.
pub fn project(part: &mut Part, projection: &str) -> Result<(), String> {
    if part.verts.is_empty() {
        return Err(format!("cannot project UVs of empty part `{}`", part.name));
    }
    if !matches!(projection, "planar" | "box" | "cylindrical" | "spherical") {
        return Err(format!("unknown UV projection `{projection}`"));
    }
    // Box projection reads the face normals; make sure they exist.
    if part.normals.len() != part.verts.len() {
        ensure_normals(part);
    }
    // UVs live in the part's own mesh space, so this is local bounds, not the
    // transformed world bounds from `Part::bounds`.
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for v in &part.verts {
        min = min.min(Vec3::from(*v));
        max = max.max(Vec3::from(*v));
    }
    let size = max - min;
    part.uvs = match projection {
        "planar" => {
            // Project onto the plane perpendicular to the largest-extent axis.
            let axis = largest_axis(size);
            part.verts
                .iter()
                .map(|v| planar_uv(v, axis, min, size))
                .collect()
        }
        "box" => part
            .verts
            .iter()
            .zip(&part.normals)
            .map(|(v, n)| {
                let axis = largest_axis(Vec3::from(*n).abs());
                planar_uv(v, axis, min, size)
            })
            .collect(),
        "cylindrical" => {
            // u = angle around Y, v = y normalized. Triangles whose u wraps get their
            // corners split onto duplicated verts so no triangle ever spans the seam.
            let base: Vec<[f32; 2]> = part
                .verts
                .iter()
                .map(|v| {
                    let u = (v[0].atan2(v[2]) / std::f32::consts::TAU + 0.5).rem_euclid(1.0);
                    [u, remap(v[1], min[1], size[1].max(1e-6))]
                })
                .collect();
            unwrap_cylinder_seam(part, &base)
        }
        "spherical" => part
            .verts
            .iter()
            .map(|v| {
                let d = Vec3::from(*v).normalize_or_zero();
                let u = d.z.atan2(d.x) / std::f32::consts::TAU + 0.5;
                let v = d.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
                [u, v.clamp(0.0, 1.0)]
            })
            .collect(),
        _ => {
            return Err(format!("unknown UV projection `{projection}`"));
        }
    };
    for uv in &mut part.uvs {
        uv[0] = uv[0].clamp(0.0, 1.0);
        uv[1] = uv[1].clamp(0.0, 1.0);
    }
    Ok(())
}

/// Rewrite a part's triangles for cylinder mapping: where a triangle's u values wrap
/// (any pair differing by more than half a turn), the smaller values are bumped up by a
/// whole turn onto duplicated verts so the seam stays texture-continuous. Returns the
/// (possibly longer) UV list.
fn unwrap_cylinder_seam(part: &mut Part, base: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut verts = part.verts.clone();
    let mut normals = part.normals.clone();
    let mut uvs = base.to_vec();
    let mut indices: Vec<u32> = Vec::with_capacity(part.indices.len());
    let tris = if part.indices.is_empty() {
        part.verts.len() / 3
    } else {
        part.indices.len() / 3
    };
    for t in 0..tris {
        let at = |k: usize| -> usize {
            if part.indices.is_empty() {
                t * 3 + k
            } else {
                part.indices[t * 3 + k] as usize
            }
        };
        let i = [at(0), at(1), at(2)];
        if i.iter().any(|k| *k >= base.len()) {
            continue;
        }
        // Unwrap the three corner u's onto one continuous branch, relative to the first
        // corner: no pair may differ by more than half a turn after this.
        let mut u = [base[i[0]][0], base[i[1]][0], base[i[2]][0]];
        for k in 1..3 {
            while u[k] - u[0] > 0.5 {
                u[k] -= 1.0;
            }
            while u[k] - u[0] < -0.5 {
                u[k] += 1.0;
            }
        }
        // If the span is still wide, the first corner is the outlier across the seam.
        let mn = u[0].min(u[1]).min(u[2]);
        let mx = u[0].max(u[1]).max(u[2]);
        if mx - mn > 0.5 {
            let mid = (u[1] + u[2]) / 2.0;
            u[0] += if mid > u[0] { 1.0 } else { -1.0 };
        }
        for k in 0..3 {
            let idx = if (u[k] - uvs[i[k]][0]).abs() > 1e-6 {
                // Duplicate the vertex so this corner can carry a shifted u.
                verts.push(verts[i[k]]);
                if !normals.is_empty() {
                    normals.push(normals[i[k]]);
                }
                uvs.push([u[k], base[i[k]][1]]);
                (uvs.len() - 1) as u32
            } else {
                uvs[i[k]][0] = u[k];
                i[k] as u32
            };
            indices.push(idx);
        }
    }
    part.verts = verts;
    part.normals = normals;
    part.indices = indices;
    uvs
}

fn planar_uv(v: &[f32; 3], axis: usize, min: Vec3, size: Vec3) -> [f32; 2] {
    let other = |k: usize| remap(v[k], min[k], size[k]);
    if axis == 0 {
        [other(2), other(1)]
    } else if axis == 1 {
        [other(0), other(2)]
    } else {
        [other(0), other(1)]
    }
}

fn largest_axis(v: Vec3) -> usize {
    if v.x >= v.y && v.x >= v.z {
        0
    } else if v.y >= v.z {
        1
    } else {
        2
    }
}

/// Map coordinate `x` into [0, 1] over the bounds starting at `min` with extent `size`.
fn remap(x: f32, min: f32, size: f32) -> f32 {
    if size <= 1e-6 {
        0.0
    } else {
        ((x - min) / size).clamp(0.0, 1.0)
    }
}

/// Test helper: every UV in range, arrays match vert count.
pub fn validate_uvs(part: &crate::Part, what: &str) {
    assert_eq!(part.uvs.len(), part.verts.len(), "{what} uv count");
    assert_eq!(part.normals.len(), part.verts.len(), "{what} normal count");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw tetrahedron: no normals, no UVs.
    fn tetrahedron() -> Part {
        let mut p = Part::new("tetra");
        p.verts = vec![
            [1.0, 1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, 1.0, -1.0],
            [1.0, -1.0, -1.0],
        ];
        p.indices = vec![0, 1, 2, 0, 3, 1, 0, 2, 3, 1, 3, 2];
        p
    }

    #[test]
    fn normals_are_normalized_and_outward() {
        let mut p = tetrahedron();
        ensure_normals(&mut p);
        assert_eq!(p.normals.len(), p.verts.len());
        for n in &p.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "not normalized: {n:?}");
        }
        // Vertex 1 sits on the two faces with outward normals leaning +x.
        assert!(
            p.normals[1][0] > 0.0,
            "vertex 1 normal should lean +x: {:?}",
            p.normals[1]
        );
    }

    #[test]
    fn fan_without_indices() {
        let mut p = tetrahedron();
        p.indices.clear();
        ensure_normals(&mut p);
        for n in &p.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn projections_stay_in_unit_square() {
        for proj in ["planar", "box", "cylindrical", "spherical"] {
            let mut p = tetrahedron();
            let Ok(()) = project(&mut p, proj) else {
                panic!("{proj} failed on the tetrahedron");
            };
            validate_uvs(&p, proj);
        }
    }

    #[test]
    fn unknown_projection_errors() {
        let mut p = tetrahedron();
        assert!(project(&mut p, "triplanar-shader-magic").is_err());
    }

    #[test]
    fn cylindrical_seam_does_not_wrap_inside_a_triangle() {
        // A ring around Y whose last triangle crosses the angle wrap: after projection
        // no triangle may hold u values straddling the seam (pair differing by > 0.5).
        let mut p = Part::new("ring");
        let n = 8;
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            p.verts.push([a.cos(), 0.0, a.sin()]);
        }
        for i in 0..n {
            let a: u32 = i;
            let b = (i + 1) % n;
            p.indices.extend([a, b, a]); // degenerate but shares verts
        }
        p.indices.extend([n - 1, 0, 0]); // crosses the wrap
        let verts_before = p.verts.len();
        let Ok(()) = project(&mut p, "cylindrical") else {
            panic!("cylindrical projection failed");
        };
        validate_uvs(&p, "cylindrical-ring");
        assert!(
            p.verts.len() > verts_before,
            "the wrap-crossing triangle needed a split"
        );
        for tri in p.indices.as_chunks::<3>().0 {
            let us: Vec<f32> = tri.iter().map(|i| p.uvs[*i as usize][0]).collect();
            for pair in [(us[0], us[1]), (us[1], us[2]), (us[0], us[2])] {
                assert!((pair.0 - pair.1).abs() <= 0.5, "wrap flip in {us:?}");
            }
        }
    }

    #[test]
    fn cylindrical_seam_splits_nothing_when_unwrapped() {
        // An arc of quads that stays inside one turn of u (280°..350°, where the u=0 seam
        // at 270° is never crossed) keeps its vert count.
        let mut p = Part::new("band");
        let n = 6;
        for y in [0.0, 1.0] {
            for i in 0..n {
                let a = (280.0 + i as f32 * 12.0).to_radians();
                p.verts.push([a.cos(), y, a.sin()]);
            }
        }
        for i in 0..n - 1 {
            let a = i as u32;
            let b = a + 1;
            p.indices.extend([a, b, a + n as u32]);
            p.indices.extend([b, b + n as u32, a + n as u32]);
        }
        let verts_before = p.verts.len();
        let Ok(()) = project(&mut p, "cylindrical") else {
            panic!("cylindrical projection failed");
        };
        assert_eq!(
            p.verts.len(),
            verts_before,
            "no wrap here, no splits expected"
        );
    }

    #[test]
    fn empty_part_errors() {
        let mut p = Part::new("empty");
        assert!(project(&mut p, "box").is_err());
    }
}
