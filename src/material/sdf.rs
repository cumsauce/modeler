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
