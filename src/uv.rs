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

/// Angle-thresholded smooth normals by smoothing groups: around each vertex
/// the incident faces are flood-filled across shared edges whose dihedral is
/// within `degrees`, and each group gets one angle-weighted normal (corner
/// angle x face normal). Transitive, so a chamfer strip of faces each within
/// the threshold of the next shades as one band instead of zigzagging.
/// Vertices whose corners fall in different groups are split, so a hard edge
/// (a boolean cut) stays crisp while a faceted cylinder wall stays smooth.
/// Incidence is gathered by position (1e-5 grid), so verts split only for a UV
/// seam still share one smooth normal. Rewrites verts/normals/indices; UVs are
/// kept when present (split verts copy theirs).
pub fn crease_normals(part: &mut Part, degrees: f32) {
    use std::collections::HashMap;
    let count = part.verts.len();
    let tris = if part.indices.is_empty() {
        count / 3
    } else {
        part.indices.len() / 3
    };
    let idx = |i: usize| -> usize { part.indices.get(i).copied().unwrap_or(i as u32) as usize };
    let mut tri_ids = Vec::with_capacity(tris);
    let mut face_n: Vec<Vec3> = Vec::with_capacity(tris);
    // Group verts by position so UV-seam duplicates share incidence.
    let mut group_of_pos: HashMap<[i64; 3], usize> = HashMap::new();
    let group: Vec<usize> = part
        .verts
        .iter()
        .map(|v| {
            let k = [
                (v[0] as f64 * 1e5).round() as i64,
                (v[1] as f64 * 1e5).round() as i64,
                (v[2] as f64 * 1e5).round() as i64,
            ];
            let next = group_of_pos.len();
            *group_of_pos.entry(k).or_insert(next)
        })
        .collect();
    let ngroups = group_of_pos.len();
    // Edge (position groups) -> triangles using it.
    let mut edge_tris: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for t in 0..tris {
        let (a, b, c) = (idx(t * 3), idx(t * 3 + 1), idx(t * 3 + 2));
        if a >= count || b >= count || c >= count {
            continue;
        }
        let n = (Vec3::from(part.verts[b]) - Vec3::from(part.verts[a]))
            .cross(Vec3::from(part.verts[c]) - Vec3::from(part.verts[a]));
        let k = tri_ids.len();
        tri_ids.push([a, b, c]);
        // Near-degenerate slivers (|cross|^2 under 1e-12, the old cut-off)
        // have no trustworthy direction: they join no group and carry no weight.
        face_n.push(if n.length_squared() > 1e-12 {
            n.normalize()
        } else {
            Vec3::ZERO
        });
        let g = [group[a], group[b], group[c]];
        for s in 0..3 {
            let (x, y) = (g[s], g[(s + 1) % 3]);
            if x != y {
                edge_tris.entry((x.min(y), x.max(y))).or_default().push(k);
            }
        }
    }
    // Union-find over corners (3 * tri + slot).
    let mut parent: Vec<u32> = (0..tri_ids.len() as u32 * 3).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    let slot =
        |k: usize, g: usize| -> Option<usize> { (0..3).find(|&s| group[tri_ids[k][s]] == g) };
    let cos_limit = degrees.to_radians().cos();
    let mut keys: Vec<&(usize, usize)> = edge_tris.keys().collect();
    keys.sort_unstable();
    for key in keys {
        let ts = &edge_tris[key];
        for i in 0..ts.len() {
            for j in i + 1..ts.len() {
                let (t1, t2) = (ts[i], ts[j]);
                let (n1, n2) = (face_n[t1], face_n[t2]);
                if n1 == Vec3::ZERO || n2 == Vec3::ZERO || n1.dot(n2) < cos_limit {
                    continue;
                }
                for g in [key.0, key.1] {
                    if let (Some(s1), Some(s2)) = (slot(t1, g), slot(t2, g)) {
                        let a = find(&mut parent, (t1 * 3 + s1) as u32);
                        let b = find(&mut parent, (t2 * 3 + s2) as u32);
                        if a != b {
                            parent[a.max(b) as usize] = a.min(b);
                        }
                    }
                }
            }
        }
    }
    // Angle-weighted normal per group; per position, an all-faces fallback.
    let mut acc = vec![Vec3::ZERO; parent.len()];
    let mut pos_acc = vec![Vec3::ZERO; ngroups];
    for (k, ids) in tri_ids.iter().enumerate() {
        let p = ids.map(|v| Vec3::from(part.verts[v]));
        for s in 0..3 {
            let e1 = p[(s + 1) % 3] - p[s];
            let e2 = p[(s + 2) % 3] - p[s];
            let ang = e1.angle_between(e2);
            let w = if ang.is_finite() { ang } else { 0.0 };
            let r = find(&mut parent, (k * 3 + s) as u32) as usize;
            acc[r] += face_n[k] * w;
            pos_acc[group[ids[s]]] += face_n[k] * w;
        }
    }
    let mut verts = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let keep_uvs = part.uvs.len() == count;
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices = Vec::new();
    let mut remap: HashMap<(usize, [i32; 3]), u32> = HashMap::new();
    for (k, ids) in tri_ids.iter().enumerate() {
        for (s, &v) in ids.iter().enumerate() {
            let r = find(&mut parent, (k * 3 + s) as u32) as usize;
            let n = if acc[r].length_squared() > 1e-12 {
                acc[r].normalize()
            } else if pos_acc[group[v]].length_squared() > 1e-12 {
                pos_acc[group[v]].normalize()
            } else {
                Vec3::Y
            };
            let key = (
                v,
                [
                    (n.x * 1000.0).round() as i32,
                    (n.y * 1000.0).round() as i32,
                    (n.z * 1000.0).round() as i32,
                ],
            );
            let id = *remap.entry(key).or_insert_with(|| {
                verts.push(part.verts[v]);
                normals.push(n.to_array());
                if keep_uvs {
                    uvs.push(part.uvs[v]);
                }
                (verts.len() - 1) as u32
            });
            indices.push(id);
        }
    }
    part.verts = verts;
    part.normals = normals;
    part.indices = indices;
    part.uvs = uvs;
}

/// Project UVs with `projection` (default "box"). Keeps every UV in [0, 1].
pub fn ensure_uvs(part: &mut Part, projection: Option<&str>) -> Result<(), crate::ModelerError> {
    project(part, projection.unwrap_or("box"))
}

/// UV-project a part in place. Returns Err for an unknown projection or a degenerate mesh.
pub fn project(part: &mut Part, projection: &str) -> Result<(), crate::ModelerError> {
    if part.verts.is_empty() {
        return Err(format!("cannot project UVs of empty part `{}`", part.name).into());
    }
    if !matches!(
        projection,
        "planar" | "box" | "cylindrical" | "spherical" | "metric" | "face" | "label"
    ) {
        return Err(format!("unknown UV projection `{projection}`").into());
    }
    if projection == "metric" {
        metric_box(part);
        return Ok(());
    }
    if matches!(projection, "face" | "label") {
        face_box(part);
        return Ok(());
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
            // Project along the smallest-extent axis (a flat plate's normal),
            // oriented as seen from its positive side: u right, v down.
            let axis = smallest_axis(size);
            let sign = [1.0, 1.0, 1.0];
            part.verts
                .iter()
                .map(|v| {
                    fit_uv(
                        metric_uv(axis as u8, sign[axis], Vec3::from(*v)),
                        axis,
                        min,
                        max,
                    )
                })
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
            return Err(format!("unknown UV projection `{projection}`").into());
        }
    };
    for uv in &mut part.uvs {
        uv[0] = uv[0].clamp(0.0, 1.0);
        uv[1] = uv[1].clamp(0.0, 1.0);
    }
    Ok(())
}

/// The metric uv of world-scaled point `w` on a face whose normal's
/// dominant axis is `axis` (0 x, 1 y, 2 z) with `sign`, oriented like an
/// image seen from outside: u right, v *down* (glTF: v = 0 is the top row),
/// so textures read upright on walls and unmirrored on every face.
pub fn metric_uv(axis: u8, sign: f32, w: Vec3) -> [f32; 2] {
    match axis {
        0 => [-sign * w.z, -w.y],
        1 => [w.x, sign * w.z],
        _ => [sign * w.x, -w.y],
    }
}

/// Box projection at real scale: each triangle is projected onto the plane
/// facing its own normal's dominant axis, with UVs in meters (the part's
/// scale applied), so a texture tiling every `t` meters looks the same size
/// on every face of every part. Corners shared by triangles that pick
/// different axes are split. Negative-facing faces flip u so nothing reads
/// mirrored from outside.
pub fn metric_box(part: &mut Part) {
    metric_box_groups(part);
}

/// `face` (alias `label`) projection: each triangle mapped by its outward
/// normal's dominant axis like [`metric_box`] (u viewer-right, v down the
/// image; side faces upright with world +y, the top with the image top
/// toward -z, the bottom toward +z), then stretched to 0..1 over that face
/// group's extent, so one image fills each side of a box and reads upright
/// and unmirrored from outside every face.
pub fn face_box(part: &mut Part) {
    let groups = metric_box_groups(part);
    let mut lo = [[f32::MAX; 2]; 6];
    let mut hi = [[f32::MIN; 2]; 6];
    for (uv, &g) in part.uvs.iter().zip(&groups) {
        for k in 0..2 {
            lo[g as usize][k] = lo[g as usize][k].min(uv[k]);
            hi[g as usize][k] = hi[g as usize][k].max(uv[k]);
        }
    }
    for (uv, &g) in part.uvs.iter_mut().zip(&groups) {
        for k in 0..2 {
            uv[k] = remap(
                uv[k],
                lo[g as usize][k],
                hi[g as usize][k] - lo[g as usize][k],
            );
        }
    }
    if part.normals.len() != part.verts.len() {
        let uvs = std::mem::take(&mut part.uvs);
        ensure_normals(part);
        part.uvs = uvs;
    }
}

/// Planar uv of a model-space point already in metric orientation, fitted to
/// 0..1 over the part's box on the two axes across `axis`.
fn fit_uv(m: [f32; 2], axis: usize, min: Vec3, max: Vec3) -> [f32; 2] {
    // Range of each metric coordinate over the box corners.
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for c in 0..8 {
        let p = Vec3::new(
            if c & 1 == 0 { min.x } else { max.x },
            if c & 2 == 0 { min.y } else { max.y },
            if c & 4 == 0 { min.z } else { max.z },
        );
        let q = metric_uv(axis as u8, 1.0, p);
        for k in 0..2 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    [
        remap(m[0], lo[0], hi[0] - lo[0]),
        remap(m[1], lo[1], hi[1] - lo[1]),
    ]
}

/// [`metric_box`], returning each new vertex's face group (axis * 2 + negative).
fn metric_box_groups(part: &mut Part) -> Vec<u8> {
    use std::collections::HashMap;
    let sc = Vec3::from(part.scale);
    let count = part.verts.len();
    let tris = if part.indices.is_empty() {
        count / 3
    } else {
        part.indices.len() / 3
    };
    let idx = |i: usize| -> usize { part.indices.get(i).copied().unwrap_or(i as u32) as usize };
    let has_n = part.normals.len() == count;
    let (mut verts, mut normals, mut uvs, mut indices) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut remap: HashMap<(usize, u8), u32> = HashMap::new();
    let mut groups = Vec::new();
    for t in 0..tris {
        let c = [idx(t * 3), idx(t * 3 + 1), idx(t * 3 + 2)];
        if c.iter().any(|&k| k >= count) {
            continue;
        }
        let p = c.map(|k| Vec3::from(part.verts[k]) * sc);
        let n = (p[1] - p[0]).cross(p[2] - p[0]);
        let a = n.abs();
        let (axis, sign) = if a.x >= a.y && a.x >= a.z {
            (0u8, n.x.signum())
        } else if a.y >= a.z {
            (1, n.y.signum())
        } else {
            (2, n.z.signum())
        };
        for (corner, &k) in c.iter().enumerate() {
            let key = (k, axis * 2 + u8::from(sign < 0.0));
            let id = *remap.entry(key).or_insert_with(|| {
                let w = p[corner];
                let uv = metric_uv(axis, sign, w);
                verts.push(part.verts[k]);
                if has_n {
                    normals.push(part.normals[k]);
                }
                uvs.push(uv);
                groups.push(key.1);
                (verts.len() - 1) as u32
            });
            indices.push(id);
        }
    }
    part.verts = verts;
    part.normals = normals;
    part.uvs = uvs;
    part.indices = indices;
    groups
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

fn smallest_axis(v: Vec3) -> usize {
    if v.x <= v.y && v.x <= v.z {
        0
    } else if v.y <= v.z {
        1
    } else {
        2
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
    #[test]
    fn metric_uvs_are_meters_on_every_face() {
        let mut p = crate::primitives::primitive(crate::PrimitiveKind::Box, [2.0, 1.0, 0.5], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        p.scale = [1.0, 3.0, 1.0];
        super::project(&mut p, "metric").unwrap_or_else(|e| panic!("{e}"));
        let span = |k: usize| {
            let (lo, hi) = p.uvs.iter().fold((f32::MAX, f32::MIN), |(a, b), uv| {
                (a.min(uv[k]), b.max(uv[k]))
            });
            hi - lo
        };
        // The front face spans 2 m across and 3 m up (scale applied).
        assert!((span(0) - 2.0).abs() < 1e-5, "{}", span(0));
        assert!((span(1) - 3.0).abs() < 1e-5, "{}", span(1));
        assert_eq!(p.uvs.len(), p.verts.len());
    }

    #[test]
    fn crease_groups_are_transitive_along_a_chamfer() {
        // Floor y = 0 (x < 0), wall x = 0.1 (y > 0.1), joined by a 45 degree
        // chamfer band; four sections along z.
        let prof = [[-1.0f32, 0.0], [0.0, 0.0], [0.1, 0.1], [0.1, 1.0]];
        let strip = |deg: f32| {
            let mut p = Part::new("chamfer");
            for k in 0..5 {
                for [x, y] in prof {
                    p.verts.push([x, y, -0.2 * k as f32]);
                }
            }
            for k in 0..4u32 {
                for i in 0..3u32 {
                    let (a, b) = (k * 4 + i, k * 4 + i + 1);
                    let (c, d) = (a + 4, b + 4);
                    p.indices.extend([a, b, d, a, d, c]);
                }
            }
            crease_normals(&mut p, deg);
            p
        };
        let normals_at = |p: &Part, x: f32, y: f32| {
            let mut ns: Vec<[i32; 3]> = p
                .verts
                .iter()
                .zip(&p.normals)
                .filter(|(v, _)| (v[0] - x).abs() < 1e-6 && (v[1] - y).abs() < 1e-6)
                .map(|(_, n)| n.map(|c| (c * 1000.0).round() as i32))
                .collect();
            ns.sort_unstable();
            ns.dedup();
            ns
        };
        // 60: one group, the same normal all along each crease line.
        let p = strip(60.0);
        let g = |x, y| normals_at(&p, x, y);
        assert_eq!(g(0.0, 0.0).len(), 1, "{:?}", g(0.0, 0.0));
        assert_eq!(g(0.1, 0.1).len(), 1, "{:?}", g(0.1, 0.1));
        // Both crease lines read floor + chamfer (+ wall): no lone chamfer.
        assert!(!g(0.0, 0.0).contains(&[-707, 707, 0]));
        // 40: floor, chamfer and wall split cleanly.
        let p = strip(40.0);
        let g = |x, y| normals_at(&p, x, y);
        assert_eq!(g(0.0, 0.0).len(), 2, "{:?}", g(0.0, 0.0));
        assert_eq!(g(0.1, 0.1).len(), 2, "{:?}", g(0.1, 0.1));
        let s = 1000.0f32 * std::f32::consts::FRAC_1_SQRT_2;
        let ch = [(-s).round() as i32, s.round() as i32, 0];
        assert!(g(0.0, 0.0).contains(&ch) && g(0.1, 0.1).contains(&ch));
        assert!(g(0.0, 0.0).contains(&[0, 1000, 0]));
        assert!(g(0.1, 0.1).contains(&[-1000, 0, 0]));
    }

    #[test]
    fn crease_splits_hard_edges_and_keeps_flat_welded() {
        let mut p = Part::new("fold");
        p.verts = vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
        p.indices = vec![0, 1, 2, 0, 3, 1];
        crease_normals(&mut p, 40.0);
        assert!(p.verts.len() > 4, "90 degree edge must split verts");
        let mut q = Part::new("flat");
        q.verts = vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [1., 1., 0.]];
        q.indices = vec![0, 1, 2, 2, 1, 3];
        crease_normals(&mut q, 40.0);
        assert_eq!(q.verts.len(), 4, "coplanar faces stay welded");
    }

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
        for proj in ["planar", "box", "cylindrical", "spherical", "face", "label"] {
            let mut p = tetrahedron();
            let Ok(()) = project(&mut p, proj) else {
                panic!("{proj} failed on the tetrahedron");
            };
            validate_uvs(&p, proj);
        }
    }

    fn cube() -> Part {
        let mut p = Part::new("cube");
        for c in 0..8u32 {
            p.verts.push([
                if c & 1 == 0 { -0.5 } else { 0.5 },
                if c & 2 == 0 { -0.5 } else { 0.5 },
                if c & 4 == 0 { -0.5 } else { 0.5 },
            ]);
        }
        // Outward-wound quads (a, b, c, d counter-clockwise from outside).
        for q in [
            [1, 3, 7, 5],
            [0, 4, 6, 2],
            [2, 6, 7, 3],
            [0, 1, 5, 4],
            [4, 5, 7, 6],
            [0, 2, 3, 1],
        ] {
            p.indices.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
        }
        p
    }

    #[test]
    fn face_projection_reads_upright_from_outside_every_face() {
        for proj in ["face", "label"] {
            let mut p = cube();
            let Ok(()) = project(&mut p, proj) else {
                panic!("{proj} failed");
            };
            validate_uvs(&p, proj);
            for tri in p.indices.chunks(3) {
                let v = tri
                    .iter()
                    .map(|&i| Vec3::from(p.verts[i as usize]))
                    .collect::<Vec<_>>();
                let n = (v[1] - v[0]).cross(v[2] - v[0]).normalize();
                // Viewer frame from outside: up = +y on sides, -z on top, +z below.
                let up = if n.y > 0.5 {
                    Vec3::NEG_Z
                } else if n.y < -0.5 {
                    Vec3::Z
                } else {
                    Vec3::Y
                };
                let right = up.cross(n);
                for &i in tri {
                    let w = Vec3::from(p.verts[i as usize]);
                    let uv = p.uvs[i as usize];
                    let eu = w.dot(right) + 0.5;
                    let ev = 0.5 - w.dot(up);
                    assert!(
                        (uv[0] - eu).abs() < 1e-5 && (uv[1] - ev).abs() < 1e-5,
                        "{proj} face {n:?} vert {w:?}: uv {uv:?}, want [{eu}, {ev}]"
                    );
                }
            }
        }
    }

    #[test]
    fn planar_projects_along_the_thin_axis() {
        // A sign plate: 1 wide (x), 0.5 tall (y), 0.02 thick (z).
        let mut p = cube();
        for v in &mut p.verts {
            v[1] *= 0.5;
            v[2] *= 0.02;
        }
        let Ok(()) = project(&mut p, "planar") else {
            panic!("planar failed");
        };
        validate_uvs(&p, "planar");
        for (v, uv) in p.verts.iter().zip(&p.uvs) {
            assert!(
                (uv[0] - (v[0] + 0.5)).abs() < 1e-5,
                "u follows x: {v:?} {uv:?}"
            );
            assert!(
                (uv[1] - (0.5 - v[1] * 2.0)).abs() < 1e-5,
                "v runs down y: {v:?} {uv:?}"
            );
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
