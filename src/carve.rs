//! `carve`: interiors by subtraction. A solid mass has rooms (cutters) taken
//! out of it; the result is only the faces that look into the void, as
//! one-sided sheets (merged into big coplanar polygons), optionally split by
//! facing (floor / wall / ceiling) and by room, with the rooms and the
//! openings between them reported as markers.
//!
//! The geometry is Manifold's `solid - union(cutters)`. A face of that result
//! looks into the void when the point just in front of it lies inside a
//! cutter (that also names the room it belongs to); the rest is the outer
//! skin of the mass. Where two cutters touch or overlap their shared wall is
//! gone from the union, so a doorway is just a third cutter through a wall.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::math::Vec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ModelerError, Part};

/// A room given as a footprint instead of a cutter part: extruded from
/// `base` up by `height` and hidden. Its name works as a cutter name
/// (`split_by` "cutter", markers).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RoomDef {
    /// The room's name (a hidden part of that name is made).
    pub name: String,
    /// Floor outline on the ground plane, [[x, z], ...] (3+ points).
    pub footprint: Vec<[f32; 2]>,
    /// Floor to ceiling, meters.
    pub height: f32,
    /// y of the floor (default 0).
    #[serde(default)]
    pub base: f32,
}

/// A floor slab inside the carved volume (a mezzanine, a loft, a catwalk):
/// added to the carved mass after the cutters are taken out, so its top reads
/// as floor, its underside as ceiling and its edges as wall, with the same
/// merging, splitting, materials and `blocks` colliders as everything else.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SlabDef {
    /// Outline on the ground plane [[x, z], ...] (3+ points); more rings are
    /// holes (a stairwell, the open middle of a ring balcony).
    pub footprint: Vec<Vec<[f32; 2]>>,
    /// y of the walking surface.
    pub top: f32,
    /// Slab thickness, meters (default 0.3), downward from `top`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness: Option<f32>,
}

/// A carve item's fields (besides `name`).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CarveDef {
    /// The closed mass to carve. Omit it and a box is made around the
    /// cutters, `wall` meters thicker on every side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solid: Option<String>,
    /// Closed parts to take out: rooms, halls, doorways, windows. Exact
    /// names, listed in priority order (a point inside two belongs to the
    /// first). Hide them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cutters: Vec<String>,
    /// Rooms given as footprints, joined to the cutters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rooms: Vec<RoomDef>,
    /// Floor slabs added inside the carved volume: mezzanines, lofts,
    /// catwalks (see [`SlabDef`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slabs: Vec<SlabDef>,
    /// With no `solid`: wall thickness of the generated mass (default 0.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall: Option<f32>,
    /// "inner" (default: only the faces looking into the void), "outer"
    /// (only the mass's outside skin, as `name_outer`) or "both".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<String>,
    /// Merge coplanar triangles into whole polygons (default true): walls
    /// are single quads, tile materials stay clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<bool>,
    /// "none" (default: one part `name`), "facing" (`name_floor`,
    /// `name_wall`, `name_ceiling`), "cutter" (`name_<cutter>`, each wearing
    /// its cutter's material) or "both" (`name_<cutter>_<facing>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_by: Option<String>,
    /// Also make markers: a `room` per cutter that is a room and an
    /// `opening` per cutter that connects rooms (or a room to the outside),
    /// named `<name>_room_<cutter>` / `<name>_opening_<cutter>`, with their
    /// boxes and (openings) the rooms they connect in `data`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub markers: bool,
    /// "none" (default), "mesh" (a static trimesh collider over every
    /// visible sheet) or "blocks" (the carved mass cut into axis-aligned
    /// boxes, made as `block` markers `<name>_blk_<i>` carrying the game's
    /// `Block` component: for games that build colliders from markers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collider: Option<String>,
}

pub const FIELDS: &[&str] = &[
    "solid", "cutters", "rooms", "slabs", "wall", "keep", "merge", "split_by", "markers",
    "collider",
];

const DEFAULT_WALL: f32 = 0.3;
const FACINGS: [&str; 3] = ["floor", "wall", "ceiling"];

pub fn validate(def: &CarveDef) -> Result<(), String> {
    if def.cutters.is_empty() && def.rooms.is_empty() {
        return Err("needs at least one cutter or room".into());
    }
    if let Some(k) = &def.keep
        && !["inner", "outer", "both"].contains(&k.as_str())
    {
        return Err(format!("keep: must be inner, outer or both, got `{k}`"));
    }
    if let Some(s) = &def.split_by
        && !["none", "facing", "cutter", "both"].contains(&s.as_str())
    {
        return Err(format!(
            "split_by: must be none, facing, cutter or both, got `{s}`"
        ));
    }
    if let Some(c) = &def.collider
        && !["none", "mesh", "blocks"].contains(&c.as_str())
    {
        return Err(format!("collider: must be none, mesh or blocks, got `{c}`"));
    }
    if let Some(w) = def.wall
        && (w.is_nan() || w <= 0.0)
    {
        return Err("wall: must be > 0 meters".into());
    }
    for r in &def.rooms {
        if r.name.is_empty() {
            return Err("rooms: every room needs a name".into());
        }
        if r.footprint.len() < 3 || r.footprint.iter().flatten().any(|c| !c.is_finite()) {
            return Err(format!(
                "rooms: `{}` footprint needs 3+ finite [x, z] points",
                r.name
            ));
        }
        if r.height.is_nan() || r.height <= 0.0 {
            return Err(format!("rooms: `{}` height must be > 0", r.name));
        }
    }
    for (i, sl) in def.slabs.iter().enumerate() {
        if sl.footprint.is_empty()
            || sl.footprint.iter().any(|r| r.len() < 3)
            || sl
                .footprint
                .iter()
                .flatten()
                .flatten()
                .any(|c| !c.is_finite())
        {
            return Err(format!(
                "slabs[{i}]: footprint needs rings of 3+ finite [x, z] points"
            ));
        }
        if !sl.top.is_finite() || sl.thickness.is_some_and(|t| t.is_nan() || t <= 0.0) {
            return Err(format!("slabs[{i}]: top must be finite, thickness > 0"));
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for n in def
        .cutters
        .iter()
        .map(String::as_str)
        .chain(def.rooms.iter().map(|r| r.name.as_str()))
    {
        if seen.contains(&n) {
            return Err(format!("cutter `{n}` is listed twice"));
        }
        seen.push(n);
    }
    Ok(())
}

fn keep_inner(def: &CarveDef) -> bool {
    def.keep.as_deref() != Some("outer")
}

fn keep_outer(def: &CarveDef) -> bool {
    matches!(def.keep.as_deref(), Some("outer" | "both"))
}

fn split_cutter(def: &CarveDef) -> bool {
    matches!(def.split_by.as_deref(), Some("cutter" | "both"))
}

fn split_facing(def: &CarveDef) -> bool {
    matches!(def.split_by.as_deref(), Some("facing" | "both"))
}

/// Every cutter name: listed cutters, then room names.
pub fn cutter_names(def: &CarveDef) -> Vec<String> {
    def.cutters
        .iter()
        .cloned()
        .chain(def.rooms.iter().map(|r| r.name.clone()))
        .collect()
}

fn sheet_name(name: &str, cutter: Option<&str>, facing: Option<&str>) -> String {
    let mut s = name.to_string();
    if let Some(c) = cutter {
        s += &format!("_{c}");
    }
    if let Some(f) = facing {
        s += &format!("_{f}");
    }
    s
}

/// The visible sheets this carve can make (some may come out empty and are
/// then skipped).
pub fn sheet_names(name: &str, def: &CarveDef) -> Vec<String> {
    let mut out = Vec::new();
    if keep_inner(def) {
        let cutters = cutter_names(def);
        let cs: Vec<Option<&str>> = if split_cutter(def) {
            cutters.iter().map(|c| Some(c.as_str())).collect()
        } else {
            vec![None]
        };
        let fs: Vec<Option<&str>> = if split_facing(def) {
            FACINGS.iter().map(|f| Some(*f)).collect()
        } else {
            vec![None]
        };
        for c in &cs {
            for f in &fs {
                out.push(sheet_name(name, *c, *f));
            }
        }
    }
    if keep_outer(def) {
        out.push(format!("{name}_outer"));
    }
    out
}

/// Every part name the op writes: the sheets and the rooms' hidden cutters.
pub fn output_names(name: &str, def: &CarveDef) -> Vec<String> {
    let mut v: Vec<String> = def.rooms.iter().map(|r| r.name.clone()).collect();
    v.extend(sheet_names(name, def));
    v
}

/// The parts this carve reads (the solid and the listed cutters).
pub fn reads(def: &CarveDef) -> Vec<&str> {
    let mut v: Vec<&str> = def.cutters.iter().map(String::as_str).collect();
    if let Some(s) = &def.solid {
        v.push(s);
    }
    v
}

// ---------------------------------------------------------------- analysis

/// A cutter's world box.
pub type BoxOf = (String, Vec3, Vec3);

/// Which cutters are rooms and which are openings between them.
#[derive(Debug, Default, PartialEq)]
pub struct Analysis {
    /// Indices into the cutter list.
    pub rooms: Vec<usize>,
    /// (opening cutter, the cutters it connects, whether it reaches the
    /// outside of the mass).
    pub openings: Vec<(usize, Vec<usize>, bool)>,
}

fn touches(a: &BoxOf, b: &BoxOf) -> bool {
    const TOL: f32 = 1e-3;
    (0..3).all(|k| a.1[k] <= b.2[k] + TOL && b.1[k] <= a.2[k] + TOL)
}

/// Rooms and openings from the cutters' boxes. An opening is a cutter that
/// touches two or more others which do not touch each other (a door between
/// two rooms), or one that touches a single other and pokes out of the mass
/// (a way outside). Everything else is a room. `solid` is the mass's box.
pub fn analyze(boxes: &[BoxOf], solid: Option<(Vec3, Vec3)>) -> Analysis {
    let n = boxes.len();
    let near: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            (0..n)
                .filter(|&j| j != i && touches(&boxes[i], &boxes[j]))
                .collect()
        })
        .collect();
    let mut out = Analysis::default();
    let mut is_opening = vec![false; n];
    for i in 0..n {
        let mine = &near[i];
        let outside = solid.is_some_and(|(lo, hi)| {
            (0..3).any(|k| boxes[i].1[k] < lo[k] - 1e-3 || boxes[i].2[k] > hi[k] + 1e-3)
        });
        // A cutter that is simply bigger than all its neighbours is a room
        // they open onto, not a door.
        let pairwise_apart = mine.len() >= 2
            && mine.iter().enumerate().all(|(a, &x)| {
                mine[a + 1..]
                    .iter()
                    .all(|&y| !touches(&boxes[x], &boxes[y]))
            });
        let volume = |b: &BoxOf| {
            let d = b.2 - b.1;
            d.x * d.y * d.z
        };
        let smaller = mine.iter().all(|&j| volume(&boxes[i]) <= volume(&boxes[j]));
        if (pairwise_apart || (mine.len() == 1 && outside)) && smaller {
            is_opening[i] = true;
            out.openings.push((i, mine.clone(), outside));
        }
    }
    out.rooms = (0..n).filter(|i| !is_opening[*i]).collect();
    out
}

/// A derived marker: kind, name, centre, size and data.
#[derive(Clone, Debug, PartialEq)]
pub struct DerivedMarker {
    pub name: String,
    pub kind: &'static str,
    pub position: Vec3,
    pub size: [f32; 3],
    pub data: serde_json::Value,
    /// Game components for a `.bsn` export (blocks carry `Block`).
    pub components: serde_json::Map<String, serde_json::Value>,
}

/// The `room` / `opening` markers of a carve, from its cutters' boxes.
pub fn markers(name: &str, boxes: &[BoxOf], solid: Option<(Vec3, Vec3)>) -> Vec<DerivedMarker> {
    let a = analyze(boxes, solid);
    let mk = |i: usize, kind: &'static str, data: serde_json::Value| {
        let (lo, hi) = (boxes[i].1, boxes[i].2);
        DerivedMarker {
            name: format!("{name}_{kind}_{}", boxes[i].0),
            kind,
            position: (lo + hi) * 0.5,
            size: (hi - lo).to_array(),
            data,
            components: Default::default(),
        }
    };
    let mut out = Vec::new();
    for &i in &a.rooms {
        out.push(mk(i, "room", serde_json::json!({"cutter": boxes[i].0})));
    }
    for (i, conn, outside) in &a.openings {
        let connects: Vec<&str> = conn.iter().map(|&j| boxes[j].0.as_str()).collect();
        out.push(mk(
            *i,
            "opening",
            serde_json::json!({"cutter": boxes[*i].0, "connects": connects, "outside": outside}),
        ));
    }
    out
}

/// World box of a room footprint.
pub fn room_box(r: &RoomDef) -> BoxOf {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in &r.footprint {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (
        r.name.clone(),
        Vec3::new(lo[0], r.base, lo[1]),
        Vec3::new(hi[0], r.base + r.height, hi[1]),
    )
}

// ----------------------------------------------------------------- blocks

const MAX_CELLS: usize = 400_000;

fn axis_coords(vals: impl Iterator<Item = f32>) -> Vec<f32> {
    let mut v: Vec<f32> = vals.collect();
    v.sort_by(f32::total_cmp);
    let mut out: Vec<f32> = Vec::new();
    for x in v {
        if out.last().is_none_or(|l| x - l > 1e-3) {
            out.push(x);
        }
    }
    out
}

/// The carved mass (the walls, floor and ceiling around the rooms) as
/// axis-aligned boxes `(centre, size)`: the grid of every vertex coordinate
/// is cut into cells, the cells inside the solid kept and merged into long
/// runs. Exact for axis-aligned rooms; slanted faces become steps.
pub fn blocks(
    name: &str,
    def: &CarveDef,
    lookup: &dyn Fn(&str) -> Result<Arc<Part>, ModelerError>,
) -> Result<Vec<(Vec3, Vec3)>, ModelerError> {
    let Solved { shape, .. } = solve(name, def, lookup)?;
    let tris = world_tris(&shape);
    let xs = axis_coords(shape.verts.iter().map(|v| v[0]));
    let ys = axis_coords(shape.verts.iter().map(|v| v[1]));
    let zs = axis_coords(shape.verts.iter().map(|v| v[2]));
    let (nx, ny, nz) = (xs.len() - 1, ys.len() - 1, zs.len() - 1);
    if nx * ny * nz > MAX_CELLS {
        return Err(ModelerError::Invalid(format!(
            "carve `{name}`: collider \"blocks\" would need {} cells (over {MAX_CELLS}): the rooms have too many distinct edges; use collider \"mesh\"",
            nx * ny * nz
        )));
    }
    let (lo, hi) = shape.bounds();
    let idx = |x: usize, y: usize, z: usize| (x * ny + y) * nz + z;
    let mut occ = vec![false; nx * ny * nz];
    for x in 0..nx {
        for y in 0..ny {
            for z in 0..nz {
                let c = Vec3::new(
                    (xs[x] + xs[x + 1]) * 0.5,
                    (ys[y] + ys[y + 1]) * 0.5,
                    (zs[z] + zs[z + 1]) * 0.5,
                );
                occ[idx(x, y, z)] = inside(&tris, lo, hi, c);
            }
        }
    }
    let mut used = vec![false; occ.len()];
    let mut out = Vec::new();
    for x in 0..nx {
        for y in 0..ny {
            for z in 0..nz {
                if !occ[idx(x, y, z)] || used[idx(x, y, z)] {
                    continue;
                }
                let free = |x: usize, y: usize, z: usize| occ[idx(x, y, z)] && !used[idx(x, y, z)];
                let mut x1 = x;
                while x1 + 1 < nx && free(x1 + 1, y, z) {
                    x1 += 1;
                }
                let mut z1 = z;
                while z1 + 1 < nz && (x..=x1).all(|i| free(i, y, z1 + 1)) {
                    z1 += 1;
                }
                let mut y1 = y;
                while y1 + 1 < ny && (x..=x1).all(|i| (z..=z1).all(|k| free(i, y1 + 1, k))) {
                    y1 += 1;
                }
                for i in x..=x1 {
                    for j in y..=y1 {
                        for k in z..=z1 {
                            used[idx(i, j, k)] = true;
                        }
                    }
                }
                let (a, b) = (
                    Vec3::new(xs[x], ys[y], zs[z]),
                    Vec3::new(xs[x1 + 1], ys[y1 + 1], zs[z1 + 1]),
                );
                out.push(((a + b) * 0.5, b - a));
            }
        }
    }
    Ok(out)
}

/// The `block` markers of a carve with `collider: "blocks"`.
pub fn block_markers(name: &str, boxes: &[(Vec3, Vec3)]) -> Vec<DerivedMarker> {
    boxes
        .iter()
        .enumerate()
        .map(|(i, (c, s))| DerivedMarker {
            name: format!("{name}_blk_{i}"),
            kind: "block",
            position: *c,
            size: s.to_array(),
            data: serde_json::Value::Null,
            components: serde_json::Map::from_iter([(
                "Block".to_string(),
                serde_json::Value::Object(Default::default()),
            )]),
        })
        .collect()
}

// ------------------------------------------------------------------ build

fn world_tris(p: &Part) -> Vec<[Vec3; 3]> {
    p.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            [
                p.world_vert(t[0] as usize),
                p.world_vert(t[1] as usize),
                p.world_vert(t[2] as usize),
            ]
        })
        .collect()
}

/// Ray-parity point-in-solid test along an off-axis direction.
fn inside(tris: &[[Vec3; 3]], lo: Vec3, hi: Vec3, p: Vec3) -> bool {
    if (0..3).any(|k| p[k] < lo[k] || p[k] > hi[k]) {
        return false;
    }
    let dir = Vec3::new(0.571_215, 0.371_193, 0.731_057).normalize();
    let mut hits = 0;
    for t in tris {
        let e1 = t[1] - t[0];
        let e2 = t[2] - t[0];
        let h = dir.cross(e2);
        let a = e1.dot(h);
        if a.abs() < 1e-12 {
            continue;
        }
        let f = 1.0 / a;
        let s = p - t[0];
        let u = f * s.dot(h);
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = f * dir.dot(q);
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        if f * e2.dot(q) > 1e-9 {
            hits += 1;
        }
    }
    hits % 2 == 1
}

struct Tri {
    p: [Vec3; 3],
    n: Vec3,
}

/// A surface polygon being collected: its triangles (as welded vertex ids),
/// the shared positions and the plane normal.
#[derive(Default)]
struct Group {
    tris: Vec<[u32; 3]>,
    n: Vec3,
}

#[derive(Default)]
struct Sheet {
    verts: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl Sheet {
    fn polygon(&mut self, pos: &[Vec3], n: Vec3, tris: &[[u32; 3]]) {
        let base = self.verts.len() as u32;
        for p in pos {
            self.verts.push(p.to_array());
            self.normals.push(n.to_array());
        }
        for t in tris {
            self.indices.extend([base + t[0], base + t[1], base + t[2]]);
        }
    }
}

fn weld_key(p: Vec3) -> (i64, i64, i64) {
    (
        (p.x * 1e4).round() as i64,
        (p.y * 1e4).round() as i64,
        (p.z * 1e4).round() as i64,
    )
}

fn pip(poly: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut c = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            c = !c;
        }
        j = i;
    }
    c
}

fn area2(poly: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    a * 0.5
}

/// Merges a coplanar run of triangles (welded ids into `pos`) into whole
/// polygons: boundary loops, holes matched to their outer loops, then a
/// constrained triangulation. Returns (positions, triangles) or None when
/// the run can't be merged (the caller keeps the triangles as they are).
fn merge_group(pos: &[Vec3], tris: &[[u32; 3]], n: Vec3) -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
    let mut count: HashMap<(u32, u32), u32> = HashMap::new();
    for t in tris {
        for k in 0..3 {
            *count.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
        }
    }
    if count.values().any(|&c| c > 1) {
        return None;
    }
    let mut out: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(a, b) in count.keys() {
        if !count.contains_key(&(b, a)) {
            out.entry(a).or_default().push(b);
        }
    }
    let mut starts: Vec<u32> = out.keys().copied().collect();
    starts.sort_unstable();
    let mut loops: Vec<Vec<u32>> = Vec::new();
    for s in starts {
        while let Some(&first) = out.get(&s).and_then(|v| v.last()) {
            let mut lp = vec![s];
            let mut cur = first;
            if let Some(v) = out.get_mut(&s) {
                v.pop();
            }
            let mut guard = 0;
            while cur != s {
                lp.push(cur);
                let next = out.get_mut(&cur)?.pop()?;
                cur = next;
                guard += 1;
                if guard > tris.len() * 3 + 8 {
                    return None;
                }
            }
            loops.push(lp);
        }
    }
    if loops.is_empty() {
        return None;
    }
    let a = if n.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let u = a.cross(n).normalize();
    let v = n.cross(u);
    let to2 = |p: Vec3| [p.dot(u) as f64, p.dot(v) as f64];
    let polys: Vec<Vec<[f64; 2]>> = loops
        .iter()
        .map(|l| l.iter().map(|&i| to2(pos[i as usize])).collect())
        .collect();
    let areas: Vec<f64> = polys.iter().map(|p| area2(p)).collect();
    let outers: Vec<usize> = (0..polys.len()).filter(|&i| areas[i] > 0.0).collect();
    if outers.is_empty() {
        return None;
    }
    let mut holes: HashMap<usize, Vec<usize>> = HashMap::new();
    for h in (0..polys.len()).filter(|&i| areas[i] <= 0.0) {
        let host = outers
            .iter()
            .copied()
            .filter(|&o| pip(&polys[o], polys[h][0]))
            .min_by(|&x, &y| areas[x].total_cmp(&areas[y]))?;
        holes.entry(host).or_default().push(h);
    }
    let mut out_pos: Vec<Vec3> = Vec::new();
    let mut out_tris: Vec<[u32; 3]> = Vec::new();
    for o in outers {
        let mut rings = vec![polys[o].clone()];
        let mut ids: Vec<u32> = loops[o].clone();
        for &h in holes.get(&o).map(Vec::as_slice).unwrap_or(&[]) {
            rings.push(polys[h].clone());
            ids.extend(&loops[h]);
        }
        let tri = manifold_csg::triangulate_polygons(&rings, 1e-9)?;
        let flat: Vec<[f64; 2]> = rings.iter().flatten().copied().collect();
        let base = out_pos.len() as u32;
        out_pos.extend(ids.iter().map(|&i| pos[i as usize]));
        for t in tri {
            let (p, q, r) = (
                flat[t[0] as usize],
                flat[t[1] as usize],
                flat[t[2] as usize],
            );
            let s = (q[0] - p[0]) * (r[1] - p[1]) - (r[0] - p[0]) * (q[1] - p[1]);
            if s.abs() < 1e-12 {
                continue;
            }
            if s > 0.0 {
                out_tris.push([base + t[0], base + t[1], base + t[2]]);
            } else {
                out_tris.push([base + t[0], base + t[2], base + t[1]]);
            }
        }
    }
    if out_tris.is_empty() {
        return None;
    }
    Some((out_pos, out_tris))
}

type PlaneKey = (usize, u8, (i32, i32, i32), i64);

fn plane_key(cutter: usize, kind: u8, n: Vec3, p: Vec3) -> PlaneKey {
    let q = |x: f32| (x * 1000.0).round() as i32;
    (
        cutter,
        kind,
        (q(n.x), q(n.y), q(n.z)),
        (n.dot(p) * 1000.0).round() as i64,
    )
}

fn facing_of(n: Vec3) -> usize {
    if n.y > 0.5 {
        0
    } else if n.y < -0.5 {
        2
    } else {
        1
    }
}

/// The carved solid and what it was made from.
struct Solved {
    cutters: Vec<(String, Arc<Part>)>,
    room_parts: Vec<Part>,
    solid: Arc<Part>,
    shape: Part,
}

/// Cutters, mass and the carved result (`solid - union(cutters)`).
fn solve(
    name: &str,
    def: &CarveDef,
    lookup: &dyn Fn(&str) -> Result<Arc<Part>, ModelerError>,
) -> Result<Solved, ModelerError> {
    validate(def).map_err(|e| ModelerError::Invalid(format!("carve `{name}`: {e}")))?;
    let fail = |m: String| ModelerError::Invalid(format!("carve `{name}`: {m}"));

    // Cutters: listed parts, then the rooms' extrusions (hidden parts).
    let mut cutters: Vec<(String, Arc<Part>)> = Vec::new();
    for c in &def.cutters {
        cutters.push((c.clone(), lookup(c)?));
    }
    let mut room_parts: Vec<Part> = Vec::new();
    for r in &def.rooms {
        let mut p = crate::csg::extrude(
            &r.name,
            std::slice::from_ref(&r.footprint),
            r.height,
            0.0,
            [1.0, 1.0],
            0,
            0.0,
        )?;
        p.translation = [0.0, r.base, 0.0];
        p.visible = false;
        cutters.push((r.name.clone(), Arc::new(p.clone())));
        room_parts.push(p);
    }

    // The mass.
    let mut cb = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for (_, p) in &cutters {
        let (lo, hi) = p.bounds();
        cb = (cb.0.min(lo), cb.1.max(hi));
    }
    let solid: Arc<Part> = match &def.solid {
        Some(s) => lookup(s)?,
        None => {
            let w = def.wall.unwrap_or(DEFAULT_WALL);
            let (lo, hi) = (cb.0 - Vec3::splat(w), cb.1 + Vec3::splat(w));
            let mut b = crate::primitives::primitive(
                crate::PrimitiveKind::Box,
                (hi - lo).to_array(),
                &format!("{name}_mass"),
            )?;
            b.translation = ((lo + hi) * 0.5).to_array();
            Arc::new(b)
        }
    };

    let mut void: Option<manifold_csg::Manifold> = None;
    for (n, p) in &cutters {
        let m = crate::csg::part_to_manifold(p).map_err(|e| fail(format!("cutter `{n}`: {e}")))?;
        void = Some(match void {
            None => m,
            Some(v) => v.union(&m),
        });
    }
    let Some(void) = void else {
        return Err(fail("needs at least one cutter".into()));
    };
    let mass = crate::csg::part_to_manifold(&solid)
        .map_err(|e| fail(format!("solid `{}`: {e}", solid.name)))?;
    let mut carved = mass.difference(&void);
    for (i, sl) in def.slabs.iter().enumerate() {
        let t = sl.thickness.unwrap_or(0.3);
        let mut p = crate::csg::extrude(
            &format!("{name}_slab{i}"),
            &sl.footprint,
            t,
            0.0,
            [1.0, 1.0],
            0,
            0.0,
        )
        .map_err(|e| fail(format!("slabs[{i}]: {e}")))?;
        p.translation = [0.0, sl.top - t, 0.0];
        let m = crate::csg::part_to_manifold(&p).map_err(|e| fail(format!("slabs[{i}]: {e}")))?;
        carved = carved.union(&m);
    }
    carved
        .status()
        .map_err(|e| fail(format!("boolean failed: {e}")))?;
    if carved.is_empty() {
        return Err(fail(format!(
            "nothing is left: the cutters cover all of `{}`. Make the mass bigger (or omit `solid` and set `wall`)",
            solid.name
        )));
    }
    let shape = crate::csg::manifold_to_part(&carved, name, solid.material.clone());
    Ok(Solved {
        cutters,
        room_parts,
        solid,
        shape,
    })
}

/// Builds the carve: the hidden room parts followed by the sheets (empty
/// ones left out).
pub fn build(
    name: &str,
    def: &CarveDef,
    lookup: &dyn Fn(&str) -> Result<Arc<Part>, ModelerError>,
) -> Result<Vec<Part>, ModelerError> {
    let fail = |m: String| ModelerError::Invalid(format!("carve `{name}`: {m}"));
    let Solved {
        cutters,
        room_parts,
        solid,
        shape,
    } = solve(name, def, lookup)?;

    // Classify every triangle of the result.
    let cut_tris: Vec<(Vec<[Vec3; 3]>, Vec3, Vec3)> = cutters
        .iter()
        .map(|(_, p)| {
            let (lo, hi) = p.bounds();
            (world_tris(p), lo, hi)
        })
        .collect();
    let (slo, shi) = solid.bounds();
    let eps = ((shi - slo).max_element() * 1e-4).clamp(1e-4, 5e-3);
    let mut inner: Vec<(usize, Tri)> = Vec::new();
    let mut outer: Vec<Tri> = Vec::new();
    for t in shape.indices.as_chunks::<3>().0 {
        let p = [
            Vec3::from(shape.verts[t[0] as usize]),
            Vec3::from(shape.verts[t[1] as usize]),
            Vec3::from(shape.verts[t[2] as usize]),
        ];
        let c = (p[0] + p[1] + p[2]) / 3.0;
        let cr = (p[1] - p[0]).cross(p[2] - p[0]);
        if cr.length() < 1e-12 {
            continue;
        }
        let n = cr.normalize();
        let probe = c + n * eps;
        let owner = cut_tris
            .iter()
            .position(|(tr, lo, hi)| inside(tr, *lo, *hi, probe));
        match owner {
            Some(i) => inner.push((i, Tri { p, n })),
            None => outer.push(Tri { p, n }),
        }
    }

    if keep_inner(def) && inner.is_empty() {
        return Err(fail(format!(
            "no cutter reaches inside `{}`: nothing was carved",
            solid.name
        )));
    }
    let merge = def.merge.unwrap_or(true);
    let by_cutter = split_cutter(def);
    let by_facing = split_facing(def);
    let mut sheets: Vec<(String, Option<usize>, Sheet)> = Vec::new();
    let mut sheet_of = |key: String, cutter: Option<usize>| -> usize {
        match sheets.iter().position(|(k, _, _)| *k == key) {
            Some(i) => i,
            None => {
                sheets.push((key, cutter, Sheet::default()));
                sheets.len() - 1
            }
        }
    };

    // Collect plane groups (welded), per destination sheet.
    struct Plane {
        sheet: usize,
        pos: Vec<Vec3>,
        ids: HashMap<(i64, i64, i64), u32>,
        group: Group,
    }
    let mut planes: HashMap<PlaneKey, Plane> = HashMap::new();
    let mut order: Vec<PlaneKey> = Vec::new();
    let mut add = |sheet: usize, key: PlaneKey, t: &Tri| {
        let pl = planes.entry(key).or_insert_with(|| {
            order.push(key);
            Plane {
                sheet,
                pos: Vec::new(),
                ids: HashMap::new(),
                group: Group::default(),
            }
        });
        let mut ids = [0u32; 3];
        for k in 0..3 {
            let wk = weld_key(t.p[k]);
            ids[k] = *pl.ids.entry(wk).or_insert_with(|| {
                pl.pos.push(t.p[k]);
                (pl.pos.len() - 1) as u32
            });
        }
        pl.group.tris.push(ids);
        pl.group.n += t.n * ((t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).length());
    };
    let mut serial = 0usize;
    if keep_inner(def) {
        for (ci, t) in &inner {
            let facing = facing_of(t.n);
            let key = sheet_name(
                name,
                by_cutter.then(|| cutters[*ci].0.as_str()),
                by_facing.then_some(FACINGS[facing]),
            );
            let s = sheet_of(key, by_cutter.then_some(*ci));
            let pk = if merge {
                plane_key(*ci, 0, t.n, t.p[0])
            } else {
                serial += 1;
                (serial, 9, (0, 0, 0), 0)
            };
            add(s, pk, t);
        }
    }
    if keep_outer(def) {
        let s = sheet_of(format!("{name}_outer"), None);
        for t in &outer {
            let pk = if merge {
                plane_key(usize::MAX, 1, t.n, t.p[0])
            } else {
                serial += 1;
                (serial, 9, (0, 0, 0), 0)
            };
            add(s, pk, t);
        }
    }

    for key in order {
        let Some(pl) = planes.remove(&key) else {
            continue;
        };
        let n = pl.group.n.try_normalize().unwrap_or(Vec3::Y);
        let merged = if pl.group.tris.len() > 1 {
            merge_group(&pl.pos, &pl.group.tris, n)
        } else {
            None
        };
        let (pos, tris) = merged.unwrap_or((pl.pos, pl.group.tris));
        sheets[pl.sheet].2.polygon(&pos, n, &tris);
    }

    let mut out: Vec<Part> = room_parts;
    let want = sheet_names(name, def);
    sheets.sort_by_key(|(k, _, _)| want.iter().position(|w| w == k).unwrap_or(usize::MAX));
    for (key, cutter, sh) in sheets {
        if sh.indices.is_empty() {
            continue;
        }
        let mut p = Part::new(&key);
        p.verts = sh.verts;
        p.normals = sh.normals;
        p.indices = sh.indices;
        p.sheet = true;
        p.uvs = vec![[0.0; 2]; p.verts.len()];
        p.material = match cutter {
            Some(ci) if !cutters[ci].1.material.is_default() => cutters[ci].1.material.clone(),
            _ => solid.material.clone(),
        };
        crate::uv::project(&mut p, "metric")?;
        out.push(p);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxp(name: &str, lo: [f32; 3], hi: [f32; 3]) -> Arc<Part> {
        let lo = Vec3::from(lo);
        let hi = Vec3::from(hi);
        let mut p =
            crate::primitives::primitive(crate::PrimitiveKind::Box, (hi - lo).to_array(), name)
                .unwrap_or_else(|e| panic!("{e}"));
        p.translation = ((lo + hi) * 0.5).to_array();
        Arc::new(p)
    }

    fn area(p: &Part) -> f32 {
        p.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let v = |i: u32| Vec3::from(p.verts[i as usize]);
                (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0])).length() * 0.5
            })
            .sum()
    }

    #[test]
    fn one_room_is_six_inward_quads() {
        let room = boxp("room", [0.0, 0.0, 0.0], [4.0, 3.0, 5.0]);
        let def = CarveDef {
            cutters: vec!["room".into()],
            ..Default::default()
        };
        let parts = build("b", &def, &|n| {
            if n == "room" {
                Ok(room.clone())
            } else {
                Err(ModelerError::MissingInput(n.into()))
            }
        })
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.len(), 1);
        let p = &parts[0];
        assert!(p.sheet);
        // 2*(4*3 + 4*5 + 3*5) = 94 m2 of surface, 12 triangles once merged.
        assert!((area(p) - 94.0).abs() < 1e-2, "area {}", area(p));
        assert_eq!(p.indices.len() / 3, 12, "merged into whole quads");
        // Every face looks into the room.
        let c = Vec3::new(2.0, 1.5, 2.5);
        for t in p.indices.as_chunks::<3>().0 {
            let v = |i: u32| Vec3::from(p.verts[i as usize]);
            let n = (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0]));
            assert!(n.dot(c - v(t[0])) > 0.0, "face looks away from the room");
        }
    }

    #[test]
    fn a_doorway_joins_two_rooms_and_splits_by_facing() {
        let a = boxp("a", [0.0, 0.0, 0.0], [4.0, 3.0, 4.0]);
        let b = boxp("b", [5.0, 0.0, 0.0], [9.0, 3.0, 4.0]);
        let door = boxp("door", [3.9, 0.0, 1.5], [5.1, 2.1, 2.5]);
        let def = CarveDef {
            cutters: vec!["a".into(), "b".into(), "door".into()],
            split_by: Some("facing".into()),
            ..Default::default()
        };
        let parts = build("h", &def, &|n| match n {
            "a" => Ok(a.clone()),
            "b" => Ok(b.clone()),
            "door" => Ok(door.clone()),
            _ => Err(ModelerError::MissingInput(n.into())),
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["h_floor", "h_wall", "h_ceiling"]);
        // The wall between the rooms keeps a hole: both rooms' walls minus
        // the doorway's 1.0 x 2.1 opening on each side.
        let walls = parts
            .iter()
            .find(|p| p.name == "h_wall")
            .map(area)
            .unwrap_or(0.0);
        assert!((walls - 96.0).abs() < 0.05, "walls {walls}");
        let boxes: Vec<BoxOf> = vec![
            (
                "a".into(),
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(4.0, 3.0, 4.0),
            ),
            (
                "b".into(),
                Vec3::new(5.0, 0.0, 0.0),
                Vec3::new(9.0, 3.0, 4.0),
            ),
            (
                "door".into(),
                Vec3::new(3.9, 0.0, 1.5),
                Vec3::new(5.1, 2.1, 2.5),
            ),
        ];
        let an = analyze(&boxes, None);
        assert_eq!(an.rooms, vec![0, 1]);
        assert_eq!(an.openings.len(), 1);
        assert_eq!(an.openings[0].0, 2);
        assert_eq!(an.openings[0].1, vec![0, 1]);
    }

    #[test]
    fn rooms_by_footprint_and_split_by_cutter() {
        let def = CarveDef {
            rooms: vec![RoomDef {
                name: "lobby".into(),
                footprint: vec![
                    [0.0, 0.0],
                    [6.0, 0.0],
                    [6.0, 2.0],
                    [2.0, 2.0],
                    [2.0, 5.0],
                    [0.0, 5.0],
                ],
                height: 3.0,
                base: 0.0,
            }],
            split_by: Some("cutter".into()),
            ..Default::default()
        };
        let parts = build("l", &def, &|n| Err(ModelerError::MissingInput(n.into())))
            .unwrap_or_else(|e| panic!("{e}"));
        let lobby = parts
            .iter()
            .find(|p| p.name == "l_lobby")
            .unwrap_or_else(|| panic!("no sheet"));
        // L shape area 6*2 + 2*3 = 18; floor + ceiling 36; walls = perimeter 22 * 3.
        assert!(
            (area(lobby) - (36.0 + 66.0)).abs() < 0.05,
            "area {}",
            area(lobby)
        );
        assert!(parts.iter().any(|p| p.name == "lobby" && !p.visible));
    }

    #[test]
    fn keep_both_adds_the_outer_skin() {
        let room = boxp("room", [0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        let def = CarveDef {
            cutters: vec!["room".into()],
            keep: Some("both".into()),
            wall: Some(0.5),
            ..Default::default()
        };
        let parts = build("b", &def, &|_| Ok(room.clone())).unwrap_or_else(|e| panic!("{e}"));
        let outer = parts
            .iter()
            .find(|p| p.name == "b_outer")
            .unwrap_or_else(|| panic!("no outer"));
        assert!(
            (area(outer) - 6.0 * 9.0).abs() < 1e-2,
            "area {}",
            area(outer)
        );
    }

    #[test]
    fn a_cutter_through_the_outer_wall_is_an_outside_opening() {
        let room = boxp("room", [0.0, 0.0, 0.0], [4.0, 3.0, 4.0]);
        let exit = boxp("exit", [3.9, 0.0, 1.0], [5.5, 2.1, 2.0]);
        let solid = boxp("mass", [-0.3, 0.0, -0.3], [4.3, 3.3, 4.3]);
        let def = CarveDef {
            solid: Some("mass".into()),
            cutters: vec!["room".into(), "exit".into()],
            ..Default::default()
        };
        let parts = build("e", &def, &|n| match n {
            "room" => Ok(room.clone()),
            "exit" => Ok(exit.clone()),
            _ => Ok(solid.clone()),
        })
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.len(), 1);
        let boxes: Vec<BoxOf> = vec![
            ("room".into(), Vec3::ZERO, Vec3::new(4.0, 3.0, 4.0)),
            (
                "exit".into(),
                Vec3::new(3.9, 0.0, 1.0),
                Vec3::new(5.5, 2.1, 2.0),
            ),
        ];
        let m = markers("e", &boxes, Some(solid.bounds()));
        let kinds: Vec<&str> = m.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, ["room", "opening"]);
        assert_eq!(m[1].data["outside"], true);
    }

    #[test]
    fn errors_say_what_is_wrong() {
        let room = boxp("room", [0.0, 0.0, 0.0], [4.0, 3.0, 4.0]);
        let far = boxp("far", [20.0, 0.0, 0.0], [24.0, 3.0, 4.0]);
        let big = boxp("big", [-5.0, -5.0, -5.0], [9.0, 9.0, 9.0]);
        let def = CarveDef {
            solid: Some("far".into()),
            cutters: vec!["room".into()],
            ..Default::default()
        };
        let look = |n: &str| match n {
            "room" => Ok(room.clone()),
            "far" => Ok(far.clone()),
            "big" => Ok(big.clone()),
            _ => Err(ModelerError::MissingInput(n.into())),
        };
        let e = build("x", &def, &look)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("nothing was carved"), "{e}");
        let def = CarveDef {
            solid: Some("room".into()),
            cutters: vec!["big".into()],
            ..Default::default()
        };
        let e = build("x", &def, &look)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("nothing is left"), "{e}");
    }

    #[test]
    fn blocks_cover_the_walls_floor_and_ceiling() {
        let room = boxp("room", [0.0, 0.0, 0.0], [4.0, 3.0, 5.0]);
        let def = CarveDef {
            cutters: vec!["room".into()],
            wall: Some(0.5),
            ..Default::default()
        };
        let b = blocks("b", &def, &|_| Ok(room.clone())).unwrap_or_else(|e| panic!("{e}"));
        // Mass 5 x 4 x 6 minus the 4 x 3 x 5 room.
        let vol: f32 = b.iter().map(|(_, s)| s.x * s.y * s.z).sum();
        assert!(
            (vol - (5.0 * 4.0 * 6.0 - 60.0)).abs() < 1e-2,
            "volume {vol}"
        );
        assert!(b.len() <= 8, "{} boxes", b.len());
        // No box reaches into the room.
        for (c, s) in &b {
            let (lo, hi) = (*c - *s * 0.5, *c + *s * 0.5);
            let inside_room = (0..3).all(|k| lo[k] < [4.0, 3.0, 5.0][k] - 1e-3 && hi[k] > 1e-3);
            assert!(!inside_room, "box {c} {s} is inside the room");
        }
    }

    #[test]
    fn a_slab_is_a_floor_above_and_a_ceiling_below() {
        let room = boxp("room", [0.0, 0.0, 0.0], [10.0, 6.0, 10.0]);
        let def = CarveDef {
            cutters: vec!["room".into()],
            // A 10 x 3 balcony along the north wall, 3 m up.
            slabs: vec![SlabDef {
                footprint: vec![vec![[0.0, 0.0], [10.0, 0.0], [10.0, 3.0], [0.0, 3.0]]],
                top: 3.0,
                thickness: Some(0.4),
            }],
            split_by: Some("facing".into()),
            ..Default::default()
        };
        let parts = build("b", &def, &|_| Ok(room.clone())).unwrap_or_else(|e| panic!("{e}"));
        let get = |n: &str| {
            parts
                .iter()
                .find(|p| p.name == n)
                .unwrap_or_else(|| panic!("no {n}"))
        };
        // Floor: the whole room floor (still exposed under the slab) plus the slab's top.
        assert!(
            (area(get("b_floor")) - (100.0 + 30.0)).abs() < 0.05,
            "floor {}",
            area(get("b_floor"))
        );
        // Ceiling: the room's ceiling plus the slab's underside, the latter at y = 2.6.
        assert!(
            (area(get("b_ceiling")) - (100.0 + 30.0)).abs() < 0.05,
            "ceiling {}",
            area(get("b_ceiling"))
        );
        let top = get("b_floor")
            .verts
            .iter()
            .map(|v| v[1])
            .fold(f32::MIN, f32::max);
        assert!((top - 3.0).abs() < 1e-3, "slab top {top}");
    }
}
