//! Scattering: copies of a part (or of rigid sets of parts, picked at random)
//! strewn over another part's surface. Points are sampled area-weighted on
//! the `on` part's triangles (upward-facing ones by default), rejected by
//! slope, region and spacing (dart throwing on a hash grid, so the layout is
//! Poisson-disk-like), and each copy is set down with its base on the point.
//! Deterministic per seed. No game code.

use crate::{Part, quat_of};
use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Hard cap on copies per scatter (each copy is a part).
pub const MAX_COPIES: u32 = 10_000;
/// Copies tried when only `density` is given and `max` is not.
pub const DEFAULT_MAX: u32 = 2_000;

/// How a scatter strews its copies (lengths in meters, angles in degrees).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScatterDef {
    /// Exact number of copies wanted (fewer when spacing/slope leave no room).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Copies per square meter of eligible surface (instead of `count`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
    /// Upper bound with `density` (also the number of output names).
    pub max: u32,
    pub seed: u64,
    /// Uniform scale range [min, max].
    pub scale: [f32; 2],
    /// "random_y" (spin about the up axis), "random" (any orientation:
    /// tumbled rocks) or "none".
    pub rotate: String,
    /// 0 = stay upright, 1 = stand along the surface normal.
    pub align: f32,
    /// Minimum distance between copies' base points; None = derived from the
    /// copies' footprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_spacing: Option<f32>,
    /// Steepest surface accepted (degrees between its normal and up); None
    /// = any face `faces` allows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_slope: Option<f32>,
    /// Extra depth each copy is pushed into the surface.
    pub sink: f32,
    /// Only points with x, z inside [min, max].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<[[f32; 2]; 2]>,
    /// "up" (faces whose normal points up) or "all".
    pub faces: String,
    /// Relative odds of each set (default equal).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<f32>,
    /// Only points within `near_distance` of these parts' surfaces (clutter
    /// along wall bases, moss round a rock).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub near: Vec<String>,
    /// Meters from the `near` parts' surfaces (default 0.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_distance: Option<f32>,
    /// 0..1: prefer points near the `on` surface's boundary, creases and
    /// corners (0 = uniform, 1 = hug the features).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edge_bias: f32,
    /// Copies gathered round `count` random centres within `radius`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clump: Option<Clump>,
    /// Terrain mask (`on` must be a terrain): "path", "pad", "not_path",
    /// "not_pad".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<String>,
    /// Only points with world y in [min, max].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<[f32; 2]>,
    /// The terrain `on` names, filled in before evaluation when `mask` is set.
    #[serde(skip)]
    pub terrain: Option<Box<crate::terrain::Terrain>>,
}

/// Cluster centres for a clumped scatter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clump {
    /// Number of cluster centres.
    pub count: u32,
    /// Meters from a centre a copy may land.
    pub radius: f32,
}

fn is_zero(x: &f32) -> bool {
    *x == 0.0
}

/// A scattered decal: the image's size range and look (see `Op::DecalScatter`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DecalScatterDef {
    pub pbr: crate::material::PbrRef,
    /// Image width range [min, max], meters.
    pub size: [f32; 2],
    /// Height / width of the image.
    pub aspect: f32,
    /// Box depth along the projection (default half the smaller side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    /// Random roll about the normal (default true).
    pub roll: bool,
    pub kind: String,
}

impl Default for ScatterDef {
    fn default() -> Self {
        Self {
            count: None,
            density: None,
            max: DEFAULT_MAX,
            seed: 1,
            scale: [1.0, 1.0],
            rotate: "random_y".into(),
            align: 0.0,
            min_spacing: None,
            max_slope: None,
            sink: 0.0,
            region: None,
            faces: "up".into(),
            weights: Vec::new(),
            near: Vec::new(),
            near_distance: None,
            edge_bias: 0.0,
            clump: None,
            mask: None,
            height: None,
            terrain: None,
        }
    }
}

impl ScatterDef {
    /// Copies tried (and output names reserved).
    pub fn slots(&self) -> u32 {
        self.count.unwrap_or(self.max).min(MAX_COPIES)
    }
}

/// The suffix each member of a set gets after `name_i`: the first member is
/// `name_i`; `oak_leaves` beside `oak` is `name_i_leaves`; members of one
/// prefab instance (`tree1.trunk`, `tree1.crown`) become `name_i.trunk`,
/// `name_i.crown`; anything else `name_i.member`.
pub fn member_suffixes(set: &[String]) -> Vec<String> {
    if set.len() == 1 {
        return vec![String::new()];
    }
    let prefix = set[0].split_once('.').map(|(p, _)| format!("{p}."));
    if let Some(p) = &prefix
        && set
            .iter()
            .all(|m| m.starts_with(p.as_str()) && m.len() > p.len())
    {
        return set.iter().map(|m| format!(".{}", &m[p.len()..])).collect();
    }
    let first = &set[0];
    set.iter()
        .enumerate()
        .map(|(i, m)| {
            if i == 0 {
                String::new()
            } else if let Some(rest) = m.strip_prefix(first.as_str())
                && (rest.starts_with('_') || rest.starts_with('.'))
            {
                rest.to_string()
            } else {
                format!(".{m}")
            }
        })
        .collect()
}

/// Every name a scatter may write: `name_i` plus each set member's suffix,
/// for every slot.
pub fn output_names(name: &str, sets: &[Vec<String>], def: &ScatterDef) -> Vec<String> {
    let mut suffixes: Vec<String> = Vec::new();
    for s in sets {
        for x in member_suffixes(s) {
            if !suffixes.contains(&x) {
                suffixes.push(x);
            }
        }
    }
    if suffixes.is_empty() {
        suffixes.push(String::new());
    }
    let n = def.slots();
    let mut out = Vec::with_capacity(n as usize * suffixes.len());
    for i in 0..n {
        for s in &suffixes {
            out.push(format!("{name}_{i}{s}"));
        }
    }
    out
}

/// splitmix64: small, fast, deterministic per seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Where a set stands: the centroid of its lowest slice (the trunk of a
/// tree, not the middle of its crown) at its lowest height, and the slice's
/// vertices (sunk below the surface's tangent plane when placed).
struct Base {
    point: Vec3,
    slice: Vec<usize>,
    height: f32,
    /// Horizontal extent (max of x and z widths), for the default spacing.
    width: f32,
}

/// The base of `verts` turned by `turn` about `center`, in the turned frame
/// (returned point is mapped back to world coordinates).
fn base_of(verts: &[Vec3], turn: Quat, center: Vec3) -> Base {
    let w: Vec<Vec3> = verts.iter().map(|v| turn * (*v - center)).collect();
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for v in &w {
        min = min.min(*v);
        max = max.max(*v);
    }
    let height = (max.y - min.y).max(1e-6);
    let cut = min.y + 0.05 * height;
    let (mut sum, mut n) = (Vec3::ZERO, 0.0f32);
    for v in w.iter().filter(|v| v.y <= cut) {
        sum += *v;
        n += 1.0;
    }
    let c = if n > 0.0 { sum / n } else { (min + max) * 0.5 };
    let foot = Vec3::new(c.x, min.y, c.z);
    Base {
        point: center + turn.inverse() * foot,
        slice: (0..w.len()).filter(|&i| w[i].y <= cut).collect(),
        height,
        width: (max.x - min.x).max(max.z - min.z),
    }
}

/// One candidate surface triangle (world space).
struct Tri {
    a: Vec3,
    b: Vec3,
    c: Vec3,
    n: Vec3,
}

/// Closest point on triangle `t` to `p` (Ericson).
fn closest_on_tri(p: Vec3, t: &[Vec3; 3]) -> Vec3 {
    let [a, b, c] = *t;
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// Distance from `p` to the segment a..b.
fn seg_dist(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let d = b - a;
    let l2 = d.length_squared();
    let t = if l2 > 0.0 {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + d * t).distance(p)
}

/// The eligible surface of a scatter's `on` part (facing, slope, region,
/// height, near), plus what the per-point filters need: the near parts'
/// triangles and the surface's feature edges (borders, creases) for
/// `edge_bias`.
struct Surface {
    tris: Vec<Tri>,
    cdf: Vec<f32>,
    area: f32,
    near: Vec<[Vec3; 3]>,
    near_distance: f32,
    /// The near parts' boxes: copies stand beside them, not inside.
    near_inside: Vec<(Vec3, Vec3)>,
    features: Vec<(Vec3, Vec3)>,
    /// Edge-bias falloff length, meters.
    feature_len: f32,
}

impl Surface {
    fn new(on: &Part, def: &ScatterDef, near_parts: &[Arc<Part>]) -> Result<Self, String> {
        let cos_slope = def
            .max_slope
            .map_or(-2.0, |d| d.clamp(0.0, 180.0).to_radians().cos());
        let up_only = def.faces != "all";
        let near_distance = def.near_distance.unwrap_or(0.5).max(0.0);
        let mut near = Vec::new();
        let mut near_boxes = Vec::new();
        let mut near_inside = Vec::new();
        for p in near_parts {
            let (lo, hi) = p.bounds();
            near_inside.push((lo, hi));
            near_boxes.push((
                lo - Vec3::splat(near_distance),
                hi + Vec3::splat(near_distance),
            ));
            for t in p.indices.as_chunks::<3>().0 {
                near.push(t.map(|i| p.world_vert(i as usize)));
            }
        }
        let mut tris = Vec::new();
        let mut cdf = Vec::new();
        let mut area = 0.0f32;
        for t in on.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| on.world_vert(i as usize));
            let cr = (b - a).cross(c - a);
            let len = cr.length();
            if len < 1e-12 {
                continue;
            }
            let n = cr / len;
            if (up_only && n.y <= 1e-4) || n.y < cos_slope - 1e-6 {
                continue;
            }
            let (mn, mx) = (a.min(b).min(c), a.max(b).max(c));
            if let Some([lo, hi]) = def.region
                && (mx.x < lo[0] || mn.x > hi[0] || mx.z < lo[1] || mn.z > hi[1])
            {
                continue;
            }
            if let Some([lo, hi]) = def.height
                && (mx.y < lo || mn.y > hi)
            {
                continue;
            }
            if !near_boxes.is_empty()
                && !near_boxes
                    .iter()
                    .any(|(lo, hi)| mx.cmpge(*lo).all() && mn.cmple(*hi).all())
            {
                continue;
            }
            area += len * 0.5;
            cdf.push(area);
            tris.push(Tri { a, b, c, n });
        }
        if tris.is_empty() {
            let mut why = Vec::new();
            if let Some(m) = def.max_slope {
                why.push(format!("under max_slope {m}°"));
            }
            if def.region.is_some() {
                why.push("inside region".to_string());
            }
            if let Some([lo, hi]) = def.height {
                why.push(format!("at height {lo}..{hi}"));
            }
            if !def.near.is_empty() {
                why.push(format!(
                    "within near_distance {near_distance} of {:?}",
                    def.near
                ));
            }
            return Err(format!(
                "`{}` has no {}surface{} to scatter on{}",
                on.name,
                if up_only { "upward-facing " } else { "" },
                if why.is_empty() {
                    String::new()
                } else {
                    format!(" {}", why.join(", "))
                },
                if up_only {
                    " (faces: \"all\" uses every face)"
                } else {
                    ""
                }
            ));
        }
        let (features, feature_len) = if def.edge_bias > 0.0 {
            let f = feature_edges(on);
            let (lo, hi) = on.bounds();
            let ext = hi - lo;
            // A tenth of the surface's span (its middle-sized side).
            let mut e = [ext.x, ext.y, ext.z];
            e.sort_by(f32::total_cmp);
            (f, (0.1 * e[1]).max(1e-3))
        } else {
            (Vec::new(), 1.0)
        };
        Ok(Self {
            tris,
            cdf,
            area,
            near,
            near_distance,
            near_inside,
            features,
            feature_len,
        })
    }

    fn target(&self, def: &ScatterDef) -> u32 {
        match (def.count, def.density) {
            (Some(c), _) => c,
            (None, Some(d)) => ((d.max(0.0) * self.area).round() as u32).min(def.max),
            (None, None) => 0,
        }
        .min(MAX_COPIES)
    }

    /// A uniform area-weighted point and its triangle's normal.
    fn point(&self, rng: &mut Rng) -> (Vec3, Vec3) {
        let r = rng.f() * self.area;
        let ti = self
            .cdf
            .partition_point(|&c| c < r)
            .min(self.tris.len() - 1);
        let t = &self.tris[ti];
        let (mut u, mut v) = (rng.f(), rng.f());
        if u + v > 1.0 {
            u = 1.0 - u;
            v = 1.0 - v;
        }
        (t.a + (t.b - t.a) * u + (t.c - t.a) * v, t.n)
    }

    /// The hard per-point filters: region, height, terrain mask, near.
    fn accepts(&self, def: &ScatterDef, p: Vec3, pad: f32) -> bool {
        if let Some([lo, hi]) = def.region
            && (p.x < lo[0] || p.x > hi[0] || p.z < lo[1] || p.z > hi[1])
        {
            return false;
        }
        if let Some([lo, hi]) = def.height
            && (p.y < lo || p.y > hi)
        {
            return false;
        }
        if let (Some(mask), Some(t)) = (&def.mask, &def.terrain) {
            let m = t.mask_at(p.x - t.translation[0], p.z - t.translation[2]);
            let ok = match mask.as_str() {
                "path" => m & 1 != 0,
                "not_path" => m & 1 == 0,
                "pad" => m & 2 != 0,
                "not_pad" => m & 2 == 0,
                _ => true,
            };
            if !ok {
                return false;
            }
        }
        if !self.near.is_empty() {
            let d2 = self.near_distance * self.near_distance;
            let close = self.near.iter().any(|t| {
                let (mn, mx) = (t[0].min(t[1]).min(t[2]), t[0].max(t[1]).max(t[2]));
                let out = (mn - p).max(p - mx).max(Vec3::ZERO);
                out.length_squared() <= d2 && closest_on_tri(p, t).distance_squared(p) <= d2
            });
            if !close {
                return false;
            }
            // Beside the near parts, never under them: a point of the floor
            // covered by the wall it hugs is inside the wall. `pad` is about
            // half a copy's footprint, so a copy's side stops at the wall.
            if self.near_inside.iter().any(|(lo, hi)| {
                p.x > lo.x - pad
                    && p.x < hi.x + pad
                    && p.z > lo.z - pad
                    && p.z < hi.z + pad
                    && p.y >= lo.y - 0.01
                    && p.y < hi.y
            }) {
                return false;
            }
        }
        true
    }

    /// Darts: up to `target` points at least `spacing` apart, through every
    /// filter, edge-biased and clumped as `def` asks.
    fn sample(
        &self,
        def: &ScatterDef,
        target: u32,
        spacing: f32,
        rng: &mut Rng,
    ) -> Vec<(Vec3, Vec3)> {
        let mut centres: Vec<Vec3> = Vec::new();
        let mut attempts = (target as usize).saturating_mul(30).max(64);
        if let Some(c) = &def.clump {
            let mut tries = 0;
            while centres.len() < c.count as usize && tries < 64 * c.count.max(1) as usize {
                tries += 1;
                let (p, _) = self.point(rng);
                if self.accepts(def, p, 0.625 * spacing) {
                    centres.push(p);
                }
            }
            // Most darts miss the clumps: throw more, in proportion.
            let covered =
                (centres.len() as f32 * std::f32::consts::PI * c.radius * c.radius).max(1e-6);
            let factor = (self.area / covered).clamp(1.0, 50.0);
            attempts = ((attempts as f32 * factor) as usize).min(2_000_000);
        }
        if def.edge_bias > 0.0 {
            attempts = attempts.saturating_mul(4).min(2_000_000);
        }
        let cell = spacing.max(1e-4);
        let key = |p: Vec3| {
            (
                (p.x / cell).floor() as i32,
                (p.y / cell).floor() as i32,
                (p.z / cell).floor() as i32,
            )
        };
        let mut grid: HashMap<(i32, i32, i32), Vec<Vec3>> = HashMap::new();
        let mut points: Vec<(Vec3, Vec3)> = Vec::new();
        if def.clump.is_some() && centres.is_empty() {
            return points;
        }
        for _ in 0..attempts {
            if points.len() >= target as usize {
                break;
            }
            let (p, n) = self.point(rng);
            // Both soft draws happen every attempt, so layouts stay stable.
            let (keep_edge, keep_clump) = (rng.f(), rng.f());
            if !self.accepts(def, p, 0.625 * spacing) {
                continue;
            }
            if let Some(c) = &def.clump {
                let d = centres
                    .iter()
                    .map(|q| q.distance(p))
                    .fold(f32::MAX, f32::min);
                let r = c.radius.max(1e-4);
                if d > r || keep_clump >= 1.0 - 0.75 * (d / r) * (d / r) {
                    continue;
                }
            }
            if def.edge_bias > 0.0 && !self.features.is_empty() {
                let d = self
                    .features
                    .iter()
                    .map(|(a, b)| seg_dist(p, *a, *b))
                    .fold(f32::MAX, f32::min);
                let b = def.edge_bias.clamp(0.0, 1.0);
                let w = (1.0 - b) + b * (-d / self.feature_len).exp();
                if keep_edge >= w {
                    continue;
                }
            }
            if spacing > 0.0 {
                let (kx, ky, kz) = key(p);
                let mut close = false;
                'search: for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            if let Some(list) = grid.get(&(kx + dx, ky + dy, kz + dz))
                                && list
                                    .iter()
                                    .any(|q| q.distance_squared(p) < spacing * spacing)
                            {
                                close = true;
                                break 'search;
                            }
                        }
                    }
                }
                if close {
                    continue;
                }
                grid.entry(key(p)).or_default().push(p);
            }
            points.push((p, n));
        }
        points
    }
}

/// `part`'s feature edges in world space: open borders (one face) and
/// creases (faces meeting at more than 30 degrees). Vertices are welded by
/// position first, since flat-shaded meshes split them per face.
fn feature_edges(part: &Part) -> Vec<(Vec3, Vec3)> {
    type Key = (i64, i64, i64);
    let (lo, hi) = part.bounds();
    let q = ((hi - lo).max_element() * 1e-5).max(1e-7);
    let id = |v: Vec3| -> Key {
        (
            (v.x / q).round() as i64,
            (v.y / q).round() as i64,
            (v.z / q).round() as i64,
        )
    };
    let mut edges: HashMap<(Key, Key), (Vec3, Vec3, Vec<Vec3>)> = HashMap::new();
    for t in part.indices.as_chunks::<3>().0 {
        let w = t.map(|i| part.world_vert(i as usize));
        let n = (w[1] - w[0]).cross(w[2] - w[0]).normalize_or_zero();
        if n == Vec3::ZERO {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (w[k], w[(k + 1) % 3]);
            let (ka, kb) = (id(a), id(b));
            let key = if ka <= kb { (ka, kb) } else { (kb, ka) };
            edges
                .entry(key)
                .or_insert_with(|| (a, b, Vec::new()))
                .2
                .push(n);
        }
    }
    let cos = 30f32.to_radians().cos();
    let mut keyed: Vec<((Key, Key), (Vec3, Vec3))> = edges
        .into_iter()
        .filter(|(_, (_, _, ns))| ns.len() == 1 || ns.iter().any(|n| n.dot(ns[0]) < cos))
        .map(|(k, (a, b, _))| (k, (a, b)))
        .collect();
    keyed.sort_by_key(|(k, _)| *k);
    keyed.into_iter().map(|(_, e)| e).collect()
}

/// Where a decal scatter's decals go: `(point on the surface, its outward
/// normal, image width, roll degrees)` per slot that fit, in slot order.
pub fn decal_points(
    on: &Part,
    near: &[Arc<Part>],
    def: &ScatterDef,
    decal: &DecalScatterDef,
) -> Result<Vec<(Vec3, Vec3, f32, f32)>, String> {
    let surface = Surface::new(on, def, near)?;
    let target = surface.target(def);
    let (lo, hi) = (
        decal.size[0].min(decal.size[1]),
        decal.size[0].max(decal.size[1]),
    );
    let spacing = def.min_spacing.unwrap_or(0.8 * hi * decal.aspect.max(1.0));
    let mut rng = Rng(def.seed ^ 0xDECA_1500_0000_0001);
    let points = surface.sample(def, target, spacing, &mut rng);
    Ok(points
        .into_iter()
        .map(|(p, n)| {
            let w = lo + (hi - lo) * rng.f();
            let roll = if decal.roll { rng.f() * 360.0 } else { 0.0 };
            (p, n, w, roll)
        })
        .collect())
}

/// Places the copies. `sets[k]` holds the parts of set k; returns
/// `(name, part)` pairs to add, in slot order.
pub fn build(
    name: &str,
    sets: &[Vec<Arc<Part>>],
    on: &Part,
    near: &[Arc<Part>],
    def: &ScatterDef,
) -> Result<Vec<(String, Part)>, String> {
    if sets.is_empty() || sets.iter().any(Vec::is_empty) {
        return Err("input: name at least one part to scatter".into());
    }
    let surface = Surface::new(on, def, near)?;
    let target = surface.target(def);
    // Each set's world vertices and its upright base (cached; a "random"
    // rotation recomputes the base per copy).
    let mut set_verts: Vec<Vec<Vec3>> = Vec::with_capacity(sets.len());
    for s in sets {
        let mut v = Vec::new();
        for p in s {
            v.extend((0..p.verts.len()).map(|i| p.world_vert(i)));
        }
        if v.is_empty() {
            return Err(format!("input `{}` has no vertices", s[0].name));
        }
        set_verts.push(v);
    }
    let centers: Vec<Vec3> = set_verts
        .iter()
        .map(|v| {
            let (mn, mx) = v.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(a, b), p| (a.min(*p), b.max(*p)),
            );
            (mn + mx) * 0.5
        })
        .collect();
    let upright: Vec<Base> = set_verts
        .iter()
        .zip(&centers)
        .map(|(v, c)| base_of(v, Quat::IDENTITY, *c))
        .collect();
    let [s_lo, s_hi] = {
        let [a, b] = def.scale;
        [a.min(b).max(1e-4), a.max(b).max(1e-4)]
    };
    let spacing = def
        .min_spacing
        .unwrap_or_else(|| 0.8 * s_hi * upright.iter().map(|b| b.width).fold(0.0f32, f32::max));
    let weights: Vec<f32> = (0..sets.len())
        .map(|k| def.weights.get(k).copied().unwrap_or(1.0).max(0.0))
        .collect();
    let wsum: f32 = weights.iter().sum();
    if wsum <= 0.0 {
        return Err("weights: at least one set needs a weight > 0".into());
    }

    let mut rng = Rng(def.seed ^ 0x5CA7_7E25_0000_0001);
    let points = surface.sample(def, target, spacing, &mut rng);
    let mut out = Vec::new();
    let tau = std::f32::consts::TAU;
    for (i, (p, n)) in points.into_iter().enumerate() {
        // Draws in a fixed order per copy so one copy's choices never shift
        // another's.
        let pick = rng.f() * wsum;
        let spin = rng.f() * tau;
        let s = s_lo + (s_hi - s_lo) * rng.f();
        let (r1, r2, r3) = (rng.f(), rng.f(), rng.f());
        let mut acc = 0.0;
        let k = weights
            .iter()
            .position(|w| {
                acc += w;
                pick < acc
            })
            .unwrap_or(sets.len() - 1);
        let up = Vec3::Y
            .lerp(n, def.align.clamp(0.0, 1.0))
            .normalize_or(Vec3::Y);
        let align = Quat::from_rotation_arc(Vec3::Y, up);
        let (turn, yaw) = match def.rotate.as_str() {
            "none" => (Quat::IDENTITY, Quat::IDENTITY),
            "random" => {
                // Uniform random orientation (Shoemake).
                let (a, b) = ((1.0 - r1).sqrt(), r1.sqrt());
                let (t1, t2) = (tau * r2, tau * r3);
                let q = Quat::from_xyzw(a * t1.sin(), a * t1.cos(), b * t2.sin(), b * t2.cos());
                (q.normalize(), Quat::IDENTITY)
            }
            _ => (Quat::IDENTITY, Quat::from_rotation_y(spin)),
        };
        let fresh;
        let base = if turn == Quat::IDENTITY {
            &upright[k]
        } else {
            fresh = base_of(&set_verts[k], turn, centers[k]);
            &fresh
        };
        let rot = align * yaw * turn;
        // Sink until the whole base slice is at or below the surface's
        // tangent plane: a trunk upright on a slope has no downhill gap, a
        // tumbled box rests on its lowest corners. Plus a hair for contact.
        let lift = base
            .slice
            .iter()
            .map(|&i| n.dot(rot * (s * (set_verts[k][i] - base.point))))
            .fold(0.0f32, f32::max)
            // A big rock on a cliff is not buried past 15% of its height
            // (its foot still meets the surface).
            .min(0.15 * s * base.height);
        let sink = def.sink + lift + (0.002 * s * base.height).min(0.005);
        let at = p - up * sink;
        let suffixes = member_suffixes(&sets[k].iter().map(|p| p.name.clone()).collect::<Vec<_>>());
        for (src, suffix) in sets[k].iter().zip(suffixes) {
            let mut part = (**src).clone();
            part.name = format!("{name}_{i}{suffix}");
            part.translation =
                (at + rot * (s * (Vec3::from(src.translation) - base.point))).to_array();
            part.rotation = (rot * quat_of(src.rotation)).normalize().to_array();
            part.scale = (Vec3::from(src.scale) * s).to_array();
            part.visible = true;
            part.occluders = None;
            out.push((part.name.clone(), part));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::commands::{Session, VerifySpec};
    use bevy::math::Vec3;
    use serde_json::{Value, json};

    fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// A 10 x 10 m slab whose top is at `y`, tilted `tilt` degrees about x,
    /// and a 0.15 m rock.
    fn scene(tilt: f32) -> Session {
        let mut s = Session::default();
        ok(s.apply_values(
            Some([("h".to_string(), 0.2)].into_iter().collect()),
            vec![
                json!({"type": "primitive", "name": "ground", "kind": "box",
                       "size": [10, "$h", 10], "translation": [0, "=$h / 2", 0],
                       "rotation": {"x": tilt}}),
                json!({"type": "primitive", "name": "rock", "kind": "sphere", "size": 0.15,
                       "translation": [20, 3, 0]}),
                json!({"type": "material", "name": "*", "color": "#888", "flat": true}),
            ],
        ));
        s
    }

    fn scatter(s: &mut Session, extra: Value) -> String {
        let mut item = json!({"type": "scatter", "name": "rocks", "input": "rock", "on": "ground",
                              "count": 30, "seed": 4});
        if let (Some(o), Some(e)) = (item.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        ok(s.apply_values(
            None,
            vec![item, json!({"type": "hide_part", "part": "rock"})],
        ))
    }

    fn copies(s: &Session) -> Vec<Vec3> {
        let mut out = Vec::new();
        while let Some(p) = s.model.part(&format!("rocks_{}", out.len())) {
            out.push(Vec3::from(p.translation));
        }
        out
    }

    #[test]
    fn places_the_count_and_is_deterministic() {
        let mut s = scene(0.0);
        let text = scatter(&mut s, json!({}));
        assert!(text.contains("placed 30"), "{text}");
        let a = copies(&s);
        assert_eq!(a.len(), 30);
        let mut t = scene(0.0);
        scatter(&mut t, json!({}));
        assert_eq!(a, copies(&t), "same seed, same layout");
        scatter(&mut t, json!({"seed": 5}));
        assert_ne!(a, copies(&t), "another seed moves them");
        // Inside the slab's top, on its surface.
        for p in &a {
            assert!(p.x.abs() <= 5.0 && p.z.abs() <= 5.0, "{p}");
        }
    }

    #[test]
    fn every_copy_touches_the_surface_and_verify_grounded_passes() {
        let mut s = scene(12.0);
        scatter(&mut s, json!({"scale": [0.5, 1.5], "rotate": "random"}));
        let ground = ok(s.model.part("ground").ok_or("no ground"));
        let g = crate::contact::Body::new(ground);
        for i in 0..30 {
            let p = ok(s.model.part(&format!("rocks_{i}")).ok_or("missing copy"));
            assert!(p.visible, "copies are visible though the source is hidden");
            let b = crate::contact::Body::new(p);
            assert!(
                crate::contact::contact(&b, &g).is_some(),
                "rocks_{i} floats"
            );
        }
        let spec: VerifySpec = ok(serde_json::from_value(json!({"grounded": true})));
        let (pass, text) = s.verify(&spec);
        assert!(pass, "{text}");
    }

    #[test]
    fn spacing_and_slope_limits_hold() {
        let mut s = scene(0.0);
        let text = scatter(
            &mut s,
            json!({"count": 400, "min_spacing": 1.2, "rotate": "none"}),
        );
        let pts = copies(&s);
        assert!(pts.len() < 400 && pts.len() > 20, "{}: {text}", pts.len());
        assert!(text.contains("of 400"), "{text}");
        for (i, a) in pts.iter().enumerate() {
            for b in &pts[i + 1..] {
                assert!(a.distance(*b) >= 1.2 - 1e-3, "{a} {b}");
            }
        }
        // A 40° slope refuses max_slope 30 (only its 40° top would qualify;
        // the sides are steeper still) but takes 45.
        let mut t = scene(40.0);
        let err = t
            .apply_values(
                None,
                vec![json!({"type": "scatter", "name": "rocks", "input": "rock",
                            "on": "ground", "count": 5, "max_slope": 30})],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("max_slope"), "{err}");
        scatter(&mut t, json!({"max_slope": 45}));
        assert_eq!(copies(&t).len(), 30);
    }

    #[test]
    fn region_limits_where_they_land() {
        let mut s = scene(0.0);
        scatter(
            &mut s,
            json!({"min_spacing": 0, "region": {"min": [1, -1], "max": [3, 2]}}),
        );
        for p in copies(&s) {
            assert!(p.x >= 0.8 && p.x <= 3.2 && p.z >= -1.2 && p.z <= 2.2, "{p}");
        }
    }

    #[test]
    fn follows_a_changed_on_part() {
        let mut s = scene(0.0);
        scatter(&mut s, json!({"rotate": "none"}));
        let low = copies(&s)[0].y;
        ok(s.apply_values(
            Some([("h".to_string(), 1.2)].into_iter().collect()),
            Vec::new(),
        ));
        let high = copies(&s)[0].y;
        assert!((high - low - 1.0).abs() < 1e-3, "{low} -> {high}");
    }

    #[test]
    fn a_glob_material_skips_slots_that_did_not_fit() {
        // 500 rocks can't fit 10 x 10 m at the default spacing: the pattern
        // material must colour the ones placed and skip the rest, and names
        // of moves and places never count as parts.
        let mut s = scene(0.0);
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "scatter", "name": "rocks", "input": "rock", "on": "ground",
                       "count": 500, "seed": 1, "min_spacing": 2}),
                json!({"type": "move", "name": "rocks_nudge", "parts": ["rocks_0"],
                       "translation": [0, 0, 0]}),
                json!({"type": "material", "name": "rocks_*", "color": "#a33", "flat": true}),
                json!({"type": "hide_part", "part": "rock"}),
            ],
        ));
        let placed = copies(&s).len();
        assert!(placed > 0 && placed < 500, "{placed}");
        for p in s
            .model
            .parts
            .iter()
            .filter(|p| p.name.starts_with("rocks_"))
        {
            assert!(p.material.base_color[0] > 0.5, "{} not coloured", p.name);
        }
    }

    #[test]
    fn density_sets_and_lsystem_leaves_come_along() {
        let mut s = scene(0.0);
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "lsystem", "name": "shrub", "preset": "bush", "seed": 2,
                       "translation": [30, 0, 0]}),
                json!({"type": "primitive", "name": "pebble", "kind": "box", "size": 0.1,
                       "translation": [25, 0.05, 0]}),
                json!({"type": "primitive", "name": "pebble_top", "kind": "box", "size": 0.05,
                       "translation": [25, 0.125, 0]}),
                json!({"type": "scatter", "name": "mix", "on": "ground", "density": 0.1,
                       "input": ["shrub", ["pebble", "pebble_top"]], "seed": 3}),
            ],
        ));
        let n = (0..)
            .take_while(|i| s.model.part(&format!("mix_{i}")).is_some())
            .count();
        assert!((5..=10).contains(&n), "density 0.1 on 100 m²: {n}");
        let mut leaves = 0;
        let mut tops = 0;
        for i in 0..n {
            leaves += usize::from(s.model.part(&format!("mix_{i}_leaves")).is_some());
            tops += usize::from(s.model.part(&format!("mix_{i}_top")).is_some());
        }
        assert!(
            leaves > 0 && tops > 0 && leaves + tops == n,
            "{leaves} {tops} {n}"
        );
        // The pebble's top rides on its pebble.
        for i in 0..n {
            if let (Some(a), Some(b)) = (
                s.model.part(&format!("mix_{i}")),
                s.model.part(&format!("mix_{i}_top")),
            ) {
                let d = Vec3::from(b.translation) - Vec3::from(a.translation);
                assert!((d.length() - 0.075).abs() < 1e-3, "{d}");
            }
        }
    }

    #[test]
    fn suffixes_name_set_members() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::member_suffixes(&v(&["oak"])), v(&[""]));
        assert_eq!(
            super::member_suffixes(&v(&["oak", "oak_leaves"])),
            v(&["", "_leaves"])
        );
        assert_eq!(
            super::member_suffixes(&v(&["t1.trunk", "t1.crown"])),
            v(&[".trunk", ".crown"])
        );
        assert_eq!(super::member_suffixes(&v(&["a", "b"])), v(&["", ".b"]));
    }
}

#[cfg(test)]
mod solid_copies {
    use crate::commands::Session;
    use serde_json::json;
    #[test]
    /// Rotated, scaled copies far from the origin still weld into closed
    /// solids (contact, collisions and booleans need them).
    fn rotated_copies_stay_closed_solids() {
        let mut s = Session::default();
        s.apply_values(None, vec![
            json!({"type":"primitive","name":"slab","kind":"box","size":[30,1,30],"translation":[0,-0.2,0],"rotation":{"x":6}}),
            json!({"type":"primitive","name":"rock_a_raw","kind":"sphere","size":0.35,"translation":[40,0,0]}),
            json!({"type":"transform","name":"rock_a","input":"rock_a_raw","scale":[1.3,0.7,1]}),
            json!({"type":"primitive","name":"rock_b","kind":"box","size":0.4,"translation":[42,0,0]}),
            json!({"type":"lsystem","name":"fir","preset":"pine","seed":5,"translation":[55,0,0]}),
            json!({"type":"scatter","name":"rocks","input":["rock_a","rock_b"],"on":"slab","count":40,"seed":2,"scale":[0.5,1.6],"rotate":"random","align":0.5}),
            json!({"type":"scatter","name":"trees","input":"fir","on":"slab","count":10,"seed":7,"scale":[0.7,1.2]}),
        ]).unwrap_or_else(|e| panic!("{e}"));
        let mut bad = Vec::new();
        // Leaf cards are open sheets on purpose, and copies stay cards.
        assert!(
            s.model
                .parts
                .iter()
                .any(|p| p.name.starts_with("trees_") && p.cards)
        );
        for p in s.model.parts.iter().filter(|p| !p.cards) {
            if let Err(e) = crate::csg::part_to_manifold(p) {
                bad.push(format!(
                    "{} rot={:?} scale={:?}: {}",
                    p.name,
                    p.rotation,
                    p.scale,
                    e.to_string().chars().take(90).collect::<String>()
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

#[cfg(test)]
mod hilly_ground {
    use crate::commands::Session;
    use serde_json::json;
    #[test]
    fn tumbled_rocks_on_hills_all_touch_the_ground() {
        let mut s = Session::default();
        s.apply_values(None, vec![
            json!({"type":"primitive","name":"slab","kind":"box","size":[30,1,30],"translation":[0,-0.2,0],"rotation":{"x":6}}),
            json!({"type":"primitive","name":"hill_a_s","kind":"sphere","size":1}),
            json!({"type":"transform","name":"hill_a","input":"hill_a_s","translation":[-5,0,-3],"scale":[7,2.2,6]}),
            json!({"type":"transform","name":"hill_b","input":"hill_a_s","translation":[6,0,4],"scale":[5,1.5,5]}),
            json!({"type":"boolean","name":"g1","op":"union","a":"slab","b":"hill_a"}),
            json!({"type":"boolean","name":"ground","op":"union","a":"g1","b":"hill_b"}),
            json!({"type":"primitive","name":"rock_a_raw","kind":"sphere","size":0.35,"translation":[40,0,0]}),
            json!({"type":"transform","name":"rock_a","input":"rock_a_raw","scale":[1.3,0.7,1]}),
            json!({"type":"primitive","name":"rock_b","kind":"box","size":0.4,"translation":[42,0,0]}),
            json!({"type":"scatter","name":"rocks","input":["rock_a","rock_b"],"on":"ground","count":80,"seed":2,"scale":[0.5,1.6],"rotate":"random","align":0.5}),
        ]).unwrap_or_else(|e| panic!("{e}"));
        let g = crate::contact::Body::new(s.model.part("ground").unwrap_or_else(|| panic!()));
        let mut bad = Vec::new();
        for i in 0..80 {
            let p = s
                .model
                .part(&format!("rocks_{i}"))
                .unwrap_or_else(|| panic!());
            let b = crate::contact::Body::new(p);
            if crate::contact::contact(&b, &g).is_none() {
                let solid = crate::csg::part_to_manifold(p).is_ok();
                let (lo, hi) = p.bounds();
                bad.push(format!(
                    "rocks_{i} solid={solid} t={:?} lo={lo} hi={hi} scale={:?} verts={}",
                    p.translation,
                    p.scale,
                    p.verts.len()
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

#[cfg(test)]
mod smart_placement {
    use crate::commands::Session;
    use bevy::math::Vec3;
    use serde_json::{Value, json};

    fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// A 10 x 10 m slab (top at y 0.2), a wall along its +x edge and a
    /// pebble to strew.
    fn room() -> Session {
        let mut s = Session::default();
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "floor", "kind": "box", "size": [10, 0.2, 10],
                       "translation": [0, 0.1, 0]}),
                json!({"type": "primitive", "name": "wall", "kind": "box", "size": [0.2, 2, 10],
                       "translation": [4.9, 1.2, 0]}),
                json!({"type": "primitive", "name": "pebble", "kind": "sphere", "size": 0.05,
                       "translation": [30, 1, 0]}),
                json!({"type": "hide_part", "part": "pebble"}),
            ],
        ));
        s
    }

    fn strew(s: &mut Session, extra: Value) -> Vec<Vec3> {
        let mut item = json!({"type": "scatter", "name": "bits", "input": "pebble", "on": "floor",
                              "count": 40, "seed": 9, "min_spacing": 0.05});
        if let (Some(o), Some(e)) = (item.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        ok(s.apply_values(None, vec![item]));
        let mut out = Vec::new();
        while let Some(p) = s.model.part(&format!("bits_{}", out.len())) {
            out.push(Vec3::from(p.translation));
        }
        out
    }

    #[test]
    fn near_keeps_copies_by_the_wall() {
        let mut s = room();
        let pts = strew(&mut s, json!({"near": "wall", "near_distance": 0.6}));
        assert_eq!(pts.len(), 40);
        // The wall's face is at x 4.8; pebbles are 5 cm across.
        for p in &pts {
            assert!(p.x >= 4.8 - 0.6 - 0.06, "{p}");
            // Beside the wall, never on the floor strip under it.
            assert!(p.x < 4.8, "{p} is inside the wall");
        }
        let err = s
            .apply_values(
                None,
                vec![
                    json!({"type": "scatter", "name": "bits", "input": "pebble", "on": "floor",
                            "count": 4, "near_distance": 1}),
                ],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("near_distance goes with `near`"), "{err}");
    }

    #[test]
    fn edge_bias_pulls_copies_to_the_border() {
        let border = |pts: &[Vec3]| {
            pts.iter()
                .map(|p| 5.0 - p.x.abs().max(p.z.abs()))
                .sum::<f32>()
                / pts.len().max(1) as f32
        };
        let mut s = room();
        let uniform = border(&strew(&mut s, json!({})));
        let biased = border(&strew(&mut s, json!({"edge_bias": 1})));
        assert!(biased < uniform * 0.6, "{biased} vs {uniform}");
    }

    #[test]
    fn clumps_gather_copies() {
        let mut s = room();
        let pts = strew(
            &mut s,
            json!({"clump": {"count": 1, "radius": 0.8}, "count": 20}),
        );
        assert!(pts.len() >= 10, "{}", pts.len());
        for a in &pts {
            for b in &pts {
                let d = Vec3::new(a.x - b.x, 0.0, a.z - b.z).length();
                assert!(d <= 1.7, "{a} {b}");
            }
        }
        let err = s
            .apply_values(
                None,
                vec![
                    json!({"type": "scatter", "name": "bits", "input": "pebble", "on": "floor",
                            "count": 4, "clump": {"count": 0, "radius": 1}}),
                ],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("clump"), "{err}");
    }

    #[test]
    fn terrain_masks_and_height_limit_where_they_land() {
        let mut s = Session::default();
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "terrain", "name": "ground", "size": [30, 30], "resolution": 0.5,
                       "height": 3, "seed": 4,
                       "paths": [{"points": [[-15, 0], [15, 0]], "width": 3, "depth": 0.2}],
                       "flatten": [{"at": [8, 8], "radius": 3, "falloff": 2}]}),
                json!({"type": "primitive", "name": "pebble", "kind": "sphere", "size": 0.05,
                       "translation": [60, 1, 0]}),
                json!({"type": "hide_part", "part": "pebble"}),
                json!({"type": "scatter", "name": "onpath", "input": "pebble", "on": "ground",
                       "count": 30, "mask": "path", "min_spacing": 0.1}),
                json!({"type": "scatter", "name": "offpath", "input": "pebble", "on": "ground",
                       "count": 30, "mask": "not_path", "min_spacing": 0.1}),
                json!({"type": "scatter", "name": "pad", "input": "pebble", "on": "ground",
                       "count": 10, "mask": "pad", "min_spacing": 0.1}),
                json!({"type": "scatter", "name": "low", "input": "pebble", "on": "ground",
                       "count": 20, "height": [0, 1.0], "min_spacing": 0.1}),
            ],
        ));
        let pts = |prefix: &str| {
            let mut out = Vec::new();
            while let Some(p) = s.model.part(&format!("{prefix}_{}", out.len())) {
                out.push(Vec3::from(p.translation));
            }
            out
        };
        let on = pts("onpath");
        assert_eq!(on.len(), 30);
        assert!(on.iter().all(|p| p.z.abs() <= 1.5 + 0.05), "{on:?}");
        let off = pts("offpath");
        assert!(off.iter().all(|p| p.z.abs() >= 1.5 - 0.05), "{off:?}");
        let pad = pts("pad");
        assert!(!pad.is_empty());
        assert!(
            pad.iter()
                .all(|p| ((p.x - 8.0).powi(2) + (p.z - 8.0).powi(2)).sqrt() <= 3.05)
        );
        let low = pts("low");
        assert!(!low.is_empty());
        assert!(low.iter().all(|p| p.y <= 1.05), "{low:?}");
        // A mask needs a terrain under it.
        let err = s
            .apply_values(
                None,
                vec![
                    json!({"type": "primitive", "name": "slab", "kind": "box", "size": [4, 0.2, 4]}),
                    json!({"type": "scatter", "name": "m", "input": "pebble", "on": "slab",
                           "count": 3, "mask": "path"}),
                ],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("not a terrain"), "{err}");
    }
}
