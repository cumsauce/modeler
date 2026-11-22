//! Walkability: can a character of a given size get from one point to
//! another over the model? Backs `verify` `walkable`.
//!
//! Recast-style: the visible parts are rasterized into vertical columns on
//! an xz grid; each column's surface hits give its solid intervals and the
//! floors on top of them (plus the ground at y = 0). A floor is standable
//! when its slope is under `max_slope` and nothing solid sits between
//! `step` and `height` above it within `radius` (walls, low ceilings).
//! Neighbouring standable floors connect when their heights differ by at
//! most `step` (a stair riser). A flood fill from `from` then says whether
//! `to` is reached, and if not, the blocker nearest the goal: a riser too
//! tall, headroom too low, a gap too narrow, a slope too steep.

use crate::{Model, Part, Vec3};
use rayon::prelude::*;
use std::collections::VecDeque;

/// The character: a capsule-ish cylinder that can climb steps.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// Meters; the character needs a 2 x radius wide gap. Default 0.3.
    #[serde(default = "d_radius")]
    pub radius: f32,
    /// Meters of headroom it needs. Default 1.8.
    #[serde(default = "d_height")]
    pub height: f32,
    /// The tallest step (stair riser, kerb) it climbs, meters. Default 0.3.
    #[serde(default = "d_step")]
    pub step: f32,
    /// The steepest slope it walks, degrees. Default 45.
    #[serde(default = "d_slope")]
    pub max_slope: f32,
}

fn d_radius() -> f32 {
    0.3
}
fn d_height() -> f32 {
    1.8
}
fn d_step() -> f32 {
    0.3
}
fn d_slope() -> f32 {
    45.0
}

impl Default for Agent {
    fn default() -> Self {
        Agent {
            radius: d_radius(),
            height: d_height(),
            step: d_step(),
            max_slope: d_slope(),
        }
    }
}

/// One route to check.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalkSpec {
    /// Start: the character's feet (snapped to the nearest floor below
    /// about a step above it).
    pub from: [f32; 3],
    /// Goal, same as `from`.
    pub to: [f32; 3],
    /// Waypoints the route must pass through, in order.
    #[serde(default)]
    pub via: Vec<[f32; 3]>,
    /// The character (default a human player: radius 0.3, height 1.8,
    /// step 0.3, max_slope 45).
    #[serde(default)]
    pub agent: Option<Agent>,
    /// Grid cell in meters (default a quarter of the radius, coarser for
    /// big scenes).
    #[serde(default)]
    pub cell: Option<f32>,
}

/// Which markers a `markers` check walks between.
#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkerSel {
    /// Marker kinds to include (default: every kind except block, room,
    /// opening: spawns, waypoints, pickups, spots...).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Only markers whose name matches one of these (`*` globs).
    #[serde(default)]
    pub names: Vec<String>,
    /// The marker everything must be reachable from (default: the first).
    #[serde(default)]
    pub from: Option<String>,
    /// The character (default a human player).
    #[serde(default)]
    pub agent: Option<Agent>,
    /// Grid cell in meters.
    #[serde(default)]
    pub cell: Option<f32>,
}

/// `{"markers": {...}}`: every selected marker is reachable from one.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkerWalk {
    pub markers: MarkerSel,
}

/// `walkable: {...}` (one route), a list of routes, or `{"markers": {...}}`
/// (every spawn / waypoint / pickup reachable from the first).
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum WalkableSpec {
    Markers(MarkerWalk),
    One(WalkSpec),
    Many(Vec<WalkSpec>),
}

impl WalkableSpec {
    pub fn routes(&self) -> Vec<&WalkSpec> {
        match self {
            WalkableSpec::One(w) => vec![w],
            WalkableSpec::Many(v) => v.iter().collect(),
            WalkableSpec::Markers(_) => Vec::new(),
        }
    }
    pub fn markers(&self) -> Option<&MarkerSel> {
        match self {
            WalkableSpec::Markers(m) => Some(&m.markers),
            _ => None,
        }
    }
}

const GROUND: u32 = u32::MAX;
const MAX_COLUMNS: usize = 4_000_000;

#[derive(Clone, Copy)]
struct Hit {
    y: f32,
    ny: f32,
    part: u32,
}

/// A solid run of a column, and the part it belongs to.
#[derive(Clone, Copy)]
struct Interval {
    lo: f32,
    hi: f32,
    part: u32,
}

#[derive(Clone, Copy)]
struct Floor {
    y: f32,
    ny: f32,
    part: u32,
}

/// Why a floor can't be stood on.
#[derive(Clone, Copy)]
enum Block {
    Slope(f32),
    Headroom {
        room: f32,
        part: u32,
    },
    Narrow {
        part: u32,
    },
    /// Something within the radius that is taller than a step but shorter
    /// than the character: a riser or kerb too tall to climb.
    Ledge {
        rise: f32,
        part: u32,
    },
}

struct Grid {
    x0: f32,
    z0: f32,
    cell: f32,
    nx: usize,
    nz: usize,
    intervals: Vec<Vec<Interval>>,
    floors: Vec<Vec<Floor>>,
    /// Per floor: None = standable.
    blocks: Vec<Vec<Option<Block>>>,
}

impl Grid {
    fn center(&self, c: usize) -> (f32, f32) {
        let (ix, iz) = (c % self.nx, c / self.nx);
        (
            self.x0 + (ix as f32 + 0.5) * self.cell,
            self.z0 + (iz as f32 + 0.5) * self.cell,
        )
    }
    fn column(&self, x: f32, z: f32) -> Option<usize> {
        let ix = ((x - self.x0) / self.cell).floor();
        let iz = ((z - self.z0) / self.cell).floor();
        if ix < 0.0 || iz < 0.0 || ix as usize >= self.nx || iz as usize >= self.nz {
            return None;
        }
        Some(iz as usize * self.nx + ix as usize)
    }
    fn point(&self, (c, f): (usize, usize)) -> Vec3 {
        let (x, z) = self.center(c);
        Vec3::new(x, self.floors[c][f].y, z)
    }
}

fn visible(model: &Model) -> Vec<&Part> {
    model
        .parts
        .iter()
        .map(|p| &**p)
        .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty())
        .collect()
}

fn build(parts: &[&Part], spec: &WalkSpec, agent: &Agent) -> Result<Grid, String> {
    let tris: Vec<Vec<[Vec3; 3]>> = parts
        .iter()
        .map(|p| crate::contact::world_tris(p))
        .collect();
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for t in tris.iter().flatten().flatten() {
        lo = lo.min(*t);
        hi = hi.max(*t);
    }
    for p in std::iter::once(&spec.from)
        .chain(std::iter::once(&spec.to))
        .chain(spec.via.iter())
    {
        let v = Vec3::from_array(*p);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    // Sinking up to 5 cm below y = 0 (a prop sunk a little) is still standing
    // on the ground plane: only a model that sits wholly lower moves the
    // floor (else every riser grows by the sink, and the gap between the
    // ground and a floor sheet at y = 0 becomes a phantom walkable layer).
    let ground = if lo.y >= -0.05 { 0.0 } else { lo.y };
    let mut cell = spec.cell.unwrap_or(agent.radius / 4.0).max(1e-3);
    let pad = agent.radius * 2.0 + cell * 2.0;
    let (w, d) = (hi.x - lo.x + 2.0 * pad, hi.z - lo.z + 2.0 * pad);
    if spec.cell.is_none() {
        cell = cell.max((w * d / MAX_COLUMNS as f32).sqrt());
    }
    let nx = (w / cell).ceil().max(1.0) as usize;
    let nz = (d / cell).ceil().max(1.0) as usize;
    if nx * nz > MAX_COLUMNS {
        return Err(format!(
            "{nx} x {nz} cells is over the {MAX_COLUMNS} limit: give a coarser `cell` (about {:.3})",
            (w * d / MAX_COLUMNS as f32).sqrt()
        ));
    }
    let (x0, z0) = (lo.x - pad, lo.z - pad);
    let n = nx * nz;
    let mut hits: Vec<Vec<Hit>> = vec![Vec::new(); n];
    for (pi, ts) in tris.iter().enumerate() {
        for t in ts {
            let nrm = (t[1] - t[0]).cross(t[2] - t[0]);
            let len = nrm.length();
            if len < 1e-12 {
                continue;
            }
            let ny = nrm.y / len;
            if ny.abs() < 0.02 {
                continue;
            }
            let (tx0, tx1) = (
                t[0].x.min(t[1].x).min(t[2].x),
                t[0].x.max(t[1].x).max(t[2].x),
            );
            let (tz0, tz1) = (
                t[0].z.min(t[1].z).min(t[2].z),
                t[0].z.max(t[1].z).max(t[2].z),
            );
            let ix0 = (((tx0 - x0) / cell) - 0.5).ceil().max(0.0) as usize;
            let ix1 = ((((tx1 - x0) / cell) - 0.5).floor() as isize).min(nx as isize - 1);
            let iz0 = (((tz0 - z0) / cell) - 0.5).ceil().max(0.0) as usize;
            let iz1 = ((((tz1 - z0) / cell) - 0.5).floor() as isize).min(nz as isize - 1);
            if ix1 < 0 || iz1 < 0 {
                continue;
            }
            let (a, b, c) = (t[0], t[1], t[2]);
            let den = (b.z - c.z) * (a.x - c.x) + (c.x - b.x) * (a.z - c.z);
            if den.abs() < 1e-14 {
                continue;
            }
            for iz in iz0..=iz1 as usize {
                let z = z0 + (iz as f32 + 0.5) * cell;
                for ix in ix0..=ix1 as usize {
                    let x = x0 + (ix as f32 + 0.5) * cell;
                    let l1 = ((b.z - c.z) * (x - c.x) + (c.x - b.x) * (z - c.z)) / den;
                    let l2 = ((c.z - a.z) * (x - c.x) + (a.x - c.x) * (z - c.z)) / den;
                    let l3 = 1.0 - l1 - l2;
                    let e = -1e-6;
                    if l1 < e || l2 < e || l3 < e {
                        continue;
                    }
                    let y = l1 * a.y + l2 * b.y + l3 * c.y;
                    hits[iz * nx + ix].push(Hit {
                        y,
                        ny,
                        part: pi as u32,
                    });
                }
            }
        }
    }
    let cols: Vec<(Vec<Interval>, Vec<Floor>)> = hits
        .into_par_iter()
        .map(|mut hs| {
            hs.sort_by(|a, b| a.y.total_cmp(&b.y));
            hs.dedup_by(|b, a| {
                (a.y - b.y).abs() < 1e-5 && (a.ny > 0.0) == (b.ny > 0.0) && a.part == b.part
            });
            // Below the ground is solid: start inside it.
            let mut depth = 1u32;
            let mut start = (f32::NEG_INFINITY, GROUND);
            let mut intervals = Vec::new();
            let mut floors: Vec<Floor> = vec![];
            let ground_hit = Hit {
                y: ground,
                ny: 1.0,
                part: GROUND,
            };
            for h in std::iter::once(ground_hit).chain(hs) {
                if h.ny < 0.0 {
                    if depth == 0 {
                        start = (h.y, h.part);
                    }
                    depth += 1;
                } else if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        let part = if start.1 == GROUND { h.part } else { start.1 };
                        intervals.push(Interval {
                            lo: start.0,
                            hi: h.y,
                            part,
                        });
                        floors.push(Floor {
                            y: h.y,
                            ny: h.ny,
                            part: h.part,
                        });
                    }
                }
            }
            if depth > 0 {
                // An open sheet or a broken mesh left us inside: close at the top.
                intervals.push(Interval {
                    lo: start.0,
                    hi: f32::INFINITY,
                    part: start.1,
                });
            }
            // A floor with solid right on top of it is buried (the ground
            // under a box), not a floor.
            floors.retain(|f: &Floor| {
                !intervals
                    .iter()
                    .any(|iv| iv.lo >= f.y - 1e-4 && iv.lo < f.y + 0.02)
            });
            (intervals, floors)
        })
        .collect();
    let (intervals, floors): (Vec<_>, Vec<_>) = cols.into_iter().unzip();
    let mut g = Grid {
        x0,
        z0,
        cell,
        nx,
        nz,
        intervals,
        floors,
        blocks: Vec::new(),
    };
    // The disk of columns within the radius (offsets and distance).
    let r = (agent.radius / cell).ceil() as isize;
    let mut disk = Vec::new();
    for dz in -r..=r {
        for dx in -r..=r {
            let dist = ((dx * dx + dz * dz) as f32).sqrt() * cell;
            if dist <= agent.radius + 1e-6 {
                disk.push((dx, dz, dist));
            }
        }
    }
    let cos_max = agent.max_slope.to_radians().cos();
    let tan_max = agent.max_slope.clamp(0.0, 80.0).to_radians().tan();
    let blocks: Vec<Vec<Option<Block>>> = (0..n)
        .into_par_iter()
        .map(|c| {
            let (ix, iz) = ((c % nx) as isize, (c / nx) as isize);
            g.floors[c]
                .iter()
                .map(|f| {
                    if f.ny < cos_max - 1e-4 {
                        return Some(Block::Slope(f.ny.clamp(-1.0, 1.0).acos().to_degrees()));
                    }
                    let (lo, hi) = (f.y + agent.step, f.y + agent.height);
                    let mut worst: Option<Block> = None;
                    for &(dx, dz, dist) in &disk {
                        let (jx, jz) = (ix + dx, iz + dz);
                        if jx < 0 || jz < 0 || jx >= nx as isize || jz >= nz as isize {
                            continue;
                        }
                        // Ground rising no steeper than a walkable slope
                        // (the next stair treads) is not a wall.
                        let lo_d = lo + dist * tan_max;
                        for iv in &g.intervals[jz as usize * nx + jx as usize] {
                            if iv.hi > lo_d && iv.lo < hi {
                                if dist <= cell * 0.75 && iv.lo > lo {
                                    let room = iv.lo - f.y;
                                    match worst {
                                        Some(Block::Headroom { room: r, .. }) if r <= room => {}
                                        _ => {
                                            worst = Some(Block::Headroom {
                                                room,
                                                part: iv.part,
                                            })
                                        }
                                    }
                                } else if iv.hi < hi
                                    && !matches!(
                                        worst,
                                        Some(Block::Headroom { .. } | Block::Narrow { .. })
                                    )
                                {
                                    // Shorter than the character: a ledge it can't step onto.
                                    let rise = iv.hi - f.y;
                                    match worst {
                                        Some(Block::Ledge { rise: r, .. }) if r <= rise => {}
                                        _ => {
                                            worst = Some(Block::Ledge {
                                                rise,
                                                part: iv.part,
                                            })
                                        }
                                    }
                                } else if iv.hi >= hi
                                    && !matches!(worst, Some(Block::Headroom { .. }))
                                {
                                    worst = Some(Block::Narrow { part: iv.part });
                                }
                            }
                        }
                    }
                    worst
                })
                .collect()
        })
        .collect();
    g.blocks = blocks;
    Ok(g)
}

/// The floor in column `c` a point at height `y` stands on: the highest
/// one no more than a step above `y`, else the nearest.
fn snap(g: &Grid, p: [f32; 3], step: f32) -> Option<(usize, usize)> {
    let c = g.column(p[0], p[2])?;
    let fs = &g.floors[c];
    let below = fs
        .iter()
        .enumerate()
        .filter(|(_, f)| f.y <= p[1] + step)
        .max_by(|a, b| a.1.y.total_cmp(&b.1.y));
    let pick = below.or_else(|| {
        fs.iter()
            .enumerate()
            .min_by(|a, b| (a.1.y - p[1]).abs().total_cmp(&(b.1.y - p[1]).abs()))
    })?;
    Some((c, pick.0))
}

fn part_name(parts: &[&Part], i: u32) -> String {
    if i == GROUND {
        "the ground".to_string()
    } else {
        format!("`{}`", parts[i as usize].name)
    }
}

fn describe_block(parts: &[&Part], b: Block, agent: &Agent) -> String {
    match b {
        Block::Slope(deg) => format!("slope {deg:.0}° > {:.0}°", agent.max_slope),
        Block::Headroom { room, part } => format!(
            "headroom {room:.2} m < {:.2} under {}",
            agent.height,
            part_name(parts, part)
        ),
        Block::Ledge { rise, part } => format!(
            "step {rise:.2} m > {:.2} up onto {} just ahead",
            agent.step,
            part_name(parts, part)
        ),
        Block::Narrow { part } => format!(
            "{} in the way (within the {:.2} m radius: a wall, or a gap under {:.2} m wide)",
            part_name(parts, part),
            agent.radius,
            agent.radius * 2.0
        ),
    }
}

fn fmt_p(p: Vec3) -> String {
    format!("[{:.2}, {:.2}, {:.2}]", p.x, p.y, p.z)
}

struct Leg {
    path: Vec<(usize, usize)>,
}

/// Flood fill from `a` to `b`; on failure the blocker nearest `b`.
fn leg(
    g: &Grid,
    parts: &[&Part],
    agent: &Agent,
    a: (usize, usize),
    b: (usize, usize),
) -> Result<Leg, String> {
    let n = g.nx * g.nz;
    let mut offs = vec![0usize; n + 1];
    for c in 0..n {
        offs[c + 1] = offs[c] + g.floors[c].len();
    }
    let id = |(c, f): (usize, usize)| offs[c] + f;
    let mut parent = vec![usize::MAX; offs[n]];
    let mut node_of = vec![(0usize, 0usize); offs[n]];
    for c in 0..n {
        for f in 0..g.floors[c].len() {
            node_of[offs[c] + f] = (c, f);
        }
    }
    let goal = g.point(b);
    let ia = id(a);
    parent[ia] = ia;
    let mut q = VecDeque::from([a]);
    // Frontier failures by cause (the message without its location):
    // the spot nearest the goal for each.
    let mut fails: std::collections::HashMap<String, (f32, String)> = Default::default();
    let mut closest = (g.point(a).distance(goal), g.point(a));
    let mut note = |d: f32, msg: String| {
        let key = msg.split(" at [").next().unwrap_or(&msg).to_string();
        let e = fails.entry(key).or_insert((f32::INFINITY, String::new()));
        if d < e.0 {
            *e = (d, msg);
        }
    };
    while let Some(cur) = q.pop_front() {
        if cur == b {
            let mut path = vec![cur];
            let mut i = id(cur);
            while parent[i] != i {
                i = parent[i];
                path.push(node_of[i]);
            }
            path.reverse();
            return Ok(Leg { path });
        }
        let (c, f) = cur;
        let fy = g.floors[c][f].y;
        let here = g.point(cur);
        let dg = here.distance(goal);
        if dg < closest.0 {
            closest = (dg, here);
        }
        let (ix, iz) = ((c % g.nx) as isize, (c / g.nx) as isize);
        for (dx, dz) in [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let (jx, jz) = (ix + dx, iz + dz);
            if jx < 0 || jz < 0 || jx >= g.nx as isize || jz >= g.nz as isize {
                continue;
            }
            let nc = jz as usize * g.nx + jx as usize;
            let Some((nf, fl)) = g.floors[nc]
                .iter()
                .enumerate()
                .min_by(|x, y| (x.1.y - fy).abs().total_cmp(&(y.1.y - fy).abs()))
            else {
                continue;
            };
            let nid = id((nc, nf));
            if parent[nid] != usize::MAX {
                continue;
            }
            let there = g.point((nc, nf));
            let dy = fl.y - fy;
            if dy.abs() > agent.step + 1e-4 {
                // Only a climb is a blocker; a drop is a ledge, not a wall.
                if dy > 0.0 && dy < agent.height {
                    note(
                        there.distance(goal),
                        format!(
                            "step {:.2} m > {:.2} from {} up onto {} at {}",
                            dy,
                            agent.step,
                            part_name(parts, g.floors[c][f].part),
                            part_name(parts, fl.part),
                            fmt_p(there)
                        ),
                    );
                }
                continue;
            }
            if let Some(blk) = g.blocks[nc][nf] {
                note(
                    there.distance(goal),
                    format!("{} at {}", describe_block(parts, blk, agent), fmt_p(there)),
                );
                continue;
            }
            parent[nid] = id(cur);
            q.push_back((nc, nf));
        }
    }
    let mut list: Vec<(f32, String)> = fails.into_values().collect();
    list.sort_by(|a, b| a.0.total_cmp(&b.0));
    let why = if list.is_empty() {
        "no floor connects them".to_string()
    } else {
        let more = list.len().saturating_sub(4);
        let mut w: Vec<String> = list.into_iter().take(4).map(|(_, m)| m).collect();
        if more > 0 {
            w.push(format!("{more} more"));
        }
        w.join("; ")
    };
    Err(format!(
        "{} (closest reached {} is {:.2} m from the goal)",
        why,
        fmt_p(closest.1),
        closest.0
    ))
}

/// Every floor reachable from `a` (indexed like `offs`), by the same rules
/// as [`leg`].
fn flood(g: &Grid, agent: &Agent, a: (usize, usize)) -> (Vec<usize>, Vec<bool>) {
    let n = g.nx * g.nz;
    let mut offs = vec![0usize; n + 1];
    for c in 0..n {
        offs[c + 1] = offs[c] + g.floors[c].len();
    }
    let mut seen = vec![false; offs[n]];
    seen[offs[a.0] + a.1] = true;
    let mut q = VecDeque::from([a]);
    while let Some((c, f)) = q.pop_front() {
        let fy = g.floors[c][f].y;
        let (ix, iz) = ((c % g.nx) as isize, (c / g.nx) as isize);
        for (dx, dz) in [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let (jx, jz) = (ix + dx, iz + dz);
            if jx < 0 || jz < 0 || jx >= g.nx as isize || jz >= g.nz as isize {
                continue;
            }
            let nc = jz as usize * g.nx + jx as usize;
            let Some((nf, fl)) = g.floors[nc]
                .iter()
                .enumerate()
                .min_by(|x, y| (x.1.y - fy).abs().total_cmp(&(y.1.y - fy).abs()))
            else {
                continue;
            };
            let nid = offs[nc] + nf;
            if seen[nid] || (fl.y - fy).abs() > agent.step + 1e-4 || g.blocks[nc][nf].is_some() {
                continue;
            }
            seen[nid] = true;
            q.push_back((nc, nf));
        }
    }
    (offs, seen)
}

/// A top-down picture of what a walker reaches: `+` reached, `.` standable
/// but not reached, blank nothing; `marks` (x, z, char) drawn on top.
fn reach_map(g: &Grid, offs: &[usize], seen: &[bool], marks: &[(f32, f32, char)]) -> String {
    let kx = g.nx.div_ceil(80).max(1);
    let kz = kx * 2;
    let (cols, rows) = (g.nx.div_ceil(kx), g.nz.div_ceil(kz));
    let mut grid = vec![vec![' '; cols]; rows];
    for c in 0..g.nx * g.nz {
        let (ix, iz) = (c % g.nx, c / g.nx);
        let cell = &mut grid[iz / kz][ix / kx];
        for f in 0..g.floors[c].len() {
            if g.blocks[c][f].is_some() {
                continue;
            }
            if seen[offs[c] + f] {
                *cell = '+';
            } else if *cell != '+' {
                *cell = '.';
            }
        }
    }
    for &(x, z, ch) in marks {
        if let Some(c) = g.column(x, z) {
            grid[(c / g.nx) / kz][(c % g.nx) / kx] = ch;
        }
    }
    let mut out = format!(
        "map (top view, +x right, -z up; '+' reached, '.' standable but not reached; 1 char = {:.1} x {:.1} m, x from {:.1}, z from {:.1}):\n",
        kx as f32 * g.cell,
        kz as f32 * g.cell,
        g.x0,
        g.z0
    );
    for row in grid {
        out += row.into_iter().collect::<String>().trim_end();
        out.push('\n');
    }
    out
}

/// `markers`: every selected marker standable and reachable from the first.
pub fn check_markers(model: &Model, sel: &MarkerSel) -> Result<String, String> {
    let agent = sel.agent.clone().unwrap_or_default();
    if agent.radius <= 0.0 || agent.height <= 0.0 || agent.step < 0.0 {
        return Err("agent: radius and height must be > 0, step >= 0".into());
    }
    let all = crate::game::markers(model).map_err(|e| e.to_string())?;
    let picked: Vec<_> = all
        .into_iter()
        .filter(|m| {
            let kind_ok = if sel.kinds.is_empty() {
                !["block", "room", "opening"].contains(&m.kind.as_str())
            } else {
                sel.kinds.contains(&m.kind)
            };
            kind_ok
                && (sel.names.is_empty()
                    || sel.names.iter().any(|p| crate::name_matches(p, &m.name)))
        })
        .collect();
    if picked.is_empty() {
        return Err("no markers selected (kinds / names match nothing)".into());
    }
    let start = match &sel.from {
        Some(n) => picked
            .iter()
            .position(|m| &m.name == n)
            .ok_or_else(|| format!("from: no selected marker named `{n}`"))?,
        None => 0,
    };
    let parts = visible(model);
    let spec = WalkSpec {
        from: picked[start].position.to_array(),
        to: picked[start].position.to_array(),
        via: picked.iter().map(|m| m.position.to_array()).collect(),
        agent: None,
        cell: sel.cell,
    };
    let g = build(&parts, &spec, &agent)?;
    let mut nodes: Vec<Option<(usize, usize)>> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    for m in &picked {
        let p = m.position.to_array();
        let Some(nd) = snap(&g, p, agent.step) else {
            bad.push(format!("`{}` {:?} is outside the grid", m.name, p));
            nodes.push(None);
            continue;
        };
        let fp = g.point(nd);
        if let Some(b) = g.blocks[nd.0][nd.1] {
            bad.push(format!(
                "`{}` {} is not standable: {}",
                m.name,
                fmt_p(fp),
                describe_block(&parts, b, &agent)
            ));
            nodes.push(None);
            continue;
        }
        if (fp.y - p[1]).abs() > 0.5 {
            bad.push(format!(
                "`{}` is at y {:.2} but the nearest floor there is y {:.2} (floating or buried)",
                m.name, p[1], fp.y
            ));
        }
        nodes.push(Some(nd));
    }
    let Some(sn) = nodes[start] else {
        return Err(format!(
            "start `{}` can't be stood on: {}",
            picked[start].name,
            bad.join("; ")
        ));
    };
    let (offs, seen) = flood(&g, &agent, sn);
    let mut marks = Vec::new();
    for (m, nd) in picked.iter().zip(&nodes) {
        let Some((c, f)) = *nd else {
            marks.push((m.position.x, m.position.z, 'X'));
            continue;
        };
        if seen[offs[c] + f] {
            marks.push((m.position.x, m.position.z, 'o'));
        } else {
            bad.push(format!(
                "`{}` {} is not reachable from `{}`",
                m.name,
                fmt_p(g.point((c, f))),
                picked[start].name
            ));
            marks.push((m.position.x, m.position.z, 'X'));
        }
    }
    if bad.is_empty() {
        return Ok(format!(
            "all {} markers standable and reachable from `{}` (radius {:.2} / height {:.2} / step {:.2}, cell {:.3})",
            picked.len(),
            picked[start].name,
            agent.radius,
            agent.height,
            agent.step,
            g.cell
        ));
    }
    let shown: Vec<&String> = bad.iter().take(12).collect();
    let more = bad.len().saturating_sub(shown.len());
    Err(format!(
        "{} of {} markers fail:\n  {}{}\n{}(o reachable marker, X failing marker)",
        bad.len(),
        picked.len(),
        shown
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
        if more > 0 {
            format!("\n  ... {more} more")
        } else {
            String::new()
        },
        reach_map(&g, &offs, &seen, &marks)
    ))
}

/// One route's check: Ok(summary) or Err(why it is blocked).
pub fn check(model: &Model, spec: &WalkSpec) -> Result<String, String> {
    let agent = spec.agent.clone().unwrap_or_default();
    if agent.radius <= 0.0 || agent.height <= 0.0 || agent.step < 0.0 {
        return Err("agent: radius and height must be > 0, step >= 0".into());
    }
    let parts = visible(model);
    let g = build(&parts, spec, &agent)?;
    let points: Vec<[f32; 3]> = std::iter::once(spec.from)
        .chain(spec.via.iter().copied())
        .chain(std::iter::once(spec.to))
        .collect();
    let mut nodes = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let label = if i == 0 {
            "from".to_string()
        } else if i == points.len() - 1 {
            "to".to_string()
        } else {
            format!("via {}", i - 1)
        };
        let Some(nd) = snap(&g, *p, agent.step) else {
            return Err(format!("{label} {:?} is outside the grid", p));
        };
        if let Some(b) = g.blocks[nd.0][nd.1] {
            return Err(format!(
                "{label} {} is not standable: {}",
                fmt_p(g.point(nd)),
                describe_block(&parts, b, &agent)
            ));
        }
        nodes.push((label, nd));
    }
    let mut path: Vec<(usize, usize)> = Vec::new();
    for w in nodes.windows(2) {
        let l = leg(&g, &parts, &agent, w[0].1, w[1].1).map_err(|e| {
            let (offs, seen) = flood(&g, &agent, w[0].1);
            let a = g.point(w[0].1);
            let b = g.point(w[1].1);
            let snapped = |label: &str, req: [f32; 3], got: Vec3| {
                if (got.y - req[1]).abs() > agent.step + 0.05 {
                    format!(
                        "\nnote: {label} y {:.2} snapped to the floor at y {:.2} ({}): if that is the wrong level, check the point is on the surface you mean",
                        req[1],
                        got.y,
                        fmt_p(got)
                    )
                } else {
                    String::new()
                }
            };
            let from_req = points[nodes.iter().position(|n| n.1 == w[0].1).unwrap_or(0)];
            let to_req = points[nodes.iter().position(|n| n.1 == w[1].1).unwrap_or(0)];
            format!(
                "blocked between {} and {}: {e}{}{}\n{}",
                w[0].0,
                w[1].0,
                snapped(&w[0].0, from_req, a),
                snapped(&w[1].0, to_req, b),
                reach_map(&g, &offs, &seen, &[(a.x, a.z, 'S'), (b.x, b.z, 'G')])
            )
        })?;
        if !path.is_empty() {
            path.pop();
        }
        path.extend(l.path);
    }
    let mut length = 0.0;
    let mut max_step: f32 = 0.0;
    let mut headroom = f32::INFINITY;
    for w in path.windows(2) {
        let (p, q) = (g.point(w[0]), g.point(w[1]));
        length += p.distance(q);
        max_step = max_step.max(q.y - p.y);
    }
    for &(c, f) in &path {
        let y = g.floors[c][f].y;
        if let Some(iv) = g.intervals[c].iter().find(|iv| iv.lo > y + 1e-4) {
            headroom = headroom.min(iv.lo - y);
        }
    }
    let room = if headroom.is_finite() {
        format!(", lowest headroom {headroom:.2} m")
    } else {
        String::new()
    };
    Ok(format!(
        "reachable for radius {:.2} / height {:.2} / step {:.2}: route about {length:.1} m, tallest step {max_step:.2} m{room} (cell {:.3})",
        agent.radius, agent.height, agent.step, g.cell
    ))
}

#[cfg(test)]
mod tests {
    use crate::commands::Session;

    fn session(items: serde_json::Value) -> Session {
        let mut s = Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("array");
        };
        s.apply_values_as(None, items, true)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    fn route(s: &Session, from: [f32; 3], to: [f32; 3]) -> Result<String, String> {
        let spec = super::WalkSpec {
            from,
            to,
            via: vec![],
            agent: None,
            cell: Some(0.05),
        };
        super::check(&s.model, &spec)
    }

    /// A flight of `n` steps of `rise` up +z onto a landing.
    fn stairs(rise: f32, n: usize) -> Session {
        let mut items = Vec::new();
        for i in 0..n {
            let h = rise * (i + 1) as f32;
            items.push(
                serde_json::json!({"type": "primitive", "name": format!("stair_{i}"),
                "kind": "box", "size": [1.2, h, 0.28],
                "translation": [0, h / 2.0, -(i as f32) * 0.28 - 0.14]}),
            );
        }
        let top = rise * n as f32;
        items.push(
            serde_json::json!({"type": "primitive", "name": "landing", "kind": "box",
            "size": [1.2, top, 1.5], "translation": [0, top / 2.0, -(n as f32) * 0.28 - 0.75]}),
        );
        session(serde_json::Value::Array(items))
    }

    #[test]
    fn open_ground_is_walkable() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "post", "kind": "box", "size": [0.2, 1, 0.2], "translation": [0, 0.5, 0]}
        ]));
        let r = route(&s, [-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn gentle_stairs_pass_and_tall_risers_fail() {
        let s = stairs(0.18, 5);
        let top = [0.0, 0.9, -2.2];
        let r = route(&s, [0.0, 0.0, 1.0], top);
        assert!(r.is_ok(), "{r:?}");
        let s = stairs(0.4, 3);
        let r = route(&s, [0.0, 0.0, 1.0], [0.0, 1.2, -1.6]);
        let e = r.err().unwrap_or_default();
        assert!(e.contains("step 0.40"), "{e}");
    }

    #[test]
    fn low_ceiling_fails_headroom() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "slab", "kind": "box", "size": [3, 0.2, 3], "translation": [0, 1.8, 0]},
        ]));
        let r = route(&s, [-3.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        let e = r.err().unwrap_or_default();
        assert!(e.contains("headroom 1.70") && e.contains("`slab`"), "{e}");
    }

    #[test]
    fn narrow_doorway_fails_and_wide_one_passes() {
        // A closed 4 x 6 room split at z = 0 by a wall with a doorway.
        let wall = |gap: f32| {
            let w = (4.0 - gap) / 2.0;
            let off = gap / 2.0 + w / 2.0;
            session(serde_json::json!([
                {"type": "primitive", "name": "wall_l", "kind": "box", "size": [w, 2.5, 0.2], "translation": [-off, 1.25, 0]},
                {"type": "primitive", "name": "wall_r", "kind": "box", "size": [w, 2.5, 0.2], "translation": [off, 1.25, 0]},
                {"type": "primitive", "name": "side_l", "kind": "box", "size": [0.2, 2.5, 6.4], "translation": [-2.1, 1.25, 0]},
                {"type": "primitive", "name": "side_r", "kind": "box", "size": [0.2, 2.5, 6.4], "translation": [2.1, 1.25, 0]},
                {"type": "primitive", "name": "end_f", "kind": "box", "size": [4.4, 2.5, 0.2], "translation": [0, 1.25, 3.1]},
                {"type": "primitive", "name": "end_b", "kind": "box", "size": [4.4, 2.5, 0.2], "translation": [0, 1.25, -3.1]},
            ]))
        };
        let r = route(&wall(0.5), [0.0, 0.0, 2.0], [0.0, 0.0, -2.0]);
        let e = r.err().unwrap_or_default();
        assert!(e.contains("in the way"), "{e}");
        let r = route(&wall(0.9), [0.0, 0.0, 2.0], [0.0, 0.0, -2.0]);
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn a_prop_sunk_a_few_mm_does_not_grow_the_risers() {
        // 0.30 m risers pass; a pebble sunk 1.6 cm must not turn them into
        // 0.32 m ones by moving the ground down.
        let mut items = vec![
            serde_json::json!({"type": "primitive", "name": "pebble", "kind": "box",
            "size": [0.1, 0.1, 0.1], "translation": [3, 0.034, 3]}),
        ];
        for i in 0..3 {
            let h = 0.3 * (i + 1) as f32;
            items.push(
                serde_json::json!({"type": "primitive", "name": format!("st{i}"),
            "kind": "box", "size": [1.2, h, 0.4], "translation": [0, h / 2.0, -(i as f32) * 0.4]}),
            );
        }
        items.push(
            serde_json::json!({"type": "primitive", "name": "top", "kind": "box",
            "size": [1.2, 0.9, 1.5], "translation": [0, 0.45, -1.55]}),
        );
        let s = session(serde_json::Value::Array(items));
        let r = route(&s, [0.0, 0.0, 1.0], [0.0, 0.9, -1.5]);
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn a_blocked_route_draws_where_it_got_to() {
        let s = stairs(0.4, 3);
        let e = route(&s, [0.0, 0.0, 1.0], [0.0, 1.2, -1.6])
            .err()
            .unwrap_or_default();
        assert!(
            e.contains("map (top view") && e.contains('S') && e.contains('G'),
            "{e}"
        );
    }

    #[test]
    fn the_markers_mode_finds_a_walled_off_marker() {
        let s = session(serde_json::json!([
            {"type": "primitive", "name": "w1", "kind": "box", "size": [6, 2.5, 0.2], "translation": [0, 1.25, 2]},
            {"type": "primitive", "name": "w2", "kind": "box", "size": [6, 2.5, 0.2], "translation": [0, 1.25, 4]},
            {"type": "primitive", "name": "w3", "kind": "box", "size": [0.2, 2.5, 2.2], "translation": [-3, 1.25, 3]},
            {"type": "primitive", "name": "w4", "kind": "box", "size": [0.2, 2.5, 2.2], "translation": [3, 1.25, 3]},
            {"type": "marker", "name": "spawn_a", "kind": "spawn", "translation": [0, 0, 0]},
            {"type": "marker", "name": "pick_open", "kind": "arcade", "translation": [-2, 0, 0.5]},
            {"type": "marker", "name": "pick_sealed", "kind": "arcade", "translation": [0, 0, 3]},
        ]));
        let sel = super::MarkerSel::default();
        let e = super::check_markers(&s.model, &sel)
            .err()
            .unwrap_or_default();
        assert!(
            e.contains("pick_sealed") && !e.contains("`pick_open`"),
            "{e}"
        );
        let ok = super::check_markers(
            &s.model,
            &super::MarkerSel {
                names: vec!["spawn_*".into(), "pick_open".into()],
                ..Default::default()
            },
        );
        assert!(ok.is_ok(), "{ok:?}");
    }
}
