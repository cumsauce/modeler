//! Export optimisation for game engines (Bevy): passes over the visible
//! parts that run before `gltf::build` writes them.
//!
//! - **contact culling**: triangles covered by another solid part (a box's
//!   bottom on the floor, a screen inset in its frame) are dropped;
//! - **viewpoint culling** (`visible_from`): triangles no ray reaches from
//!   any point in the given boxes (the outside of a room the camera never
//!   leaves) are dropped;
//! - **weld**: duplicate vertices merged, unused ones dropped;
//! - **instancing**: parts that are translated copies of another (array
//!   copies) share its mesh, placed by their node;
//! - **merge** (`by_material`): parts with the same material fold into one
//!   mesh, one draw call each (except `merge_keep` patterns);
//! - material dedup, u16 indices, ORM packing and emissive strength happen
//!   in `gltf::build`, driven by the same [`Options`].
//!
//! Every pass is deterministic and is counted in the [`Report`].

use bevy::math::Vec3;
use rayon::prelude::*;
use serde_json::Value;
use std::collections::HashMap;

use crate::Part;

/// What the exporter does beyond writing the parts as they are.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub dedup_materials: bool,
    pub cull_contacts: bool,
    /// Boxes ([min, max]) the camera can be in; empty = no viewpoint culling.
    pub visible_from: Vec<(Vec3, Vec3)>,
    /// Viewpoints sampled in those boxes.
    pub view_samples: usize,
    pub instance: bool,
    pub merge: bool,
    /// Part-name patterns (`*` globs) kept as their own nodes when merging.
    pub merge_keep: Vec<String>,
    pub weld: bool,
    /// u16 indices where they fit.
    pub small_indices: bool,
    /// Occlusion packed into the metallic-roughness texture's R channel.
    pub orm: bool,
    /// `KHR_materials_emissive_strength` on emissive materials (bloom).
    pub emissive_strength: Option<f32>,
    /// Baked textures as shared PNG files (`Some(true)`) or packed into the
    /// buffer (`Some(false)`); `None` = the format's default (.gltf shares,
    /// .glb embeds).
    pub shared_textures: Option<bool>,
}

impl Options {
    /// Today's plain export.
    pub fn none() -> Self {
        Options {
            dedup_materials: false,
            cull_contacts: false,
            visible_from: Vec::new(),
            view_samples: 256,
            instance: false,
            merge: false,
            merge_keep: Vec::new(),
            weld: false,
            small_indices: false,
            orm: false,
            emissive_strength: None,
            shared_textures: None,
        }
    }

    /// Every lossless pass plus contact culling: what a game wants.
    pub fn game() -> Self {
        Options {
            dedup_materials: true,
            cull_contacts: true,
            instance: true,
            weld: true,
            small_indices: true,
            orm: true,
            ..Options::none()
        }
    }

    pub fn is_none(&self) -> bool {
        *self == Options::none()
    }

    /// `"game"`, `"none"`, or an object: `{preset?, dedup_materials?,
    /// cull_contacts?, visible_from?: [{min, max}], view_samples?, instance?,
    /// merge?: "by_material" | false, merge_keep?: [..], weld?,
    /// small_indices?, orm?, emissive_strength?}` (fields override the
    /// preset; the default preset is "game").
    pub fn parse(v: &Value) -> Result<Self, String> {
        let preset = |s: &str| match s {
            "game" => Ok(Options::game()),
            "none" => Ok(Options::none()),
            other => Err(format!("optimize: unknown preset `{other}` (game, none)")),
        };
        let m = match v {
            Value::Null => return Ok(Options::none()),
            Value::String(s) => return preset(s),
            Value::Object(m) => m,
            other => {
                return Err(format!(
                    "optimize: expected \"game\", \"none\" or an object, got {other}"
                ));
            }
        };
        let mut o = preset(m.get("preset").and_then(Value::as_str).unwrap_or("game"))?;
        for (k, x) in m {
            let flag = |x: &Value| {
                x.as_bool()
                    .ok_or_else(|| format!("optimize.{k}: expected true or false"))
            };
            match k.as_str() {
                "preset" => {}
                "textures" => {
                    o.shared_textures = match x.as_str() {
                        Some("shared") => Some(true),
                        Some("embed") => Some(false),
                        _ => return Err("optimize.textures: \"shared\" or \"embed\"".into()),
                    }
                }
                "dedup_materials" => o.dedup_materials = flag(x)?,
                "cull_contacts" => o.cull_contacts = flag(x)?,
                "instance" => o.instance = flag(x)?,
                "weld" => o.weld = flag(x)?,
                "small_indices" => o.small_indices = flag(x)?,
                "orm" => o.orm = flag(x)?,
                "merge" => {
                    o.merge = match x {
                        Value::Bool(b) => *b,
                        Value::String(s) if s == "by_material" => true,
                        other => {
                            return Err(format!(
                                "optimize.merge: \"by_material\" or false, got {other}"
                            ));
                        }
                    }
                }
                "merge_keep" => {
                    o.merge_keep = serde_json::from_value(x.clone())
                        .map_err(|e| format!("optimize.merge_keep: {e}"))?
                }
                "view_samples" => {
                    o.view_samples = x
                        .as_u64()
                        .filter(|n| (1..=4096).contains(n))
                        .ok_or("optimize.view_samples: 1..4096")?
                        as usize
                }
                "emissive_strength" => {
                    o.emissive_strength = match x {
                        Value::Null => None,
                        x => Some(
                            x.as_f64()
                                .filter(|s| *s > 0.0)
                                .ok_or("optimize.emissive_strength: a positive number")?
                                as f32,
                        ),
                    }
                }
                "visible_from" => {
                    let boxes: Vec<Value> = serde_json::from_value(x.clone())
                        .map_err(|e| format!("optimize.visible_from: {e}"))?;
                    o.visible_from = boxes
                        .iter()
                        .map(|b| {
                            let p = |key: &str| -> Result<Vec3, String> {
                                let a: [f32; 3] = serde_json::from_value(
                                    b.get(key).cloned().unwrap_or(Value::Null),
                                )
                                .map_err(|_| {
                                    format!(
                                        "optimize.visible_from: each box needs {key}: [x, y, z]"
                                    )
                                })?;
                                Ok(Vec3::from_array(a))
                            };
                            let (a, b) = (p("min")?, p("max")?);
                            Ok((a.min(b), a.max(b)))
                        })
                        .collect::<Result<_, String>>()?;
                }
                other => {
                    return Err(format!(
                        "optimize: unknown field `{other}` (preset, dedup_materials, cull_contacts, visible_from, view_samples, instance, merge, merge_keep, weld, small_indices, orm, emissive_strength)"
                    ));
                }
            }
        }
        Ok(o)
    }
}

/// How a part is written: its own mesh, or the mesh of an earlier part
/// moved by `offset` (a translated copy).
#[derive(Clone, Debug, PartialEq)]
pub enum MeshRef {
    Own,
    CopyOf { source: usize, offset: [f32; 3] },
}

/// The parts to write, with how each one's mesh is shared.
pub struct Prepared {
    pub parts: Vec<Part>,
    pub meshes: Vec<MeshRef>,
    pub report: Report,
}

/// Before / after counts and what each pass did.
#[derive(Default, Debug)]
pub struct Report {
    pub before_tris: usize,
    pub before_verts: usize,
    pub before_parts: usize,
    pub lines: Vec<String>,
}

fn tri_count(parts: &[Part]) -> usize {
    parts.iter().map(|p| p.indices.len() / 3).sum()
}

fn vert_count(parts: &[Part]) -> usize {
    parts.iter().map(|p| p.verts.len()).sum()
}

fn glob(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(p) else {
                return false;
            };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else if let Some(at) = rest.find(p) {
            rest = &rest[at + p.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Runs the geometry passes over the visible parts.
pub fn prepare(parts: Vec<Part>, o: &Options) -> Prepared {
    let mut report = Report {
        before_tris: tri_count(&parts),
        before_verts: vert_count(&parts),
        before_parts: parts.len(),
        lines: Vec::new(),
    };
    let mut parts = parts;
    if o.cull_contacts || !o.visible_from.is_empty() {
        let keep = cull(&parts, o);
        let mut dropped_contact = 0;
        let mut dropped_view = 0;
        for (p, k) in parts.iter_mut().zip(&keep) {
            let mut idx = Vec::with_capacity(p.indices.len());
            for (t, why) in p.indices.as_chunks::<3>().0.iter().zip(k) {
                match why {
                    Keep::Yes => idx.extend_from_slice(t),
                    Keep::Covered => dropped_contact += 1,
                    Keep::Unseen => dropped_view += 1,
                }
            }
            p.indices = idx;
        }
        if o.cull_contacts {
            report.lines.push(format!(
                "contact culling: {dropped_contact} triangle(s) covered by another part"
            ));
        }
        if !o.visible_from.is_empty() {
            report.lines.push(format!(
                "viewpoint culling: {dropped_view} triangle(s) unseen from {} box(es), {} viewpoints",
                o.visible_from.len(),
                o.view_samples
            ));
        }
        let empty: Vec<String> = parts
            .iter()
            .filter(|p| p.indices.is_empty())
            .map(|p| p.name.clone())
            .collect();
        if !empty.is_empty() {
            report.lines.push(format!(
                "{} part(s) entirely hidden, not exported: {}",
                empty.len(),
                empty.join(", ")
            ));
            parts.retain(|p| !p.indices.is_empty());
        }
    }
    // Culling leaves unused vertices behind: always compact; weld when asked.
    let before = vert_count(&parts);
    if o.weld || o.cull_contacts || !o.visible_from.is_empty() {
        for p in &mut parts {
            compact(p, o.weld);
        }
        report.lines.push(format!(
            "weld/compact: {} -> {} vertices",
            before,
            vert_count(&parts)
        ));
    }
    if o.merge {
        let n = parts.len();
        parts = merge(parts, &o.merge_keep);
        report.lines.push(format!(
            "merge by material: {n} parts -> {} meshes",
            parts.len()
        ));
    }
    let meshes = if o.instance && !o.merge {
        let m = find_instances(&parts);
        let copies = m.iter().filter(|r| **r != MeshRef::Own).count();
        report.lines.push(format!(
            "instancing: {copies} part(s) reuse another part's mesh"
        ));
        m
    } else {
        vec![MeshRef::Own; parts.len()]
    };
    Prepared {
        parts,
        meshes,
        report,
    }
}

// ---------------------------------------------------------------- culling

#[derive(Clone, Copy, PartialEq)]
enum Keep {
    Yes,
    Covered,
    Unseen,
}

/// A bounding volume hierarchy over triangles (median split, 4 per leaf).
struct Bvh {
    tris: Vec<[Vec3; 3]>,
    nodes: Vec<BvhNode>,
}

struct BvhNode {
    min: Vec3,
    max: Vec3,
    /// Leaf: tris[start..start + count]; inner: children at `start`, `start + 1`.
    start: u32,
    count: u32,
}

impl Bvh {
    fn new(mut items: Vec<([Vec3; 3], u32)>) -> Self {
        let mut nodes = Vec::new();
        if !items.is_empty() {
            nodes.push(BvhNode {
                min: Vec3::ZERO,
                max: Vec3::ZERO,
                start: 0,
                count: 0,
            });
            let len = items.len();
            Self::build(&mut items, 0, len, 0, &mut nodes);
        }
        let tris = items.into_iter().map(|(t, _)| t).collect();
        Bvh { tris, nodes }
    }

    fn build(
        items: &mut [([Vec3; 3], u32)],
        lo: usize,
        hi: usize,
        at: usize,
        nodes: &mut Vec<BvhNode>,
    ) {
        let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        let (mut cmn, mut cmx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for (t, _) in &items[lo..hi] {
            for v in t {
                mn = mn.min(*v);
                mx = mx.max(*v);
            }
            let c = (t[0] + t[1] + t[2]) / 3.0;
            cmn = cmn.min(c);
            cmx = cmx.max(c);
        }
        nodes[at].min = mn;
        nodes[at].max = mx;
        if hi - lo <= 4 {
            nodes[at].start = lo as u32;
            nodes[at].count = (hi - lo) as u32;
            return;
        }
        let ext = cmx - cmn;
        let axis = if ext.x >= ext.y && ext.x >= ext.z {
            0
        } else if ext.y >= ext.z {
            1
        } else {
            2
        };
        let mid = (lo + hi) / 2;
        let key = |t: &[Vec3; 3]| t[0][axis] + t[1][axis] + t[2][axis];
        items[lo..hi].select_nth_unstable_by(mid - lo, |a, b| key(&a.0).total_cmp(&key(&b.0)));
        let left = nodes.len();
        for _ in 0..2 {
            nodes.push(BvhNode {
                min: Vec3::ZERO,
                max: Vec3::ZERO,
                start: 0,
                count: 0,
            });
        }
        nodes[at].start = left as u32;
        nodes[at].count = 0;
        Self::build(items, lo, mid, left, nodes);
        Self::build(items, mid, hi, left + 1, nodes);
    }

    fn slab(n: &BvhNode, o: Vec3, inv: Vec3, tmax: f32) -> bool {
        let t1 = (n.min - o) * inv;
        let t2 = (n.max - o) * inv;
        let tn = t1.min(t2).max_element();
        let tf = t1.max(t2).min_element();
        tf >= tn.max(0.0) && tn <= tmax
    }

    /// Calls `f(triangle index, distance)` for every hit with 0 < t < tmax;
    /// stops when `f` returns true (and returns true).
    fn walk(&self, o: Vec3, d: Vec3, tmax: f32, f: &mut dyn FnMut(usize, f32) -> bool) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let inv = Vec3::ONE / d;
        let mut stack = vec![0u32];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i as usize];
            if !Self::slab(n, o, inv, tmax) {
                continue;
            }
            if n.count > 0 {
                for k in n.start as usize..(n.start + n.count) as usize {
                    if let Some(t) = ray_tri(o, d, &self.tris[k])
                        && t < tmax
                        && f(k, t)
                    {
                        return true;
                    }
                }
            } else {
                stack.push(n.start);
                stack.push(n.start + 1);
            }
        }
        false
    }
}

/// Two-sided Möller–Trumbore; the distance of a hit in front of `o`.
fn ray_tri(o: Vec3, d: Vec3, t: &[Vec3; 3]) -> Option<f32> {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist > 1e-7).then_some(dist)
}

fn world_tris(p: &Part) -> Vec<[Vec3; 3]> {
    let w: Vec<Vec3> = (0..p.verts.len()).map(|i| p.world_vert(i)).collect();
    p.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [w[t[0] as usize], w[t[1] as usize], w[t[2] as usize]])
        .collect()
}

/// A closed, opaque part hides what it covers; glass and sheets don't.
fn occludes(p: &Part) -> bool {
    p.material.base_color[3] >= 0.999 && crate::verify::open_edge_count(p) == 0
}

/// Sample points of a triangle, pulled 10% toward its centroid (so they are
/// never exactly on a shared edge) and pushed `eps` out along its normal.
fn samples(t: &[Vec3; 3], eps: f32) -> Option<(Vec3, [Vec3; 4])> {
    let n = (t[1] - t[0]).cross(t[2] - t[0]);
    if n.length_squared() < 1e-20 {
        return None;
    }
    let n = n.normalize();
    let c = (t[0] + t[1] + t[2]) / 3.0;
    let s = |v: Vec3| v * 0.9 + c * 0.1 + n * eps;
    Some((n, [c + n * eps, s(t[0]), s(t[1]), s(t[2])]))
}

/// An odd direction, so parity rays rarely graze an edge.
const PARITY_DIR: Vec3 = Vec3::new(0.538_516_5, 0.741_909_7, 0.399_683_7);

fn cull(parts: &[Part], o: &Options) -> Vec<Vec<Keep>> {
    let tris: Vec<Vec<[Vec3; 3]>> = parts.par_iter().map(world_tris).collect();
    let bounds: Vec<(Vec3, Vec3)> = tris
        .iter()
        .map(|ts| {
            ts.iter().flatten().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(a, b), v| (a.min(*v), b.max(*v)),
            )
        })
        .collect();
    let occluder: Vec<bool> = parts.iter().map(occludes).collect();
    // One BVH per occluding part (point-in-solid), one over every occluder
    // (visibility rays).
    let solids: Vec<Option<Bvh>> = tris
        .par_iter()
        .zip(&occluder)
        .map(|(ts, occ)| occ.then(|| Bvh::new(ts.iter().map(|t| (*t, 0)).collect())))
        .collect();
    let all = Bvh::new(
        tris.iter()
            .enumerate()
            .filter(|(i, _)| occluder[*i])
            .flat_map(|(i, ts)| ts.iter().map(move |t| (*t, i as u32)))
            .collect(),
    );
    let views = viewpoints(&o.visible_from, o.view_samples);
    let inside = |b: usize, p: Vec3| -> bool {
        let (lo, hi) = bounds[b];
        if p.cmplt(lo).any() || p.cmpgt(hi).any() {
            return false;
        }
        let Some(bvh) = &solids[b] else {
            return false;
        };
        let mut n = 0;
        bvh.walk(p, PARITY_DIR, f32::MAX, &mut |_, _| {
            n += 1;
            false
        });
        n % 2 == 1
    };
    (0..parts.len())
        .into_par_iter()
        .map(|a| {
            let (lo, hi) = bounds[a];
            let eps = ((hi - lo).length() * 0.002).clamp(1e-4, 5e-3);
            tris[a]
                .iter()
                .map(|t| {
                    let Some((n, pts)) = samples(t, eps) else {
                        return Keep::Covered;
                    };
                    if o.cull_contacts
                        && pts.iter().all(|p| {
                            (0..parts.len()).any(|b| b != a && occluder[b] && inside(b, *p))
                        })
                    {
                        return Keep::Covered;
                    }
                    if !views.is_empty() {
                        let seen = pts.iter().any(|p| {
                            views.iter().any(|v| {
                                let d = *v - *p;
                                if d.dot(n) <= 0.0 {
                                    return false;
                                }
                                let len = d.length();
                                !all.walk(*p, d / len, len, &mut |_, _| true)
                            })
                        });
                        if !seen {
                            return Keep::Unseen;
                        }
                    }
                    Keep::Yes
                })
                .collect()
        })
        .collect()
}

/// Deterministic, evenly spread points in the boxes (a jittered grid by box
/// volume).
fn viewpoints(boxes: &[(Vec3, Vec3)], n: usize) -> Vec<Vec3> {
    if boxes.is_empty() {
        return Vec::new();
    }
    let vol: Vec<f32> = boxes
        .iter()
        .map(|(a, b)| (*b - *a).max(Vec3::splat(1e-3)).element_product())
        .collect();
    let total: f32 = vol.iter().sum();
    let mut out = Vec::new();
    let mut h: u32 = 0x9e37_79b9;
    let mut rnd = || {
        h ^= h << 13;
        h ^= h >> 17;
        h ^= h << 5;
        (h as f32) / (u32::MAX as f32)
    };
    for ((a, b), v) in boxes.iter().zip(&vol) {
        let k = ((n as f32 * v / total).round() as usize).max(1);
        let ext = (*b - *a).max(Vec3::splat(1e-3));
        // Cells proportional to the box's shape.
        let s = (k as f32 / ext.element_product()).cbrt();
        let cells = (ext * s).ceil().max(Vec3::ONE);
        let (cx, cy, cz) = (cells.x as usize, cells.y as usize, cells.z as usize);
        let step = ext / cells;
        let mut all = Vec::new();
        for i in 0..cx {
            for j in 0..cy {
                for l in 0..cz {
                    let jit = Vec3::new(rnd(), rnd(), rnd());
                    all.push(*a + (Vec3::new(i as f32, j as f32, l as f32) + jit) * step);
                }
            }
        }
        // Thin out evenly to k.
        let stride = all.len() as f32 / k as f32;
        for j in 0..k.min(all.len()) {
            out.push(all[((j as f32 * stride) as usize).min(all.len() - 1)]);
        }
    }
    out
}

// ---------------------------------------------------------- weld / compact

/// Drops unused vertices and (with `weld`) merges identical ones.
fn compact(p: &mut Part, weld: bool) {
    let mut map: HashMap<[u32; 8], u32> = HashMap::new();
    let mut remap = vec![u32::MAX; p.verts.len()];
    let (mut v, mut nrm, mut uv) = (Vec::new(), Vec::new(), Vec::new());
    for i in p.indices.iter_mut() {
        let old = *i as usize;
        if remap[old] == u32::MAX || weld {
            let key = {
                let a = p.verts[old];
                let n = p.normals.get(old).copied().unwrap_or_default();
                let t = p.uvs.get(old).copied().unwrap_or_default();
                [
                    a[0].to_bits(),
                    a[1].to_bits(),
                    a[2].to_bits(),
                    n[0].to_bits(),
                    n[1].to_bits(),
                    n[2].to_bits(),
                    t[0].to_bits(),
                    t[1].to_bits(),
                ]
            };
            let new = if weld {
                *map.entry(key).or_insert_with(|| {
                    v.push(p.verts[old]);
                    nrm.push(p.normals.get(old).copied().unwrap_or_default());
                    uv.push(p.uvs.get(old).copied().unwrap_or_default());
                    (v.len() - 1) as u32
                })
            } else {
                if remap[old] == u32::MAX {
                    v.push(p.verts[old]);
                    nrm.push(p.normals.get(old).copied().unwrap_or_default());
                    uv.push(p.uvs.get(old).copied().unwrap_or_default());
                    remap[old] = (v.len() - 1) as u32;
                }
                remap[old]
            };
            remap[old] = new;
        }
        *i = remap[old];
    }
    p.verts = v;
    p.normals = nrm;
    p.uvs = uv;
}

// ------------------------------------------------------------- instancing

/// A part whose vertices are another's moved by one offset (and the same
/// indices, normals, material, and uvs when textured) reuses its mesh.
fn find_instances(parts: &[Part]) -> Vec<MeshRef> {
    let ident = |p: &Part| p.rotation == [0.0, 0.0, 0.0, 1.0] && p.scale == [1.0, 1.0, 1.0];
    let shape_key = |p: &Part| {
        // Offset-invariant: counts plus the first edges.
        let e = |i: usize| -> [i32; 3] {
            let (a, b) = (p.verts[0], p.verts[i.min(p.verts.len() - 1)]);
            [0, 1, 2].map(|k| ((b[k] - a[k]) * 1e4).round() as i32)
        };
        (p.verts.len(), p.indices.len(), e(1), e(p.verts.len() / 2))
    };
    let mat_key = |p: &Part| serde_json::to_string(&p.material).unwrap_or_default();
    let mut groups: HashMap<_, Vec<usize>> = HashMap::new();
    let mut out = vec![MeshRef::Own; parts.len()];
    for (i, p) in parts.iter().enumerate() {
        if p.verts.is_empty() || !ident(p) {
            continue;
        }
        let key = (shape_key(p), mat_key(p));
        let cands = groups.entry(key).or_default();
        let found = cands.iter().copied().find_map(|s| {
            let q = &parts[s];
            let off = Vec3::from_array(p.verts[0]) - Vec3::from_array(q.verts[0]);
            let same = p.indices == q.indices
                && p.normals
                    .iter()
                    .zip(&q.normals)
                    .all(|(a, b)| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5))
                && p.verts.iter().zip(&q.verts).all(|(a, b)| {
                    (Vec3::from_array(*a) - Vec3::from_array(*b) - off)
                        .abs()
                        .max_element()
                        < 1e-4
                })
                && (p.material.pbr.is_none()
                    || p.uvs
                        .iter()
                        .zip(&q.uvs)
                        .all(|(a, b)| (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5));
            same.then(|| {
                let t = Vec3::from_array(p.translation) - Vec3::from_array(q.translation) + off;
                (s, t.to_array())
            })
        });
        match found {
            Some((source, offset)) => out[i] = MeshRef::CopyOf { source, offset },
            None => cands.push(i),
        }
    }
    out
}

// ------------------------------------------------------------------ merge

/// Parts sharing a material fold into one mesh in world space.
fn merge(parts: Vec<Part>, keep: &[String]) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::new();
    let mut by_mat: Vec<(String, Part)> = Vec::new();
    for p in parts {
        if keep.iter().any(|k| glob(k, &p.name)) {
            out.push(p);
            continue;
        }
        let key = serde_json::to_string(&p.material).unwrap_or_default();
        let q = crate::quat_of(p.rotation);
        let (t, s) = (Vec3::from_array(p.translation), Vec3::from_array(p.scale));
        let world = |v: [f32; 3]| (q * (Vec3::from_array(v) * s) + t).to_array();
        let wn = |n: [f32; 3]| {
            (q * (Vec3::from_array(n) / s))
                .normalize_or_zero()
                .to_array()
        };
        let slot = match by_mat.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                let mut m = Part::new(match &p.material.pbr {
                    Some(r) => format!("merged_{}", r.material),
                    None => format!("merged_{}", by_mat.len()),
                });
                m.material = p.material.clone();
                m.visible = true;
                by_mat.push((key, m));
                by_mat.len() - 1
            }
        };
        let m = &mut by_mat[slot].1;
        let base = m.verts.len() as u32;
        m.verts.extend(p.verts.iter().map(|v| world(*v)));
        m.normals.extend(p.normals.iter().map(|n| wn(*n)));
        m.uvs.extend(p.uvs.iter().copied());
        m.indices.extend(p.indices.iter().map(|i| i + base));
    }
    out.extend(by_mat.into_iter().map(|(_, m)| m));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Session;
    use serde_json::json;

    fn session(items: Value) -> Session {
        let mut s = Session::default();
        s.apply_values_as(None, items.as_array().cloned().unwrap_or_default(), false)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    fn visible(s: &Session) -> Vec<Part> {
        s.model
            .parts
            .iter()
            .filter(|p| p.visible)
            .map(|p| (**p).clone())
            .collect()
    }

    #[test]
    fn a_box_on_a_floor_loses_its_bottom_and_the_floor_keeps_its_top() {
        let s = session(json!([
            {"type": "primitive", "name": "floor", "kind": "box", "size": [2, 0.1, 2], "translation": [0, 0.05, 0]},
            {"type": "primitive", "name": "crate", "kind": "box", "size": 0.4, "translation": [0, 0.3, 0]},
            {"type": "primitive", "name": "alone", "kind": "box", "size": 0.2, "translation": [3, 0.1, 0]}
        ]));
        let o = Options {
            cull_contacts: true,
            ..Options::none()
        };
        let before: HashMap<String, usize> = visible(&s)
            .iter()
            .map(|p| (p.name.clone(), p.indices.len() / 3))
            .collect();
        let out = prepare(visible(&s), &o);
        let after: HashMap<String, usize> = out
            .parts
            .iter()
            .map(|p| (p.name.clone(), p.indices.len() / 3))
            .collect();
        assert_eq!(after["crate"], before["crate"] - 2, "{after:?}");
        assert_eq!(after["alone"], before["alone"]);
        assert_eq!(
            after["floor"], before["floor"],
            "the floor's top is only partly covered"
        );
    }

    #[test]
    fn inside_a_closed_room_the_outside_is_culled() {
        let s = session(json!([
            {"type": "primitive", "name": "room_raw", "kind": "box", "size": [4, 3, 4], "translation": [0, 1.5, 0]},
            {"type": "shell", "name": "room", "input": "room_raw", "thickness": 0.2},
            {"type": "hide_part", "part": "room_raw"}
        ]));
        let o = Options {
            visible_from: vec![(Vec3::new(-1.5, 0.5, -1.5), Vec3::new(1.5, 2.5, 1.5))],
            view_samples: 64,
            ..Options::none()
        };
        let outward = |p: &Part| {
            world_tris(p)
                .iter()
                .filter(|t| {
                    let n = (t[1] - t[0]).cross(t[2] - t[0]);
                    let c = (t[0] + t[1] + t[2]) / 3.0;
                    n.dot(c - Vec3::new(0.0, 1.5, 0.0)) > 0.0
                })
                .count()
        };
        let room = visible(&s);
        assert!(outward(&room[0]) >= 12);
        let out = prepare(room, &o);
        assert_eq!(outward(&out.parts[0]), 0);
        assert!(tri_count(&out.parts) >= 12);
    }

    #[test]
    fn array_copies_share_one_mesh() {
        let s = session(json!([
            {"type": "primitive", "name": "post", "kind": "cylinder", "size": [0.05, 1], "translation": [0, 0.5, 0]},
            {"type": "linear_array", "name": "post", "input": "post", "count": 4, "offset": [0.5, 0, 0]}
        ]));
        let out = prepare(visible(&s), &Options::game());
        let copies: Vec<&MeshRef> = out.meshes.iter().filter(|m| **m != MeshRef::Own).collect();
        assert_eq!(copies.len(), 3, "{:?}", out.meshes);
        assert!(out.meshes.iter().any(|m| matches!(m,
            MeshRef::CopyOf { offset, .. } if (offset[0] - 1.5).abs() < 1e-4)));
    }

    #[test]
    fn weld_merges_duplicates_and_merge_folds_by_material() {
        let mut p = Part::new("t");
        p.verts = vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0; 3]];
        p.normals = vec![[0.0, 0.0, 1.0]; 4];
        p.uvs = vec![[0.0; 2]; 4];
        p.uvs[1] = [1.0, 0.0];
        p.uvs[2] = [0.0, 1.0];
        p.indices = vec![0, 1, 2, 3, 1, 2];
        compact(&mut p, true);
        assert_eq!(p.verts.len(), 3);
        assert_eq!(p.indices, vec![0, 1, 2, 0, 1, 2]);
        let mut q = p.clone();
        q.name = "u".into();
        q.translation = [2.0, 0.0, 0.0];
        let m = merge(vec![p, q], &[]);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].indices.len(), 12);
        assert!((m[0].verts[4][0] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn options_parse_presets_and_overrides() {
        assert_eq!(Options::parse(&json!("game")), Ok(Options::game()));
        let o = Options::parse(&json!({"merge": "by_material", "merge_keep": ["door*"],
            "visible_from": [{"min": [0, 0, 0], "max": [1, 2, 3]}]}))
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(o.merge && o.cull_contacts && o.visible_from.len() == 1);
        assert!(Options::parse(&json!({"merj": true})).is_err());
        assert!(glob("door*", "door_l") && !glob("door*", "a_door"));
    }
}
