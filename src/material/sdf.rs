//! Signed distance functions (negative inside), 2D and 3D, in meters.

pub type V2 = [f32; 2];
pub type V3 = [f32; 3];

#[inline]
fn len2(p: V2) -> f32 {
    (p[0] * p[0] + p[1] * p[1]).sqrt()
}

#[inline]
fn len3(p: V3) -> f32 {
    (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt()
}

#[inline]
fn sub2(a: V2, b: V2) -> V2 {
    [a[0] - b[0], a[1] - b[1]]
}

#[inline]
fn sub3(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn dot2(a: V2, b: V2) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

#[inline]
fn dot3(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn circle(p: V2, r: f32) -> f32 {
    len2(p) - r
}

/// Box of half-size `b`, corners rounded by `r` (outer size kept).
pub fn rect(p: V2, b: V2, r: f32) -> f32 {
    let q = [p[0].abs() - b[0] + r, p[1].abs() - b[1] + r];
    len2([q[0].max(0.0), q[1].max(0.0)]) + q[0].max(q[1]).min(0.0) - r
}

/// Distance to the segment a-b, minus half the width.
pub fn segment(p: V2, a: V2, b: V2, width: f32) -> f32 {
    let (pa, ba) = (sub2(p, a), sub2(b, a));
    let h = (dot2(pa, ba) / dot2(ba, ba).max(1e-12)).clamp(0.0, 1.0);
    len2([pa[0] - ba[0] * h, pa[1] - ba[1] * h]) - width * 0.5
}

/// A closed polygon (any winding), exact distance.
pub fn polygon(p: V2, v: &[V2]) -> f32 {
    if v.len() < 3 {
        return f32::MAX;
    }
    let mut d = dot2(sub2(p, v[0]), sub2(p, v[0]));
    let mut s = 1.0f32;
    let mut j = v.len() - 1;
    for i in 0..v.len() {
        let e = sub2(v[j], v[i]);
        let w = sub2(p, v[i]);
        let h = (dot2(w, e) / dot2(e, e).max(1e-12)).clamp(0.0, 1.0);
        let b = [w[0] - e[0] * h, w[1] - e[1] * h];
        d = d.min(dot2(b, b));
        let c = [p[1] >= v[i][1], p[1] < v[j][1], e[0] * w[1] > e[1] * w[0]];
        if (c[0] && c[1] && c[2]) || (!c[0] && !c[1] && !c[2]) {
            s = -s;
        }
        j = i;
    }
    s * d.sqrt()
}

/// Signed distance to a set of closed rings filled even-odd (glyph outlines
/// with their holes): negative inside. `boxes` are each ring's bounds
/// (min, max), to skip rings too far to matter.
#[cfg(test)]
pub fn filled(p: V2, rings: &[Vec<V2>], boxes: &[(V2, V2)]) -> f32 {
    let mut d = f32::MAX;
    let mut s = 1.0f32;
    for (v, (lo, hi)) in rings.iter().zip(boxes) {
        if v.len() < 3 {
            continue;
        }
        // A ring changes the sign only where p's row crosses it.
        let crosses = p[1] >= lo[1] && p[1] < hi[1] && p[0] >= lo[0];
        let bx = (lo[0] - p[0]).max(p[0] - hi[0]).max(0.0);
        let by = (lo[1] - p[1]).max(p[1] - hi[1]).max(0.0);
        let near = bx * bx + by * by < d;
        if !crosses && !near {
            continue;
        }
        let mut j = v.len() - 1;
        for i in 0..v.len() {
            let e = sub2(v[j], v[i]);
            let w = sub2(p, v[i]);
            if near {
                let h = (dot2(w, e) / dot2(e, e).max(1e-12)).clamp(0.0, 1.0);
                let b = [w[0] - e[0] * h, w[1] - e[1] * h];
                d = d.min(dot2(b, b));
            }
            let c = [p[1] >= v[i][1], p[1] < v[j][1], e[0] * w[1] > e[1] * w[0]];
            if (c[0] && c[1] && c[2]) || (!c[0] && !c[1] && !c[2]) {
                s = -s;
            }
            j = i;
        }
    }
    s * d.sqrt()
}

/// Closed rings (filled even-odd) with their edges bucketed in a grid, so
/// a distance query looks only at nearby cells and the inside test only at
/// edges crossing its row: text with hundreds of edges stays fast.
pub struct EdgeGrid {
    edges: Vec<(V2, V2)>,
    lo: V2,
    cell: f32,
    nx: usize,
    ny: usize,
    /// Per cell: edges touching it.
    cells: Vec<Vec<u32>>,
    /// Per row of cells: edges whose y span meets the row.
    rows: Vec<Vec<u32>>,
}

impl EdgeGrid {
    pub fn new(rings: &[Vec<V2>]) -> EdgeGrid {
        let mut edges = Vec::new();
        for r in rings {
            for i in 0..r.len() {
                edges.push((r[i], r[(i + 1) % r.len()]));
            }
        }
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for (a, b) in &edges {
            for p in [a, b] {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
        }
        if edges.is_empty() {
            lo = [0.0; 2];
            hi = [1.0; 2];
        }
        // About 4 edges a cell on average.
        let area = ((hi[0] - lo[0]) * (hi[1] - lo[1])).max(1e-8);
        let cell = (area / (edges.len() as f32 / 4.0).max(1.0))
            .sqrt()
            .max(1e-5);
        let nx = (((hi[0] - lo[0]) / cell).ceil() as usize).clamp(1, 512);
        let ny = (((hi[1] - lo[1]) / cell).ceil() as usize).clamp(1, 512);
        let cell = ((hi[0] - lo[0]) / nx as f32)
            .max((hi[1] - lo[1]) / ny as f32)
            .max(1e-5);
        let mut cells = vec![Vec::new(); nx * ny];
        let mut rows = vec![Vec::new(); ny];
        let ci = |v: f32, o: f32, n: usize| (((v - o) / cell).floor().max(0.0) as usize).min(n - 1);
        for (k, (a, b)) in edges.iter().enumerate() {
            let (x0, x1) = (ci(a[0].min(b[0]), lo[0], nx), ci(a[0].max(b[0]), lo[0], nx));
            let (y0, y1) = (ci(a[1].min(b[1]), lo[1], ny), ci(a[1].max(b[1]), lo[1], ny));
            for y in y0..=y1 {
                rows[y].push(k as u32);
                for x in x0..=x1 {
                    cells[y * nx + x].push(k as u32);
                }
            }
        }
        EdgeGrid {
            edges,
            lo,
            cell,
            nx,
            ny,
            cells,
            rows,
        }
    }

    fn inside(&self, p: V2) -> bool {
        let y = (p[1] - self.lo[1]) / self.cell;
        if y < 0.0 || y >= self.ny as f32 {
            return false;
        }
        let mut odd = false;
        for &k in &self.rows[y as usize] {
            let (a, b) = self.edges[k as usize];
            if (a[1] > p[1]) != (b[1] > p[1]) {
                let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if x > p[0] {
                    odd = !odd;
                }
            }
        }
        odd
    }

    /// Signed distance: negative inside.
    pub fn sdf(&self, p: V2) -> f32 {
        if self.edges.is_empty() {
            return f32::MAX;
        }
        let gx = (p[0] - self.lo[0]) / self.cell;
        let gy = (p[1] - self.lo[1]) / self.cell;
        let cx = (gx.floor().max(0.0) as usize).min(self.nx - 1) as i64;
        let cy = (gy.floor().max(0.0) as usize).min(self.ny - 1) as i64;
        // How far p is outside the grid (cells), so rings bound correctly.
        let ox = (-gx).max(gx - self.nx as f32).max(0.0);
        let oy = (-gy).max(gy - self.ny as f32).max(0.0);
        let outside = (ox * ox + oy * oy).sqrt() * self.cell;
        let mut best = f32::MAX;
        let max_r = self.nx.max(self.ny) as i64;
        for r in 0..=max_r {
            // Every edge farther than this from p is in a later ring.
            let bound = outside + (r as f32 - 1.0).max(0.0) * self.cell;
            if best.sqrt() <= bound {
                break;
            }
            for y in (cy - r)..=(cy + r) {
                if y < 0 || y >= self.ny as i64 {
                    continue;
                }
                for x in (cx - r)..=(cx + r) {
                    if x < 0 || x >= self.nx as i64 {
                        continue;
                    }
                    if (y - cy).abs() != r && (x - cx).abs() != r {
                        continue;
                    }
                    for &k in &self.cells[y as usize * self.nx + x as usize] {
                        let (a, b) = self.edges[k as usize];
                        let e = sub2(b, a);
                        let w = sub2(p, a);
                        let h = (dot2(w, e) / dot2(e, e).max(1e-12)).clamp(0.0, 1.0);
                        let q = [w[0] - e[0] * h, w[1] - e[1] * h];
                        best = best.min(dot2(q, q));
                    }
                }
            }
        }
        let d = best.sqrt();
        if self.inside(p) { -d } else { d }
    }
}

/// A regular `n`-pointed star: outer radius `r`, inner radius `ri`.
pub fn star(p: V2, r: f32, ri: f32, n: u32) -> f32 {
    let n = n.max(2);
    let pts: Vec<V2> = (0..n * 2)
        .map(|k| {
            let a = std::f32::consts::PI * k as f32 / n as f32 + std::f32::consts::FRAC_PI_2;
            let rr = if k % 2 == 0 { r } else { ri };
            [a.cos() * rr, a.sin() * rr]
        })
        .collect();
    polygon(p, &pts)
}

/// Regular hexagon, flat sides at ±y, apothem `r`.
pub fn hexagon(p: V2, r: f32) -> f32 {
    let k = [-0.866_025_4f32, 0.5, 0.577_350_3];
    let mut q = [p[0].abs(), p[1].abs()];
    let d = 2.0 * (k[0] * q[0] + k[1] * q[1]).min(0.0);
    q = [q[0] - d * k[0], q[1] - d * k[1]];
    q = [q[0] - q[0].clamp(-k[2] * r, k[2] * r), q[1] - r];
    len2(q) * q[1].signum()
}

/// An arc of radius `r` from `from` to `to` degrees (counter-clockwise from
/// +x), stroke `width`.
pub fn arc(p: V2, r: f32, from: f32, to: f32, width: f32) -> f32 {
    let a = p[1].atan2(p[0]).to_degrees().rem_euclid(360.0);
    let (f, t) = (from.rem_euclid(360.0), to.rem_euclid(360.0));
    let inside = if f <= t {
        a >= f && a <= t
    } else {
        a >= f || a <= t
    };
    if inside {
        (len2(p) - r).abs() - width * 0.5
    } else {
        let end = |deg: f32| {
            let d = deg.to_radians();
            len2(sub2(p, [d.cos() * r, d.sin() * r]))
        };
        end(from).min(end(to)) - width * 0.5
    }
}

/// Distance to the nearest of concentric rings `spacing` apart.
pub fn rings(p: V2, spacing: f32) -> f32 {
    let s = spacing.max(1e-6);
    let r = len2(p);
    let k = (r / s).round() * s;
    (r - k).abs()
}

pub fn sphere(p: V3, r: f32) -> f32 {
    len3(p) - r
}

/// Box of half-size `b`, edges rounded by `r` (outer size kept).
pub fn boxd(p: V3, b: V3, r: f32) -> f32 {
    let q = [
        p[0].abs() - b[0] + r,
        p[1].abs() - b[1] + r,
        p[2].abs() - b[2] + r,
    ];
    len3([q[0].max(0.0), q[1].max(0.0), q[2].max(0.0)]) + q[0].max(q[1]).max(q[2]).min(0.0) - r
}

/// Torus in the xz plane (around y).
pub fn torus(p: V3, major: f32, minor: f32) -> f32 {
    let q = [len2([p[0], p[2]]) - major, p[1]];
    len2(q) - minor
}

pub fn capsule(p: V3, a: V3, b: V3, r: f32) -> f32 {
    let (pa, ba) = (sub3(p, a), sub3(b, a));
    let h = (dot3(pa, ba) / dot3(ba, ba).max(1e-12)).clamp(0.0, 1.0);
    len3([pa[0] - ba[0] * h, pa[1] - ba[1] * h, pa[2] - ba[2] * h]) - r
}

/// Capped cylinder along y: radius `r`, half-height `h`.
pub fn cylinder(p: V3, r: f32, h: f32) -> f32 {
    let d = [len2([p[0], p[2]]) - r, p[1].abs() - h];
    d[0].max(d[1]).min(0.0) + len2([d[0].max(0.0), d[1].max(0.0)])
}

/// Plane through `offset * n` with unit normal `n` (positive on n's side).
pub fn plane(p: V3, n: V3, offset: f32) -> f32 {
    let l = len3(n).max(1e-12);
    dot3(p, n) / l - offset
}

/// Polynomial smooth minimum with blend radius `k` (0 = hard min).
pub fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_match_the_analytic_values() {
        assert!((circle([3.0, 4.0], 2.0) - 3.0).abs() < 1e-6);
        assert!((rect([2.0, 0.0], [1.0, 0.5], 0.0) - 1.0).abs() < 1e-6);
        assert!((rect([0.0, 0.0], [1.0, 0.5], 0.0) + 0.5).abs() < 1e-6);
        assert!((segment([0.5, 1.0], [0.0, 0.0], [1.0, 0.0], 0.2) - 0.9).abs() < 1e-6);
        let sq = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        assert!((polygon([0.5, 0.5], &sq) + 0.5).abs() < 1e-6);
        assert!((polygon([2.0, 0.5], &sq) - 1.0).abs() < 1e-6);
        assert!((hexagon([0.0, 2.0], 1.0) - 1.0).abs() < 1e-5);
        assert!((sphere([0.0, 3.0, 4.0], 1.0) - 4.0).abs() < 1e-6);
        assert!((boxd([0.0, 0.0, 3.0], [1.0, 1.0, 1.0], 0.0) - 2.0).abs() < 1e-6);
        assert!((torus([3.0, 0.0, 0.0], 2.0, 0.5) - 0.5).abs() < 1e-6);
        assert!((cylinder([0.0, 3.0, 0.0], 1.0, 1.0) - 2.0).abs() < 1e-6);
        assert!((plane([0.0, 2.0, 0.0], [0.0, 1.0, 0.0], 0.5) - 1.5).abs() < 1e-6);
        assert!(smin(1.0, 1.0, 0.5) < 1.0);
        assert!((arc([0.0, 1.0], 1.0, 0.0, 180.0, 0.1) + 0.05).abs() < 1e-5);
    }
}

#[cfg(test)]
mod text_tests {
    #[test]
    fn letters_are_negative_inside_and_holes_stay_open() {
        // An "O": a ring with a hole, 1 m tall, centred at the origin.
        let rings = crate::text::outline("O", "sans", 1.0, "center", 1.0, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        let boxes: Vec<([f32; 2], [f32; 2])> = rings
            .iter()
            .map(|r| {
                r.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(a, b), p| {
                    (
                        [a[0].min(p[0]), a[1].min(p[1])],
                        [b[0].max(p[0]), b[1].max(p[1])],
                    )
                })
            })
            .collect();
        let (lo, hi) = boxes
            .iter()
            .fold(([f32::MAX; 2], [f32::MIN; 2]), |(a, b), (l, h)| {
                (
                    [a[0].min(l[0]), a[1].min(l[1])],
                    [b[0].max(h[0]), b[1].max(h[1])],
                )
            });
        let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        // The hole in the middle is outside, the stroke at the left is inside.
        assert!(super::filled(mid, &rings, &boxes) > 0.0);
        assert!(super::filled([lo[0] + 0.03, mid[1]], &rings, &boxes) < 0.0);
        assert!(super::filled([hi[0] + 0.5, mid[1]], &rings, &boxes) > 0.4);
        // The grid gives the same field as the brute-force search.
        let grid = super::EdgeGrid::new(&rings);
        for i in 0..400 {
            let p = [
                lo[0] - 0.2 + (hi[0] - lo[0] + 0.4) * ((i * 37 % 400) as f32 / 400.0),
                lo[1] - 0.2 + (hi[1] - lo[1] + 0.4) * ((i * 91 % 400) as f32 / 400.0),
            ];
            let (a, b) = (super::filled(p, &rings, &boxes), grid.sdf(p));
            assert!((a - b).abs() < 1e-5, "{p:?}: {a} vs {b}");
        }
    }
}
