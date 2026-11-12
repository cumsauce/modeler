//! Smooth skin weights and CPU posing.
//!
//! Weights: every vertex starts on the bone whose envelope (the segment
//! head..tail, scaled by the bone's radius) is nearest, then the weights
//! diffuse over the mesh's own edges for about `blend` meters, so joints
//! blend smoothly and two legs side by side (connected only through the
//! hips) never share weights. The four strongest bones are kept, summing
//! to 1.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::math::{Mat4, Vec3};
use rayon::prelude::*;

use super::{BindDef, Binding, Skeleton};
use crate::{Part, quat_of};

fn seg_dist(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let l2 = ab.length_squared();
    let t = if l2 > 1e-12 {
        ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + ab * t)).length()
}

type Sparse = [(u16, f32); 4];

fn top4(acc: &mut [(u16, f32)]) -> Sparse {
    acc.sort_by_key(|a| a.0);
    let mut merged: Vec<(u16, f32)> = Vec::with_capacity(acc.len());
    for &(j, w) in acc.iter() {
        match merged.last_mut() {
            Some(m) if m.0 == j => m.1 += w,
            _ => merged.push((j, w)),
        }
    }
    merged.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out = [(0u16, 0.0f32); 4];
    let sum: f32 = merged.iter().take(4).map(|m| m.1).sum();
    for (k, m) in merged.iter().take(4).enumerate() {
        out[k] = (m.0, if sum > 0.0 { m.1 / sum } else { 0.0 });
    }
    out
}

static CACHE: Mutex<Option<HashMap<u64, Arc<Binding>>>> = Mutex::new(None);

fn key(part: &Part, skel: &Skeleton, def: &BindDef, name: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    for v in part
        .verts
        .iter()
        .flatten()
        .chain(&part.translation)
        .chain(&part.rotation)
        .chain(&part.scale)
    {
        v.to_bits().hash(&mut h);
    }
    part.indices.hash(&mut h);
    for b in &skel.bones {
        b.name.hash(&mut h);
        b.parent.hash(&mut h);
        for v in b
            .head
            .to_array()
            .iter()
            .chain(&b.tail.to_array())
            .chain(&[b.radius])
        {
            v.to_bits().hash(&mut h);
        }
    }
    serde_json::to_string(def).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Smooth weights of `part` (in its world placement) against `skel`.
pub fn smooth_weights(
    part: &Part,
    skel: &Skeleton,
    def: &BindDef,
    name: &str,
) -> Result<Binding, String> {
    if def.bone.is_some() {
        return Err("`bone` is for rigid binds; smooth weights pick bones themselves (use `paint` to force a region)".into());
    }
    let k = key(part, skel, def, name);
    if let Ok(g) = CACHE.lock()
        && let Some(b) = g.as_ref().and_then(|m| m.get(&k))
    {
        return Ok((**b).clone());
    }
    let n = part.verts.len();
    let world: Vec<Vec3> = (0..n).map(|i| part.world_vert(i)).collect();
    // Weld coincident vertices (uv seams, creases) so weights diffuse
    // across them and stay equal on both sides.
    let mut group = vec![0u32; n];
    let mut groups: Vec<Vec3> = Vec::new();
    {
        let (lo, hi) = world
            .iter()
            .fold((Vec3::MAX, Vec3::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let q = ((hi - lo).max_element() * 1e-6).max(1e-9);
        let mut map: HashMap<[i64; 3], u32> = HashMap::new();
        for (i, v) in world.iter().enumerate() {
            let c = [
                (v.x / q).round() as i64,
                (v.y / q).round() as i64,
                (v.z / q).round() as i64,
            ];
            let g = *map.entry(c).or_insert_with(|| {
                groups.push(*v);
                (groups.len() - 1) as u32
            });
            group[i] = g;
        }
    }
    let m = groups.len();
    let mut nbr: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut edge_sum = 0.0f64;
    let mut edges = 0usize;
    for t in part.indices.as_chunks::<3>().0 {
        let g = [
            group[t[0] as usize],
            group[t[1] as usize],
            group[t[2] as usize],
        ];
        for (a, b) in [(g[0], g[1]), (g[1], g[2]), (g[2], g[0])] {
            if a != b && !nbr[a as usize].contains(&b) {
                nbr[a as usize].push(b);
                nbr[b as usize].push(a);
                edge_sum += f64::from((groups[a as usize] - groups[b as usize]).length());
                edges += 1;
            }
        }
    }
    let paint: Vec<(u16, Vec3, Vec3)> = def
        .paint
        .iter()
        .map(|p| {
            skel.bone(&p.bone)
                .map(|i| (i as u16, Vec3::from(p.min), Vec3::from(p.max)))
                .ok_or_else(|| format!("paint: no bone `{}`", p.bone))
        })
        .collect::<Result<_, _>>()?;
    // Hard start: nearest envelope (distance in units of the bone radius),
    // unless painted.
    let mut w: Vec<Sparse> = groups
        .par_iter()
        .map(|p| {
            let mut out = [(0u16, 0.0f32); 4];
            if let Some((j, ..)) = paint
                .iter()
                .rev()
                .find(|(_, lo, hi)| p.cmpge(*lo).all() && p.cmple(*hi).all())
            {
                out[0] = (*j, 1.0);
                return out;
            }
            let mut best = (0usize, f32::MAX);
            for (j, b) in skel.bones.iter().enumerate() {
                let d = seg_dist(*p, b.head, b.tail) / b.radius;
                if d < best.1 {
                    best = (j, d);
                }
            }
            out[0] = (best.0 as u16, 1.0);
            out
        })
        .collect();
    // Diffuse: spreading by about sqrt(iterations) edges.
    let edge = if edges > 0 {
        (edge_sum / edges as f64) as f32
    } else {
        1.0
    };
    let mut radii: Vec<f32> = skel.bones.iter().map(|b| b.radius).collect();
    radii.sort_by(f32::total_cmp);
    let blend = def.blend.unwrap_or(radii[radii.len() / 2]);
    if blend.is_nan() || blend < 0.0 {
        return Err("blend must be zero or positive".into());
    }
    let iters = ((blend / edge.max(1e-6)).powi(2).round() as usize).min(200);
    for _ in 0..iters {
        w = (0..m)
            .into_par_iter()
            .map(|g| {
                let mut acc: Vec<(u16, f32)> = Vec::with_capacity(4 * (nbr[g].len() + 1));
                let own = 0.5;
                for &(j, x) in &w[g] {
                    if x > 0.0 {
                        acc.push((j, x * own));
                    }
                }
                let k = nbr[g].len().max(1) as f32;
                for &u in &nbr[g] {
                    for &(j, x) in &w[u as usize] {
                        if x > 0.0 {
                            acc.push((j, x * (1.0 - own) / k));
                        }
                    }
                }
                if nbr[g].is_empty() {
                    return w[g];
                }
                top4(&mut acc)
            })
            .collect();
    }
    let mut joints = Vec::with_capacity(n);
    let mut weights = Vec::with_capacity(n);
    for &g in &group {
        let s = w[g as usize];
        joints.push([s[0].0, s[1].0, s[2].0, s[3].0]);
        weights.push([s[0].1, s[1].1, s[2].1, s[3].1]);
    }
    let b = Binding {
        skeleton: skel.name.clone(),
        bind: name.to_string(),
        bone: None,
        joints,
        weights,
    };
    if let Ok(mut g) = CACHE.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        if map.len() > 256 {
            map.clear();
        }
        map.insert(k, Arc::new(b.clone()));
    }
    Ok(b)
}

/// The part posed by bone matrices (`Skeleton::skin_matrices`), baked into
/// world space (identity transform).
pub fn pose_part(part: &Part, b: &Binding, mats: &[Mat4]) -> Part {
    let mut out = part.clone();
    let r = quat_of(part.rotation);
    let n = part.verts.len();
    let mat_of = |i: usize| -> Mat4 {
        if let Some(bone) = b.bone {
            return mats.get(bone).copied().unwrap_or(Mat4::IDENTITY);
        }
        let (j, w) = (b.joints.get(i), b.weights.get(i));
        let (Some(j), Some(w)) = (j, w) else {
            return Mat4::IDENTITY;
        };
        let mut m = Mat4::ZERO;
        for k in 0..4 {
            if w[k] > 0.0 {
                m += mats.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k];
            }
        }
        m
    };
    let posed: Vec<([f32; 3], Option<[f32; 3]>)> = (0..n)
        .into_par_iter()
        .map(|i| {
            let m = mat_of(i);
            let p = m.transform_point3(part.world_vert(i));
            let nn = part.normals.get(i).map(|nv| {
                let wn = r * (Vec3::from(*nv) / Vec3::from(part.scale)).normalize_or_zero();
                m.transform_vector3(wn).normalize_or_zero().to_array()
            });
            (p.to_array(), nn)
        })
        .collect();
    out.verts = posed.iter().map(|p| p.0).collect();
    if !part.normals.is_empty() {
        out.normals = posed
            .iter()
            .map(|p| p.1.unwrap_or([0.0, 1.0, 0.0]))
            .collect();
    }
    out.translation = [0.0; 3];
    out.rotation = [0.0, 0.0, 0.0, 1.0];
    out.scale = [1.0; 3];
    out
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;
    use bevy::math::Quat;

    fn bar() -> Part {
        // A 2 m bar along +x, finely subdivided, 0.2 thick.
        let mut p = Part::new("bar");
        let segs = 40;
        for i in 0..=segs {
            let x = 2.0 * i as f32 / segs as f32;
            for (y, z) in [(-0.1, -0.1), (0.1, -0.1), (0.1, 0.1), (-0.1, 0.1)] {
                p.verts.push([x, y, z]);
            }
        }
        for i in 0..segs as u32 {
            for k in 0..4u32 {
                let a = i * 4 + k;
                let b = i * 4 + (k + 1) % 4;
                let (c, d) = (a + 4, b + 4);
                p.indices.extend([a, b, d, a, d, c]);
            }
        }
        p
    }

    fn skel() -> Skeleton {
        let def = SkeletonDef {
            bones: vec![
                BoneDef {
                    name: "a".into(),
                    parent: None,
                    head: PointRef::Point([0.0, 0.0, 0.0]),
                    tail: None,
                    radius: Some(0.1),
                },
                BoneDef {
                    name: "b".into(),
                    parent: Some("a".into()),
                    head: PointRef::Point([1.0, 0.0, 0.0]),
                    tail: Some(PointRef::Point([2.0, 0.0, 0.0])),
                    radius: Some(0.1),
                },
            ],
            mirror: None,
            ..Default::default()
        };
        build("s", &def, &[], &[]).unwrap_or_else(|e| panic!("{e}"))
    }

    fn bind() -> BindDef {
        BindDef {
            skeleton: "s".into(),
            parts: vec!["bar".into()],
            bone: None,
            mode: "smooth".into(),
            blend: Some(0.15),
            paint: Vec::new(),
        }
    }

    #[test]
    fn two_bone_bar_bends() {
        let (p, s) = (bar(), skel());
        let b = smooth_weights(&p, &s, &bind(), "t").unwrap_or_else(|e| panic!("{e}"));
        for w in &b.weights {
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        }
        // Ends are fully on their bone, the joint blends.
        assert_eq!(b.joints[0][0], 0);
        assert!(b.weights[0][0] > 0.99);
        let last = p.verts.len() - 1;
        assert_eq!(b.joints[last][0], 1);
        let mid = 20 * 4;
        assert!(
            b.weights[mid][0] < 0.9,
            "joint should blend: {:?}",
            b.weights[mid]
        );
        let mut pose = s.rest();
        pose[1].1 = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let posed = pose_part(&p, &b, &s.skin_matrices(&pose));
        // The tip swung up to about (1, 1).
        let tip = Vec3::from(posed.verts[last]);
        let want = Vec3::X
            + Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)
                * (Vec3::from(p.verts[last]) - Vec3::X);
        assert!((tip - want).length() < 1e-4, "{tip:?} vs {want:?}");
        // The root end didn't move.
        assert!((Vec3::from(posed.verts[0]) - Vec3::from(p.verts[0])).length() < 1e-4);
    }

    #[test]
    fn weights_do_not_bleed_between_separate_legs() {
        // Two bars 0.05 apart, not connected: each takes only its own bone.
        let mut p = bar();
        let q = bar();
        let off = p.verts.len() as u32;
        for v in &q.verts {
            p.verts.push([v[0], v[1] + 0.25, v[2]]);
        }
        p.indices.extend(q.indices.iter().map(|i| i + off));
        let def = SkeletonDef {
            bones: vec![
                BoneDef {
                    name: "l".into(),
                    parent: None,
                    head: PointRef::Point([0.0, 0.0, 0.0]),
                    tail: Some(PointRef::Point([2.0, 0.0, 0.0])),
                    radius: Some(0.1),
                },
                BoneDef {
                    name: "r".into(),
                    parent: None,
                    head: PointRef::Point([0.0, 0.25, 0.0]),
                    tail: Some(PointRef::Point([2.0, 0.25, 0.0])),
                    radius: Some(0.1),
                },
            ],
            mirror: None,
            ..Default::default()
        };
        let s = build("s", &def, &[], &[]).unwrap_or_else(|e| panic!("{e}"));
        let mut d = bind();
        d.blend = Some(1.0);
        let b = smooth_weights(&p, &s, &d, "t").unwrap_or_else(|e| panic!("{e}"));
        for i in 0..p.verts.len() {
            let want = if (i as u32) < off { 0 } else { 1 };
            assert_eq!(b.joints[i][0], want);
            assert!(b.weights[i][0] > 0.999, "{i}: {:?}", b.weights[i]);
        }
    }
}
