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

use bevy::math::{Mat4, Quat, Vec3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

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
    let (mut verts, mut normals) = (Vec::new(), Vec::new());
    pose_into(part, b, mats, &mut verts, &mut normals);
    Part {
        name: part.name.clone(),
        verts,
        normals: if part.normals.is_empty() {
            Vec::new()
        } else {
            normals
        },
        uvs: part.uvs.clone(),
        indices: part.indices.clone(),
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
        material: part.material.clone(),
        visible: part.visible,
        occluders: part.occluders.clone(),
        detail: part.detail.clone(),
        cards: part.cards,
        sheet: part.sheet,
        rig: part.rig.clone(),
        branches: part.branches.clone(),
        joints: part.joints.clone(),
        skin: part.skin.clone(),
        morphs: None,
        correctives: None,
        cage: None,
    }
}

/// Area-weighted vertex normals of a mesh (unit, zero where no face).
fn vertex_normals(verts: &[[f32; 3]], indices: &[u32]) -> Vec<Vec3> {
    let mut n = vec![Vec3::ZERO; verts.len()];
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| i as usize);
        let (Some(pa), Some(pb), Some(pc)) = (verts.get(a), verts.get(b), verts.get(c)) else {
            continue;
        };
        let f = (Vec3::from(*pb) - Vec3::from(*pa)).cross(Vec3::from(*pc) - Vec3::from(*pa));
        n[a] += f;
        n[b] += f;
        n[c] += f;
    }
    n.iter().map(|v| v.normalize_or_zero()).collect()
}

/// The part's own vertices and normals with morph targets applied:
/// `verts + sum(w * delta)` for each (shape index, weight), normals bent
/// by the turn of the face normals around them (so creases and seams keep their
/// split). `None` when no weight applies (no work, the part unchanged).
pub fn morphed(part: &Part, weights: &[(usize, f32)]) -> Option<(Vec<[f32; 3]>, Vec<[f32; 3]>)> {
    let m = part.morphs.as_ref()?;
    let n = part.verts.len();
    let used: Vec<(&[[f32; 3]], f32)> = weights
        .iter()
        .filter(|(_, w)| *w != 0.0)
        .filter_map(|(i, w)| m.get(*i).map(|d| (d.1.as_slice(), *w)))
        .filter(|(d, _)| d.len() == n)
        .collect();
    if used.is_empty() {
        return None;
    }
    let verts: Vec<[f32; 3]> = (0..n)
        .into_par_iter()
        .map(|i| {
            let mut v = Vec3::from(part.verts[i]);
            for (d, w) in &used {
                v += Vec3::from(d[i]) * *w;
            }
            v.to_array()
        })
        .collect();
    let normals = if part.normals.len() == n {
        let before = vertex_normals(&part.verts, &part.indices);
        let after = vertex_normals(&verts, &part.indices);
        (0..n)
            .map(|i| {
                let base = Vec3::from(part.normals[i]);
                if before[i] == Vec3::ZERO || after[i] == Vec3::ZERO {
                    return base.to_array();
                }
                (bevy::math::Quat::from_rotation_arc(before[i], after[i]) * base).to_array()
            })
            .collect()
    } else {
        part.normals.clone()
    };
    Some((verts, normals))
}

/// A normal carried by `m`: the cofactor (inverse transpose, scaled by
/// the determinant) of its 3x3, so scaled bones shade right; normalised.
fn carry_normal(m: &Mat4, n: Vec3) -> Vec3 {
    let (c0, c1, c2) = (
        m.x_axis.truncate(),
        m.y_axis.truncate(),
        m.z_axis.truncate(),
    );
    let (a, b, c) = (c1.cross(c2), c2.cross(c0), c0.cross(c1));
    let det = c0.dot(a);
    let v = a * n.x + b * n.y + c * n.z;
    if det < 0.0 { -v } else { v }.normalize_or_zero()
}

/// Poses `part` into caller buffers: world-space positions into `verts`
/// and normals into `normals` (left empty when the part has none). The
/// per-frame path for the window, which needs nothing else of the part.
pub fn pose_into(
    part: &Part,
    b: &Binding,
    mats: &[Mat4],
    verts: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
) {
    if let Some(fixed) = corrected(part, mats) {
        return pose_into(&fixed, b, mats, verts, normals);
    }
    let r = quat_of(part.rotation);
    let n = part.verts.len();
    let scale = Vec3::from(part.scale);
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
    verts.clear();
    normals.clear();
    let has_n = !part.normals.is_empty();
    if has_n {
        let pairs: Vec<([f32; 3], [f32; 3])> = (0..n)
            .into_par_iter()
            .map(|i| {
                let m = mat_of(i);
                let p = m.transform_point3(part.world_vert(i));
                let nn = part.normals.get(i).map_or([0.0, 1.0, 0.0], |nv| {
                    let wn = r * (Vec3::from(*nv) / scale).normalize_or_zero();
                    carry_normal(&m, wn).to_array()
                });
                (p.to_array(), nn)
            })
            .collect();
        verts.reserve(n);
        normals.reserve(n);
        for (p, q) in pairs {
            verts.push(p);
            normals.push(q);
        }
    } else {
        (0..n)
            .into_par_iter()
            .map(|i| mat_of(i).transform_point3(part.world_vert(i)).to_array())
            .collect_into_vec(verts);
    }
}

// ---------------------------------------------------------------------
// Corrective shapes: pose-driven morph targets that make plain linear
// blend skinning (what glTF and Bevy do) look like a better deformer.
//
// The reference deformer is skin relaxation: the LBS pose with the joint's
// blend zone (where the moving bone's subtree and the rest share
// vertices, two rings wider) relaxed toward the rest edge lengths by
// Jacobi position-based distance constraints, everything else held where
// LBS puts it. LBS's two failures at a joint are edges pulled long on the
// outside of the bend and collapsed on the inside (the pinch and the
// candy wrapper); relaxing toward rest lengths undoes exactly those, so the
// skin slides around the elbow and knee and keeps its area (and so most of
// its volume), while rigid regions come back unchanged. Delta mush (the
// first candidate: smooth the posed mesh, re-add the rest detail in the
// smoothed surface's frames) was tried and measured worse here: its
// frames flip on the folded smoothed surface at 120 degrees (10 cm knee
// offsets, stretch up from 1.5x to 6x), and its tangential drift adds
// stretch rather than removing it. Keys along a sweep are warm-started
// from the previous key, so their tents blend without sliding.
// ---------------------------------------------------------------------

/// `correctives` on a bind: `true` / `false`, or which joints and poses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum CorrectivesDef {
    On(bool),
    Spec(CorrectiveSpec),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CorrectiveSpec {
    /// Bones (`*` globs) to make correctives for (default: every bone the
    /// built-in table or the limits cover).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<String>,
    /// Custom key poses instead of the built-in ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poses: Vec<CorrectivePose>,
    /// At most this many targets (default 160; the most valuable first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
}

/// One key pose: `bone` turned `degrees` about its rest-frame `axis`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CorrectivePose {
    pub bone: String,
    /// `x`, `y` or `z` (the bone's rest frame).
    pub axis: String,
    pub degrees: f32,
}

/// A driven corrective shape (derived; see [`CorrectivesDef`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Corrective {
    /// Target name (`fix_elbow_l_x+120`).
    pub name: String,
    pub bone: String,
    pub bone_index: usize,
    pub parent: Option<usize>,
    /// Unit axis in the bone's rest frame.
    pub axis: [f32; 3],
    /// The bone's rest rotation in world space.
    pub rest: [f32; 4],
    /// The bone's rest rotation relative to its parent's rest frame (what
    /// the exported joint node's rotation is at rest).
    pub rest_local: [f32; 4],
    /// Signed angle (degrees) about `axis` at full weight.
    pub degrees: f32,
    /// Where the weight starts rising from 0 (the previous key, or 0).
    pub from: f32,
    /// Where it is back to 0 (the next key of the same joint and axis);
    /// `None` = it stays 1 past `degrees`. Keys of one joint split the
    /// sweep into tents, so their corrections interpolate linearly.
    pub to: Option<f32>,
    /// Sparse offsets in the part's own space: vertex indices and deltas.
    pub idx: Vec<u32>,
    pub delta: Vec<[f32; 3]>,
}

impl Corrective {
    /// The full-length offsets (zero off the joint), for export.
    pub fn dense(&self, n: usize) -> Vec<[f32; 3]> {
        let mut d = vec![[0.0; 3]; n];
        for (i, v) in self.idx.iter().zip(&self.delta) {
            if let Some(x) = d.get_mut(*i as usize) {
                *x = *v;
            }
        }
        d
    }

    /// The driver weight from a local joint rotation offset `r` (relative
    /// to rest, in the bone's rest frame).
    pub fn weight_of_local(&self, r: Quat) -> f32 {
        driver_weight(r, Vec3::from(self.axis), self.from, self.degrees, self.to)
    }

    /// The driver weight in a pose given as skin matrices.
    pub fn weight(&self, mats: &[Mat4]) -> f32 {
        let rot = |i: usize| {
            mats.get(i)
                .map_or(Quat::IDENTITY, |m| m.to_scale_rotation_translation().1)
        };
        let rest = quat_of(self.rest);
        let rel = self.parent.map_or(Quat::IDENTITY, rot).inverse() * rot(self.bone_index);
        self.weight_of_local(rest.inverse() * rel * rest)
    }
}

/// The driver curve on the twist angle of `r` about `axis` (swing-twist
/// split, so a turn about another axis adds nothing): 0 at `from`, rising
/// linearly to 1 at `peak`, then back to 0 at `to` (or held at 1).
pub fn driver_weight(r: Quat, axis: Vec3, from: f32, peak: f32, to: Option<f32>) -> f32 {
    modeler_anim::corrective_weight(r, axis, from, peak, to)
}

/// The part with its active correctives added to its rest vertices
/// (normals turned with the faces around them), or `None` when none is
/// active in this pose.
pub fn corrected(part: &Part, mats: &[Mat4]) -> Option<Part> {
    let cs = part.correctives.as_ref()?;
    let n = part.verts.len();
    let used: Vec<(&Corrective, f32)> = cs
        .iter()
        .map(|c| (c, c.weight(mats)))
        .filter(|(c, w)| *w > 1e-4 && c.idx.iter().all(|i| (*i as usize) < n))
        .collect();
    if used.is_empty() {
        return None;
    }
    let mut verts = part.verts.clone();
    let mut hit = vec![false; n];
    for (c, w) in &used {
        for (i, d) in c.idx.iter().zip(&c.delta) {
            let v = &mut verts[*i as usize];
            for k in 0..3 {
                v[k] += d[k] * w;
            }
            hit[*i as usize] = true;
        }
    }
    let mut normals = part.normals.clone();
    if normals.len() == n {
        let mut before = vec![Vec3::ZERO; n];
        let mut after = vec![Vec3::ZERO; n];
        for t in part.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| i as usize);
            if a >= n || b >= n || c >= n || !(hit[a] || hit[b] || hit[c]) {
                continue;
            }
            let f = |v: &[[f32; 3]]| {
                (Vec3::from(v[b]) - Vec3::from(v[a])).cross(Vec3::from(v[c]) - Vec3::from(v[a]))
            };
            let (f0, f1) = (f(&part.verts), f(&verts));
            for i in [a, b, c] {
                before[i] += f0;
                after[i] += f1;
            }
        }
        for i in 0..n {
            if !hit[i] {
                continue;
            }
            let (b0, a0) = (before[i].normalize_or_zero(), after[i].normalize_or_zero());
            if b0 != Vec3::ZERO && a0 != Vec3::ZERO {
                normals[i] = (Quat::from_rotation_arc(b0, a0) * Vec3::from(normals[i])).to_array();
            }
        }
    }
    Some(Part {
        verts,
        normals,
        correctives: None,
        ..part.clone()
    })
}

/// The welded vertex graph of a mesh: coincident vertices share a group.
struct Graph {
    group: Vec<u32>,
    /// One vertex of each group.
    rep: Vec<usize>,
    pos: Vec<Vec3>,
    nbr: Vec<Vec<u32>>,
    edge: f32,
}

fn graph(world: &[Vec3], indices: &[u32]) -> Graph {
    let n = world.len();
    let mut group = vec![0u32; n];
    let mut rep = Vec::new();
    let mut pos: Vec<Vec3> = Vec::new();
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
        group[i] = *map.entry(c).or_insert_with(|| {
            pos.push(*v);
            rep.push(i);
            (pos.len() - 1) as u32
        });
    }
    let mut nbr: Vec<Vec<u32>> = vec![Vec::new(); pos.len()];
    let (mut sum, mut cnt) = (0.0f64, 0usize);
    for t in indices.as_chunks::<3>().0 {
        if t.iter().any(|i| *i as usize >= n) {
            continue;
        }
        let g = t.map(|i| group[i as usize]);
        if g[0] == g[1] || g[1] == g[2] || g[2] == g[0] {
            continue;
        }
        for (a, b) in [(g[0], g[1]), (g[1], g[2]), (g[2], g[0])] {
            if !nbr[a as usize].contains(&b) {
                nbr[a as usize].push(b);
                nbr[b as usize].push(a);
                sum += f64::from((pos[a as usize] - pos[b as usize]).length());
                cnt += 1;
            }
        }
    }
    Graph {
        group,
        rep,
        pos,
        nbr,
        edge: if cnt > 0 {
            (sum / cnt as f64) as f32
        } else {
            1.0
        },
    }
}

impl Graph {
    /// Edge-length relaxation of `x` toward the rest lengths (Jacobi
    /// position-based distance constraints), moving only `free` groups.
    fn relax(
        &self,
        x: &[Vec3],
        anchor: &[Vec3],
        alpha: f32,
        free: &[bool],
        iters: usize,
    ) -> Vec<Vec3> {
        let mut x = x.to_vec();
        let edges: Vec<(u32, u32, f32)> = self
            .nbr
            .iter()
            .enumerate()
            .flat_map(|(a, nb)| {
                nb.iter()
                    .filter(move |b| (**b as usize) > a)
                    .map(move |b| (a as u32, *b, 0.0))
            })
            .filter(|(a, b, _)| free[*a as usize] || free[*b as usize])
            .map(|(a, b, _)| (a, b, (self.pos[a as usize] - self.pos[b as usize]).length()))
            .collect();
        let mut acc = vec![Vec3::ZERO; x.len()];
        let mut cnt = vec![0u32; x.len()];
        for _ in 0..iters {
            acc.iter_mut().for_each(|v| *v = Vec3::ZERO);
            cnt.iter_mut().for_each(|v| *v = 0);
            for &(a, b, l0) in &edges {
                let (a, b) = (a as usize, b as usize);
                let d = x[b] - x[a];
                let l = d.length();
                if l < 1e-9 {
                    continue;
                }
                let c = d * ((l - l0) / l);
                let (fa, fb) = (free[a], free[b]);
                let (wa, wb) = match (fa, fb) {
                    (true, true) => (0.5, 0.5),
                    (true, false) => (1.0, 0.0),
                    (false, true) => (0.0, 1.0),
                    _ => continue,
                };
                acc[a] += c * wa;
                acc[b] -= c * wb;
                cnt[a] += 1;
                cnt[b] += 1;
            }
            for i in 0..x.len() {
                if free[i] && cnt[i] > 0 {
                    x[i] += acc[i] / cnt[i] as f32;
                    let pull = (anchor[i] - x[i]) * alpha;
                    x[i] += pull;
                }
            }
        }
        x
    }
}

/// The built-in key poses for the `human` game rig (signs follow the
/// pose sheet: flexion is +x (the shoulder's raises the arm forward),
/// shoulders raise sideways about z), most valuable
/// first: a bind's `max` keeps the head of this list (see [`key_poses`]):
/// the elbows and knees (LBS collapses them worst), the hips (the crotch
/// and the buttock fold), the shoulders (the armpit), then the spine, the
/// clavicles (a shrug),
/// neck, the wrists, the jaw, the thumbs, the fingers last (small, and 24
/// per pair of hands).
fn human_poses() -> Vec<(String, char, f32)> {
    let mut v: Vec<(String, char, f32)> = Vec::new();
    let sides = [("l", 1.0f32), ("r", -1.0)];
    let both = |v: &mut Vec<(String, char, f32)>, b: &str, ax: char, d: f32, mirror: bool| {
        for (s, sg) in sides {
            v.push((format!("{b}_{s}"), ax, if mirror { d * sg } else { d }));
        }
    };
    // (The shoulder's keys stand alone: their anchored in-betweens left
    // the armpit at 2.31x (arms up) and 1.86x (forward) where one relaxed
    // key gives 2.11x and 1.74x; see `SINGLE_KEY`.)
    both(&mut v, "elbow", 'x', 120.0, false);
    both(&mut v, "knee", 'x', 120.0, false);
    both(&mut v, "thigh", 'x', -100.0, false);
    both(&mut v, "shoulder", 'z', 90.0, true);
    both(&mut v, "shoulder", 'x', 90.0, false);
    v.push(("spine".into(), 'x', 30.0));
    both(&mut v, "thigh", 'z', 45.0, true);
    both(&mut v, "clavicle", 'z', 30.0, true);
    v.push(("spine".into(), 'y', 45.0));
    v.push(("spine".into(), 'y', -45.0));
    v.push(("neck".into(), 'y', 60.0));
    v.push(("neck".into(), 'y', -60.0));
    both(&mut v, "wrist", 'x', 60.0, false);
    both(&mut v, "wrist", 'x', -60.0, false);
    v.push(("jaw".into(), 'x', 20.0));
    both(&mut v, "thumb_1", 'x', 45.0, false);
    both(&mut v, "thumb_2", 'x', 40.0, false);
    both(&mut v, "thumb_3", 'x', 60.0, false);
    for k in 1..=3 {
        for f in ["index", "middle", "ring", "pinky"] {
            both(&mut v, &format!("{f}_{k}"), 'x', 90.0, false);
        }
    }
    v
}

fn axis_of(c: char) -> Vec3 {
    match c {
        'y' => Vec3::Y,
        'z' => Vec3::Z,
        _ => Vec3::X,
    }
}

/// Whether correctives are on for this bind of `part`: by default only
/// for a mesh carrying the human template's weights.
pub fn correctives_on(part: &Part, def: &BindDef) -> bool {
    match &def.correctives {
        None => part.skin.is_some(),
        Some(CorrectivesDef::On(b)) => *b,
        Some(CorrectivesDef::Spec(_)) => true,
    }
}

/// The key poses a bind's correctives cover: (bone index, axis, degrees).
fn key_poses(skel: &Skeleton, def: &BindDef) -> Result<Vec<(usize, char, f32)>, String> {
    let spec = match &def.correctives {
        Some(CorrectivesDef::Spec(s)) => s.clone(),
        _ => CorrectiveSpec::default(),
    };
    let pick =
        |b: &str| spec.joints.is_empty() || spec.joints.iter().any(|g| crate::name_matches(g, b));
    let mut out = Vec::new();
    if !spec.poses.is_empty() {
        for p in &spec.poses {
            let ax = match p.axis.as_str() {
                "x" => 'x',
                "y" => 'y',
                "z" => 'z',
                o => return Err(format!("correctives: axis `{o}`: use x, y or z")),
            };
            let hits: Vec<usize> = (0..skel.bones.len())
                .filter(|i| crate::name_matches(&p.bone, &skel.bones[*i].name))
                .collect();
            if hits.is_empty() {
                return Err(format!("correctives: no bone matches `{}`", p.bone));
            }
            out.extend(hits.into_iter().map(|i| (i, ax, p.degrees)));
        }
    } else {
        for (b, ax, d) in human_poses() {
            if let Some(i) = skel.bone(&b)
                && pick(&b)
            {
                out.push((i, ax, d));
            }
        }
        if out.is_empty() {
            // Any other rig: each limited axis at its ends, else +-90 x.
            for (i, b) in skel.bones.iter().enumerate() {
                if b.parent.is_none() || !pick(&b.name) {
                    continue;
                }
                match &b.limits {
                    Some(l) => {
                        for (ax, r) in [('x', l.x), ('y', l.y), ('z', l.z)] {
                            let Some([lo, hi]) = r else { continue };
                            if lo <= -20.0 {
                                out.push((i, ax, lo.max(-120.0)));
                            }
                            if hi >= 20.0 {
                                out.push((i, ax, hi.min(120.0)));
                            }
                        }
                    }
                    None => {
                        out.push((i, 'x', 90.0));
                        out.push((i, 'x', -90.0));
                    }
                }
            }
        }
    }
    // In-betweens: a deep bend gets keys at a third / half of the way, so
    // the correction follows the sweep instead of scaling one shape. The
    // full-depth keys come first (in the table's priority order), then the
    // in-betweens, so a small `max` (WebGL's 24 morph targets) keeps one
    // shape per joint before it refines any of them; the tents adapt to
    // whichever keys survive.
    let mut full = Vec::new();
    let mut between = Vec::new();
    let human = spec.poses.is_empty() && out.len() > 8;
    for (i, ax, d) in out {
        let single = human
            && SINGLE_KEY
                .iter()
                .any(|(b, a)| *a == ax && skel.bones[i].name.starts_with(b));
        let k = if single {
            1
        } else if d.abs() >= 90.0 {
            3
        } else if d.abs() >= 40.0 {
            2
        } else {
            1
        };
        full.push((i, ax, d));
        for q in 1..k {
            between.push((i, ax, d * q as f32 / k as f32));
        }
    }
    // The two joints LBS collapses worst (the first four full keys: the
    // elbows and knees) get their in-betweens right after the ten most
    // valuable full keys: with `max: 24` the elbow at 120 degrees is 1.3x
    // with them and 2.8x without.
    let head = full.len().min(10);
    let early: Vec<(usize, char, f32)> = between
        .iter()
        .copied()
        .filter(|b| {
            full[..full.len().min(4)]
                .iter()
                .any(|f| f.0 == b.0 && f.1 == b.1)
        })
        .collect();
    let mut keyed: Vec<(usize, char, f32)> = full[..head].to_vec();
    keyed.extend(early.iter().copied());
    keyed.extend(full[head..].iter().copied());
    keyed.extend(between.into_iter().filter(|b| !early.contains(b)));
    keyed.truncate(spec.max.unwrap_or(160).min(256));
    Ok(keyed)
}

/// Built-in keys (bone name prefix, axis) that get no in-betweens.
const SINGLE_KEY: &[(&str, char)] = &[("shoulder_", 'z'), ("shoulder_", 'x')];

/// Relaxation sweeps per key pose.
const RELAX_ITERS: usize = 150;

/// Bones longer than this (meters) free vertices wholly on their own side.
const INTO_SUBTREE: f32 = 0.08;

/// The share of a vertex's weight on a joint's two sides for its key to
/// move it.
const OWNED: f32 = 0.9;

/// Per relaxation step, how far each free vertex of a multi-key sweep is
/// pulled back toward LBS: picks the least-moving relaxed shape, so the
/// in-between weights don't slide.
const ANCHOR: f32 = 0.03;

/// The edge ratio over which a corrective's in-between may not make an
/// edge worse than LBS, and the rounds of pulling it back.
const GUARD: f32 = 1.5;
const GUARD_STRETCH: f32 = 1.9;
const GUARD_ROUNDS: usize = 10;

static CORR_CACHE: Mutex<Option<HashMap<u64, Arc<Vec<Corrective>>>>> = Mutex::new(None);

/// Corrective shapes for a smooth `binding` of `part` (see the module
/// notes): one per key pose, sparse, cached by mesh, weights and rig.
pub fn correctives(
    part: &Part,
    skel: &Skeleton,
    binding: &Binding,
    def: &BindDef,
) -> Result<Option<Arc<Vec<Corrective>>>, String> {
    if binding.bone.is_some() || !correctives_on(part, def) || part.verts.len() < 3 {
        return Ok(None);
    }
    let n = part.verts.len();
    if binding.joints.len() != n {
        return Ok(None);
    }
    let poses = key_poses(skel, def)?;
    if poses.is_empty() {
        return Ok(None);
    }
    let k = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key(part, skel, def, "correctives").hash(&mut h);
        binding.joints.hash(&mut h);
        for w in binding.weights.iter().flatten() {
            w.to_bits().hash(&mut h);
        }
        h.finish()
    };
    if let Ok(g) = CORR_CACHE.lock()
        && let Some(c) = g.as_ref().and_then(|m| m.get(&k))
    {
        return Ok(Some(c.clone()));
    }
    let world: Vec<Vec3> = (0..n).map(|i| part.world_vert(i)).collect();
    let g = graph(&world, &part.indices);
    let rot = quat_of(part.rotation);
    let scale = Vec3::from(part.scale);
    let blended = |mats: &[Mat4], i: usize| -> Mat4 {
        let (j, w) = (binding.joints[i], binding.weights[i]);
        let mut m = Mat4::ZERO;
        for q in 0..4 {
            if w[q] > 0.0 {
                m += mats.get(j[q] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[q];
            }
        }
        m
    };
    let iters = RELAX_ITERS;
    // One key: the relaxed pose minus LBS, back in rest space. `prev` (the
    // previous key's rest-space offsets, world axes) warm-starts it, so
    // the keys of one sweep come from one continuous solution and blend
    // between them without sliding.
    let solve = |bi: usize,
                 ax: char,
                 deg: f32,
                 prev: &[Vec3],
                 alpha: f32|
     -> (Option<Corrective>, Vec<Vec3>) {
        let bone = &skel.bones[bi];
        let axis = axis_of(ax);
        let mut pose = skel.rest();
        pose[bi].1 = Quat::from_axis_angle(axis, deg.to_radians());
        let mats = skel.skin_matrices(&pose);
        let lbs: Vec<Vec3> = g
            .rep
            .iter()
            .map(|&v| blended(&mats, v).transform_point3(world[v]))
            .collect();
        let start: Vec<Vec3> = g
            .rep
            .iter()
            .map(|&v| blended(&mats, v).transform_point3(world[v] + prev[v]))
            .collect();
        let moving: Vec<bool> = (0..skel.bones.len())
            .map(|mut b| {
                loop {
                    if b == bi {
                        break true;
                    }
                    match skel.bones[b].parent {
                        Some(p) => b = p,
                        None => break false,
                    }
                }
            })
            .collect();
        // Free: where the bone's subtree and the rest share a vertex (the
        // blend zone), two rings wider; rigid regions keep LBS exactly.
        // Owned: nearly all the weight on the bone's subtree and its
        // ancestors, so a sibling's key (the next finger's webbing) never
        // moves it. Free: owned and split between the subtree and the
        // rest, grown two rings over owned vertices not wholly in the
        // subtree, so a chain's keys (finger joints 1 and 2) don't both
        // move one vertex and double up.
        let ancestor: Vec<bool> = (0..skel.bones.len())
            .map(|b| {
                let mut x = bone.parent;
                while let Some(p) = x {
                    if p == b {
                        return true;
                    }
                    x = skel.bones[p].parent;
                }
                false
            })
            .collect();
        let split: Vec<(f32, bool)> = g
            .rep
            .iter()
            .map(|&v| {
                let (j, w) = (binding.joints[v], binding.weights[v]);
                let (mut m, mut o) = (0.0f32, 0.0f32);
                for q in 0..4 {
                    let b = j[q] as usize;
                    if moving.get(b).copied().unwrap_or(false) {
                        m += w[q];
                    } else if ancestor.get(b).copied().unwrap_or(false) {
                        o += w[q];
                    }
                }
                (m, m + o > OWNED)
            })
            .collect();
        let mut free: Vec<bool> = split
            .iter()
            .map(|(m, owned)| *owned && *m > 0.01 && *m < 0.99)
            .collect();
        // Limb bones (longer than `INTO_SUBTREE`) also free the rings
        // wholly on their own side: the armpit and the crotch can then take
        // skin from the arm and the thigh instead of tearing between two
        // rigid borders. Digits keep the strict rule (chain keys of one
        // finger would double up on one vertex).
        let into_subtree = bone.head.distance(bone.tail) > INTO_SUBTREE;
        for _ in 0..2 {
            let prev = free.clone();
            for (q, f) in free.iter_mut().enumerate() {
                if !*f
                    && split[q].1
                    && (split[q].0 < 0.99 || into_subtree)
                    && g.nbr[q].iter().any(|u| prev[*u as usize])
                {
                    *f = true;
                }
            }
        }
        let reference = g.relax(&start, &lbs, alpha, &free, iters);
        let (mut idx, mut delta) = (Vec::new(), Vec::new());
        let mut dense = vec![Vec3::ZERO; n];
        for v in 0..n {
            let q = g.group[v] as usize;
            let c = reference[q] - lbs[q];
            if c.length() < 2e-4 {
                continue;
            }
            let m3 = bevy::math::Mat3::from_mat4(blended(&mats, v));
            if m3.determinant().abs() < 1e-6 {
                continue;
            }
            let wd = m3.inverse() * c;
            dense[v] = wd;
            idx.push(v as u32);
            delta.push(((rot.inverse() * wd) / scale).to_array());
        }
        if idx.is_empty() {
            return (None, dense);
        }
        let (_, rl) = bone.rest_local(&skel.bones);
        let c = Corrective {
            name: format!(
                "fix_{}_{ax}{}{}",
                bone.name,
                if deg < 0.0 { "-" } else { "+" },
                deg.abs().round()
            ),
            bone: bone.name.clone(),
            bone_index: bi,
            parent: bone.parent,
            axis: axis.to_array(),
            rest: bone.rest.to_array(),
            rest_local: rl.to_array(),
            degrees: deg,
            from: 0.0,
            to: None,
            idx,
            delta,
        };
        (Some(c), dense)
    };
    // Interpolation guard: a tent blends its key linearly with the one
    // before it, but a relaxed key can slide skin across a fold (the inside
    // of a knuckle unfolds), and half of that slide at half the angle
    // collapses edges LBS kept. At a quarter, half and three quarters of
    // the way from the previous key, every edge the blend pinches (weighted
    // as `verify` does) over `GUARD` or stretches over `GUARD_STRETCH`, and
    // worse than LBS alone, pulls its ends' offsets back toward the previous
    // key's, a few rounds, so in-betweens never tear what the keys fix.
    let edges: Vec<(usize, usize, f32)> = {
        let mut e: Vec<(usize, usize)> = part
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
            .filter(|(a, b)| (*a as usize) < n && (*b as usize) < n)
            .map(|(a, b)| (a.min(b) as usize, a.max(b) as usize))
            .collect();
        e.sort_unstable();
        e.dedup();
        e.into_iter()
            .map(|(a, b)| (a, b, world[a].distance(world[b])))
            .filter(|e| e.2 > 1e-6)
            .collect()
    };
    // (stretch, pinch weighted as `verify` does)
    let ratio = |l0: f32, l1: f32| (l1 / l0, l0 / l1.max(1e-9) * 0.4);
    let damp = |bi: usize, ax: char, d0: f32, d1: f32, prev: &[Vec3], mut d: Vec<Vec3>| {
        let touched: Vec<bool> = (0..n)
            .map(|i| d[i] != Vec3::ZERO || prev[i] != Vec3::ZERO)
            .collect();
        let local: Vec<&(usize, usize, f32)> = edges
            .iter()
            .filter(|e| touched[e.0] || touched[e.1])
            .collect();
        if local.is_empty() {
            return d;
        }
        let poses: Vec<(f32, Vec<Mat4>)> = [0.25f32, 0.5, 0.75]
            .iter()
            .map(|f| {
                let mut pose = skel.rest();
                let a = d0 + (d1 - d0) * f;
                pose[bi].1 = Quat::from_axis_angle(axis_of(ax), a.to_radians());
                (*f, skel.skin_matrices(&pose))
            })
            .collect();
        let lbs: Vec<Vec<(f32, f32)>> = poses
            .iter()
            .map(|(_, m)| {
                local
                    .iter()
                    .map(|&&(a, b, l0)| {
                        let pa = blended(m, a).transform_point3(world[a]);
                        let pb = blended(m, b).transform_point3(world[b]);
                        ratio(l0, pa.distance(pb))
                    })
                    .collect()
            })
            .collect();
        for _ in 0..GUARD_ROUNDS {
            let mut hit = vec![false; g.pos.len()];
            let mut any = false;
            for ((f, m), base) in poses.iter().zip(&lbs) {
                let at = |i: usize| {
                    let c = prev[i] + (d[i] - prev[i]) * *f;
                    blended(m, i).transform_point3(world[i] + c)
                };
                for (&&(a, b, l0), r0) in local.iter().zip(base) {
                    let r = ratio(l0, at(a).distance(at(b)));
                    if (r.0 > GUARD_STRETCH && r.0 > r0.0 + 0.05)
                        || (r.1 > GUARD && r.1 > r0.1 + 0.05)
                    {
                        hit[g.group[a] as usize] = true;
                        hit[g.group[b] as usize] = true;
                        any = true;
                    }
                }
            }
            if !any {
                break;
            }
            for i in 0..n {
                if hit[g.group[i] as usize] {
                    d[i] = prev[i] + (d[i] - prev[i]) * 0.6;
                }
            }
        }
        d
    };
    // The sparse offsets of a key from its (guarded) dense rest-space ones.
    let resparse = |mut c: Corrective, d: &[Vec3]| -> Option<Corrective> {
        c.idx.clear();
        c.delta.clear();
        for (v, wd) in d.iter().enumerate() {
            if wd.length() < 1e-4 {
                continue;
            }
            c.idx.push(v as u32);
            c.delta.push(((rot.inverse() * *wd) / scale).to_array());
        }
        (!c.idx.is_empty()).then_some(c)
    };
    // Chains: the keys of one joint, axis and side, nearest rest first.
    let mut chains: Vec<Vec<(usize, char, f32)>> = Vec::new();
    for p in &poses {
        match chains
            .iter_mut()
            .find(|c| c[0].0 == p.0 && c[0].1 == p.1 && c[0].2.signum() == p.2.signum())
        {
            Some(c) => c.push(*p),
            None => chains.push(vec![*p]),
        }
    }
    for c in &mut chains {
        c.sort_by(|a, b| a.2.abs().total_cmp(&b.2.abs()));
    }
    let out: Vec<Corrective> = chains
        .par_iter()
        .flat_map_iter(|chain| {
            let mut prev = vec![Vec3::ZERO; n];
            let mut got = Vec::new();
            // A sweep of several keys is pulled toward LBS so its keys
            // share one least-moving solution; a lone key relaxes freely.
            let alpha = if chain.len() > 1 { ANCHOR } else { 0.0 };
            let mut prev_deg = 0.0f32;
            for &(bi, ax, deg) in chain {
                let (c, d) = solve(bi, ax, deg, &prev, alpha);
                let d = damp(bi, ax, prev_deg, deg, &prev, d);
                got.extend(c.and_then(|c| resparse(c, &d)));
                prev = d;
                prev_deg = deg;
            }
            got
        })
        .collect();
    let mut out = out;
    // Tents: each key fades in from the previous key of the same joint,
    // axis and side, and out toward the next.
    let keys: Vec<(usize, [f32; 3], f32)> = out
        .iter()
        .map(|c| (c.bone_index, c.axis, c.degrees))
        .collect();
    for c in out.iter_mut() {
        let same = |k: &&(usize, [f32; 3], f32)| {
            k.0 == c.bone_index && k.1 == c.axis && k.2.signum() == c.degrees.signum()
        };
        let a = c.degrees.abs();
        c.from = keys
            .iter()
            .filter(same)
            .map(|k| k.2)
            .filter(|d| d.abs() < a - 1e-3)
            .max_by(|x, y| x.abs().total_cmp(&y.abs()))
            .unwrap_or(0.0);
        c.to = keys
            .iter()
            .filter(same)
            .map(|k| k.2)
            .filter(|d| d.abs() > a + 1e-3)
            .min_by(|x, y| x.abs().total_cmp(&y.abs()));
    }
    let arc = Arc::new(out);
    if let Ok(mut c) = CORR_CACHE.lock() {
        let m = c.get_or_insert_with(HashMap::new);
        if m.len() > 64 {
            m.clear();
        }
        m.insert(k, arc.clone());
    }
    Ok((!arc.is_empty()).then_some(arc))
}

// ---------------------------------------------------------------------
// Weight transfer (`weights_from`): clothing takes the body's weights.
// ---------------------------------------------------------------------

/// Weights of `part` from the closest point of `src`'s surface (both in
/// their world rest placement): barycentric over the closest triangle
/// among those around the nearest source vertices, then (with `blend`)
/// diffused over the part's own edges.
pub fn transfer_weights(
    part: &Part,
    src: &Part,
    skel: &Skeleton,
    def: &BindDef,
    name: &str,
) -> Result<Binding, String> {
    let from = def.weights_from.as_deref().unwrap_or("?");
    let Some(sb) = src.rig.as_ref() else {
        return Err(format!(
            "weights_from `{from}`: that part is not bound to any skeleton: add a bind of `{from}` to `{}` (a bind item, or a body's own rig; it may be in the same batch, binds order themselves by weights_from)",
            skel.name
        ));
    };
    if sb.skeleton != skel.name {
        return Err(format!(
            "weights_from `{from}`: it is bound to `{}`, not `{}`",
            sb.skeleton, skel.name
        ));
    }
    let m = src.verts.len();
    let sw: Vec<Sparse> = (0..m)
        .map(|i| match sb.bone {
            Some(b) => [(b as u16, 1.0), (0, 0.0), (0, 0.0), (0, 0.0)],
            None => {
                let (j, w) = (
                    sb.joints.get(i).copied().unwrap_or_default(),
                    sb.weights.get(i).copied().unwrap_or_default(),
                );
                [(j[0], w[0]), (j[1], w[1]), (j[2], w[2]), (j[3], w[3])]
            }
        })
        .collect();
    let near = closest_points(part, src)?;
    let n = part.verts.len();
    let mut w: Vec<Sparse> = near
        .iter()
        .map(|(tri, bc)| {
            let mut acc: Vec<(u16, f32)> = Vec::with_capacity(12);
            for (k, v) in tri.iter().enumerate() {
                for &(j, x) in &sw[*v as usize] {
                    if x > 0.0 {
                        acc.push((j, x * bc[k]));
                    }
                }
            }
            top4(&mut acc)
        })
        .collect();
    if let Some(blend) = def.blend.filter(|b| *b > 0.0) {
        let world: Vec<Vec3> = (0..n).map(|i| part.world_vert(i)).collect();
        let g = graph(&world, &part.indices);
        let mut gw: Vec<Sparse> = g.rep.iter().map(|&v| w[v]).collect();
        let iters = ((blend / g.edge.max(1e-6)).powi(2).round() as usize).min(200);
        for _ in 0..iters {
            gw = (0..gw.len())
                .into_par_iter()
                .map(|q| {
                    let nb = &g.nbr[q];
                    if nb.is_empty() {
                        return gw[q];
                    }
                    let mut acc: Vec<(u16, f32)> = Vec::new();
                    acc.extend(gw[q].iter().filter(|x| x.1 > 0.0).map(|x| (x.0, x.1 * 0.5)));
                    let k = nb.len() as f32;
                    for u in nb {
                        acc.extend(
                            gw[*u as usize]
                                .iter()
                                .filter(|x| x.1 > 0.0)
                                .map(|x| (x.0, x.1 * 0.5 / k)),
                        );
                    }
                    top4(&mut acc)
                })
                .collect();
        }
        w = g.group.iter().map(|q| gw[*q as usize]).collect();
    }
    Ok(Binding {
        skeleton: skel.name.clone(),
        bind: name.to_string(),
        bone: None,
        joints: w.iter().map(|s| [s[0].0, s[1].0, s[2].0, s[3].0]).collect(),
        weights: w.iter().map(|s| [s[0].1, s[1].1, s[2].1, s[3].1]).collect(),
    })
}

/// For every vertex of `part` (world rest placement): the closest point on
/// `src`'s surface as a source triangle and barycentric weights (among the
/// triangles around the nearest source vertices, found with a grid).
fn closest_points(part: &Part, src: &Part) -> Result<Vec<([u32; 3], [f32; 3])>, String> {
    let m = src.verts.len();
    if m == 0 || src.indices.is_empty() {
        return Err("weights_from: the source part is empty".into());
    }
    let sp: Vec<Vec3> = (0..m).map(|i| src.world_vert(i)).collect();
    let tris = src.indices.as_chunks::<3>().0;
    let mut around: Vec<Vec<u32>> = vec![Vec::new(); m];
    for (t, tri) in tris.iter().enumerate() {
        for &i in tri {
            if let Some(a) = around.get_mut(i as usize) {
                a.push(t as u32);
            }
        }
    }
    let (lo, hi) = sp
        .iter()
        .fold((Vec3::MAX, Vec3::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let cell = ((hi - lo).max_element() / 64.0).max(1e-4);
    let key = |p: Vec3| -> [i32; 3] {
        let q = ((p - lo) / cell).floor();
        [q.x as i32, q.y as i32, q.z as i32]
    };
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (i, p) in sp.iter().enumerate() {
        grid.entry(key(*p)).or_default().push(i as u32);
    }
    Ok((0..part.verts.len())
        .into_par_iter()
        .map(|i| {
            let p = part.world_vert(i);
            let c = key(p);
            // Grow the search shell until vertices turn up, then one more.
            let mut near: Vec<(f32, u32)> = Vec::new();
            let mut extra = 1;
            for r in 0i32..96 {
                for x in -r..=r {
                    for y in -r..=r {
                        for z in -r..=r {
                            if x.abs().max(y.abs()).max(z.abs()) != r {
                                continue;
                            }
                            if let Some(v) = grid.get(&[c[0] + x, c[1] + y, c[2] + z]) {
                                near.extend(
                                    v.iter()
                                        .map(|j| ((sp[*j as usize] - p).length_squared(), *j)),
                                );
                            }
                        }
                    }
                }
                if !near.is_empty() {
                    if extra == 0 {
                        break;
                    }
                    extra -= 1;
                }
            }
            near.sort_by(|a, b| a.0.total_cmp(&b.0));
            near.truncate(8);
            let mut best = (f32::MAX, [0u32; 3], [1.0f32, 0.0, 0.0]);
            for (_, j) in &near {
                for &t in &around[*j as usize] {
                    let tri = tris[t as usize];
                    let [a, b, cc] = tri.map(|k| sp[k as usize]);
                    let (q, bc) = closest_on_tri(p, a, b, cc);
                    let d = (q - p).length_squared();
                    if d < best.0 {
                        best = (d, tri, bc);
                    }
                }
            }
            match near.first() {
                _ if best.0 < f32::MAX => (best.1, best.2),
                Some((_, j)) => ([*j; 3], [1.0, 0.0, 0.0]),
                None => ([0; 3], [1.0, 0.0, 0.0]),
            }
        })
        .collect())
}

/// [`closest_points`] for tests elsewhere in the crate.
#[cfg(test)]
pub(crate) fn closest_for_test(part: &Part, src: &Part) -> Vec<([u32; 3], [f32; 3])> {
    closest_points(part, src).unwrap_or_default()
}

/// Correctives for a part whose weights came from `src` (`weights_from`):
/// one per source corrective (same driver), each vertex taking the
/// source's offset at its closest point (rest space, barycentric), so
/// clothing follows the corrected skin rather than plain LBS. Keeping the
/// offset's length as well (turning it with the skin triangle instead of
/// the blended matrix, which shrinks it across a bend) was tried: exact at
/// the keys but 5 mm off between them, so it is left out.
pub fn transfer_correctives(part: &Part, src: &Part) -> Option<Arc<Vec<Corrective>>> {
    let cs = src.correctives.as_ref()?;
    let near = closest_points(part, src).ok()?;
    let (rs, ss) = (quat_of(src.rotation), Vec3::from(src.scale));
    let (rt, st) = (quat_of(part.rotation), Vec3::from(part.scale));
    let out: Vec<Corrective> = cs
        .par_iter()
        .filter_map(|c| {
            let dense: HashMap<u32, Vec3> = c
                .idx
                .iter()
                .zip(&c.delta)
                .map(|(i, d)| (*i, rs * (Vec3::from(*d) * ss)))
                .collect();
            let (mut idx, mut delta) = (Vec::new(), Vec::new());
            for (v, (tri, bc)) in near.iter().enumerate() {
                let at = |k: usize| dense.get(&tri[k]).copied().unwrap_or(Vec3::ZERO);
                let d: Vec3 = (0..3).map(|k| at(k) * bc[k]).sum();
                if d.length() < 1e-4 {
                    continue;
                }
                idx.push(v as u32);
                delta.push(((rt.inverse() * d) / st).to_array());
            }
            (!idx.is_empty()).then(|| Corrective {
                idx,
                delta,
                ..c.clone()
            })
        })
        .collect();
    (!out.is_empty()).then(|| Arc::new(out))
}

/// The closest point of triangle abc to p and its barycentric weights
/// (Ericson, Real-Time Collision Detection 5.1.5).
fn closest_on_tri(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> (Vec3, [f32; 3]) {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return (a, [1.0, 0.0, 0.0]);
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return (b, [0.0, 1.0, 0.0]);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return (a + ab * v, [1.0 - v, v, 0.0]);
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return (c, [0.0, 0.0, 1.0]);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return (a + ac * w, [1.0 - w, 0.0, w]);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return (b + (c - b) * w, [0.0, 1.0 - w, w]);
    }
    let den = 1.0 / (va + vb + vc);
    let (v, w) = (vb * den, vc * den);
    (a + ab * v + ac * w, [1.0 - v - w, v, w])
}

/// Signed volume of a closed mesh given as world positions.
pub fn mesh_volume(verts: &[[f32; 3]], indices: &[u32]) -> f32 {
    let mut v = 0.0f64;
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| verts.get(i as usize).map_or(Vec3::ZERO, |x| Vec3::from(*x)));
        v += f64::from(a.dot(b.cross(c)));
    }
    (v / 6.0) as f32
}

/// `verify animation volume`: per smooth-bound part, the posed volume over
/// the rest volume at `t`, and the bone nearest the vertex that moved the
/// most (for the failure line). Parts that aren't closed are skipped.
pub fn volume_ratios(
    rest: &[Arc<Part>],
    posed: &[Arc<Part>],
    skel: &Skeleton,
) -> Vec<(String, f32, String)> {
    let mut out = Vec::new();
    for (r, p) in rest.iter().zip(posed) {
        let Some(b) = r
            .rig
            .as_ref()
            .filter(|b| b.bone.is_none() && b.skeleton == skel.name)
        else {
            continue;
        };
        if r.cards || r.sheet || r.indices.is_empty() {
            continue;
        }
        let rv: Vec<[f32; 3]> = (0..r.verts.len())
            .map(|i| r.world_vert(i).to_array())
            .collect();
        let v0 = mesh_volume(&rv, &r.indices);
        if v0.abs() < 1e-9 {
            continue;
        }
        let v1 = mesh_volume(&p.verts, &p.indices);
        let (mut far, mut at) = (0.0f32, 0usize);
        for i in 0..rv.len().min(p.verts.len()) {
            let d = (Vec3::from(p.verts[i]) - Vec3::from(rv[i])).length();
            if d > far {
                far = d;
                at = i;
            }
        }
        let bone = b
            .joints
            .get(at)
            .and_then(|j| skel.bones.get(j[0] as usize))
            .map_or(String::new(), |x| x.name.clone());
        out.push((r.name.clone(), v1 / v0, bone));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

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
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
                },
                BoneDef {
                    name: "b".into(),
                    parent: Some("a".into()),
                    head: PointRef::Point([1.0, 0.0, 0.0]),
                    tail: Some(PointRef::Point([2.0, 0.0, 0.0])),
                    radius: Some(0.1),
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
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
            weights_from: None,
            correctives: None,
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
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
                },
                BoneDef {
                    name: "r".into(),
                    parent: None,
                    head: PointRef::Point([0.0, 0.25, 0.0]),
                    tail: Some(PointRef::Point([2.0, 0.25, 0.0])),
                    radius: Some(0.1),
                    rest: None,
                    roll: None,
                    limits: None,
                    look_at: None,
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

    #[test]
    fn scaled_bones_carry_normals_by_the_inverse_transpose() {
        let sk = skel();
        let mut part = Part::new("tri");
        part.verts = vec![[0.0, 0.0, 0.0], [1.0, -1.0, 0.0], [0.0, 0.0, 1.0]];
        let n = Vec3::new(1.0, 1.0, 0.0).normalize().to_array();
        part.normals = vec![n; 3];
        part.indices = vec![0, 1, 2];
        let b = Binding {
            skeleton: sk.name.clone(),
            bind: "b".into(),
            bone: Some(0),
            joints: Vec::new(),
            weights: Vec::new(),
        };
        // A scale track's pose: bone `a` stretched 2x along x.
        let mut pose = sk.rest();
        pose[0].2 = Vec3::new(2.0, 1.0, 1.0);
        let mats = sk.skin_matrices(&pose);
        let posed = pose_part(&part, &b, &mats);
        let want = Vec3::new(0.5, 1.0, 0.0).normalize();
        for v in &posed.normals {
            assert!((Vec3::from(*v) - want).length() < 1e-5, "{v:?}");
        }
        // The normal stays perpendicular to the stretched surface.
        let e = Vec3::from(posed.verts[1]) - Vec3::from(posed.verts[0]);
        assert!(e.dot(want).abs() < 1e-5);
        // The buffer path gives the same as pose_part.
        let (mut vs, mut ns) = (Vec::new(), Vec::new());
        pose_into(&part, &b, &mats, &mut vs, &mut ns);
        assert_eq!(vs, posed.verts);
        assert_eq!(ns, posed.normals);
    }

    #[test]
    fn volume_ratio_of_a_bent_bar() {
        // A closed box-ish bar: the open bar plus end caps.
        let mut p = bar();
        let last = (p.verts.len() - 4) as u32;
        p.indices.extend([0, 2, 1, 0, 3, 2]);
        p.indices
            .extend([last, last + 1, last + 2, last, last + 2, last + 3]);
        let s = skel();
        let b = smooth_weights(&p, &s, &bind(), "t").unwrap_or_else(|e| panic!("{e}"));
        p.rig = Some(Arc::new(b.clone()));
        let rest = vec![Arc::new(p.clone())];
        let mut pose = s.rest();
        pose[1].1 = Quat::from_rotation_z(1.2);
        let posed = vec![Arc::new(pose_part(&p, &b, &s.skin_matrices(&pose)))];
        let r = volume_ratios(&rest, &rest, &s);
        assert!((r[0].1 - 1.0).abs() < 1e-4, "{r:?}");
        let r = volume_ratios(&rest, &posed, &s);
        assert!(r[0].1 < 1.0 && r[0].1 > 0.5, "{r:?}");
        assert_eq!(r[0].2, "b");
    }

    #[test]
    fn correctives_drive_by_the_bone_angle() {
        let (mut p, s) = (bar(), skel());
        let mut def = bind();
        def.correctives = Some(CorrectivesDef::On(true));
        let b = smooth_weights(&p, &s, &def, "t").unwrap_or_else(|e| panic!("{e}"));
        let c = correctives(&p, &s, &b, &def)
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|| panic!("no correctives"));
        // x +-90 on `b`, split into 30 / 60 / 90 keys.
        assert_eq!(
            c.len(),
            6,
            "{:?}",
            c.iter().map(|x| &x.name).collect::<Vec<_>>()
        );
        p.correctives = Some(c.clone());
        let mut pose = s.rest();
        pose[1].1 = Quat::from_rotation_x(15f32.to_radians());
        let mats = s.skin_matrices(&pose);
        let k45 = c
            .iter()
            .find(|x| x.degrees == 30.0)
            .unwrap_or_else(|| panic!("no 30"));
        assert!((k45.weight(&mats) - 0.5).abs() < 1e-3);
        assert!(corrected(&p, &s.skin_matrices(&s.rest())).is_none());
    }
}
