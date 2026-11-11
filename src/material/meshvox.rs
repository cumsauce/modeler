//! A part's mesh as an occupancy grid, for mesh-aware material nodes
//! (`mesh_ao`, `mesh_curvature`): built lazily from the object-domain
//! samples' part, inside/outside by ray parity along y per column.

use std::sync::OnceLock;

/// Triangles (object space, meters, scale applied) plus their voxel grid.
#[derive(Debug)]
pub struct MeshVox {
    pub tris: Vec<[[f32; 3]; 3]>,
    grid: OnceLock<Grid>,
}

#[derive(Debug)]
pub struct Grid {
    lo: [f32; 3],
    /// Voxel edge in meters.
    pub cell: f32,
    n: [usize; 3],
    occ: Vec<bool>,
}

/// Voxels along the longest axis.
const RES: f32 = 72.0;

impl MeshVox {
    pub fn new(tris: Vec<[[f32; 3]; 3]>) -> MeshVox {
        MeshVox {
            tris,
            grid: OnceLock::new(),
        }
    }

    pub fn grid(&self) -> &Grid {
        self.grid.get_or_init(|| Grid::build(&self.tris))
    }
}

impl Grid {
    fn build(tris: &[[[f32; 3]; 3]]) -> Grid {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for t in tris {
            for v in t {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
        }
        if tris.is_empty() {
            return Grid {
                lo: [0.0; 3],
                cell: 1.0,
                n: [1, 1, 1],
                occ: vec![false],
            };
        }
        let ext = (0..3)
            .map(|k| hi[k] - lo[k])
            .fold(0.0f32, f32::max)
            .max(1e-6);
        let cell = ext / RES;
        // Two voxels of padding so samples just outside read as empty.
        let lo = lo.map(|x| x - 2.0 * cell);
        let n = [0, 1, 2].map(|k| (((hi[k] - lo[k]) / cell).ceil() as usize + 3).max(1));
        // Bin triangles into the xz columns their footprint covers.
        let mut cols: Vec<Vec<u32>> = vec![Vec::new(); n[0] * n[2]];
        for (ti, t) in tris.iter().enumerate() {
            let x0 = t.iter().map(|v| v[0]).fold(f32::MAX, f32::min);
            let x1 = t.iter().map(|v| v[0]).fold(f32::MIN, f32::max);
            let z0 = t.iter().map(|v| v[2]).fold(f32::MAX, f32::min);
            let z1 = t.iter().map(|v| v[2]).fold(f32::MIN, f32::max);
            let ix = |x: f32| (((x - lo[0]) / cell - 0.5).max(0.0) as usize).min(n[0] - 1);
            let iz = |z: f32| (((z - lo[2]) / cell - 0.5).max(0.0) as usize).min(n[2] - 1);
            let (a0, a1) = (ix(x0), (ix(x1) + 1).min(n[0] - 1));
            let (b0, b1) = (iz(z0), (iz(z1) + 1).min(n[2] - 1));
            for x in a0..=a1 {
                for z in b0..=b1 {
                    cols[x * n[2] + z].push(ti as u32);
                }
            }
        }
        let mut occ = vec![false; n[0] * n[1] * n[2]];
        for x in 0..n[0] {
            for z in 0..n[2] {
                let (px, pz) = (
                    lo[0] + (x as f32 + 0.5) * cell,
                    lo[2] + (z as f32 + 0.5) * cell,
                );
                let mut ys: Vec<f32> = cols[x * n[2] + z]
                    .iter()
                    .filter_map(|&t| hit_y(&tris[t as usize], px, pz))
                    .collect();
                ys.sort_by(f32::total_cmp);
                ys.dedup_by(|a, b| (*a - *b).abs() < 1e-7);
                for pair in ys.chunks(2) {
                    let [a, b] = [pair[0], *pair.get(1).unwrap_or(&pair[0])];
                    let y0 = (((a - lo[1]) / cell) - 0.5).ceil().max(0.0) as usize;
                    let y1 = ((((b - lo[1]) / cell) - 0.5).floor().max(-1.0) + 1.0) as usize;
                    for y in y0..y1.min(n[1]) {
                        occ[(x * n[1] + y) * n[2] + z] = true;
                    }
                }
            }
        }
        Grid { lo, cell, n, occ }
    }

    /// Inside the solid at this object-space point (nearest voxel).
    #[inline]
    pub fn inside(&self, p: [f32; 3]) -> bool {
        let mut i = [0usize; 3];
        for k in 0..3 {
            let f = (p[k] - self.lo[k]) / self.cell;
            if f < 0.0 || f >= self.n[k] as f32 {
                return false;
            }
            i[k] = f as usize;
        }
        self.occ[(i[0] * self.n[1] + i[1]) * self.n[2] + i[2]]
    }

    /// The model's size: the longest box edge in meters.
    pub fn extent(&self) -> f32 {
        self.cell * RES
    }
}

/// Where the vertical line through (x, z) crosses the triangle, if it does.
fn hit_y(t: &[[f32; 3]; 3], x: f32, z: f32) -> Option<f32> {
    let [a, b, c] = *t;
    let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if d.abs() < 1e-14 {
        return None;
    }
    let l0 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / d;
    let l1 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / d;
    let l2 = 1.0 - l0 - l1;
    // Half-open edges so a line through a shared edge counts once.
    if l0 < 0.0 || l1 < 0.0 || l2 <= 0.0 {
        return None;
    }
    Some(l0 * a[1] + l1 * b[1] + l2 * c[1])
}

/// Evenly spread unit directions (Fibonacci sphere), deterministic.
pub fn directions(n: usize) -> Vec<[f32; 3]> {
    let g = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    (0..n)
        .map(|i| {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
            let r = (1.0 - y * y).max(0.0).sqrt();
            let a = g * i as f32;
            [r * a.cos(), y, r * a.sin()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(h: f32) -> Vec<[[f32; 3]; 3]> {
        let v = |x: f32, y: f32, z: f32| [x * h, y * h, z * h];
        let q = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]| [[a, b, c], [a, c, d]];
        let mut t = Vec::new();
        for s in [-1.0f32, 1.0] {
            t.extend(q(
                v(-1., s, -1.),
                v(1., s, -1.),
                v(1., s, 1.),
                v(-1., s, 1.),
            ));
            t.extend(q(
                v(s, -1., -1.),
                v(s, 1., -1.),
                v(s, 1., 1.),
                v(s, -1., 1.),
            ));
            t.extend(q(
                v(-1., -1., s),
                v(1., -1., s),
                v(1., 1., s),
                v(-1., 1., s),
            ));
        }
        t
    }

    #[test]
    fn a_cube_fills_inside_and_not_out() {
        let m = MeshVox::new(cube(0.5));
        let g = m.grid();
        assert!(g.inside([0.0, 0.0, 0.0]));
        assert!(g.inside([0.4, -0.4, 0.4]));
        assert!(!g.inside([0.6, 0.0, 0.0]));
        assert!(!g.inside([0.0, 0.7, 0.0]));
    }
}
