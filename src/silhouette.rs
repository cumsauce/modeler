//! `silhouette`: a solid carved from two or three orthographic outlines.
//!
//! Each outline (side [z, y], top [x, z], front [x, y]) becomes a 2D signed
//! distance (sampled once on a fine grid), extruded through the third axis;
//! the solid is their intersection, smooth by `blend` so the edges round
//! off, meshed like an `sdf`. It is also an sdf shape (`{"shape":
//! "silhouette", ...}`), so it mixes with spheres, chains and the rest.
//!
//! Outlines may come from reference images (`from_reference`): the
//! subject of a PNG (the same extraction as the `agent_views` reference
//! compare) is traced, simplified and scaled to meters. The server resolves
//! those before parsing, since it knows the project the image lives in.

use bevy::math::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sdf::{SdfDef, Shape};

fn none<T>(v: &Option<T>) -> bool {
    v.is_none()
}

/// The parameters of a `silhouette` op (outlines already expanded).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SilhouetteDef {
    pub side: Vec<Vec<[f32; 2]>>,
    pub top: Vec<Vec<[f32; 2]>>,
    #[serde(default, skip_serializing_if = "none")]
    pub front: Option<Vec<Vec<[f32; 2]>>>,
    /// Smooth radius of the intersection (rounds the edges), meters.
    #[serde(default, skip_serializing_if = "none")]
    pub blend: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub resolution: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub tris: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bake: bool,
    #[serde(default)]
    pub translation: [f32; 3],
    #[serde(default = "ident")]
    pub rotation: [f32; 4],
    #[serde(default, skip_deserializing, skip_serializing_if = "none")]
    pub target_tris: Option<u32>,
    /// Profiles traced from project images: re-traced on re-derive when
    /// the image's content changes (the stored points are the fallback).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<RefSource>,
}

/// Where a traced profile came from (filled by the server's
/// `from_reference` pass; not written by hand).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefSource {
    pub image: String,
    pub view: String,
    #[serde(default, skip_serializing_if = "none")]
    pub height: Option<f32>,
    #[serde(default, skip_serializing_if = "none")]
    pub size: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flip: bool,
    /// Keep every region at least this share of the largest one's area.
    #[serde(default, skip_serializing_if = "none")]
    pub regions: Option<f32>,
    #[serde(default = "yes")]
    pub holes: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub smooth: bool,
    #[serde(default)]
    pub round: f32,
    /// FNV-1a of the image bytes the stored points were traced from.
    pub hash: u64,
}

fn yes() -> bool {
    true
}

/// FNV-1a 64 (stable across builds, unlike `DefaultHasher`).
pub fn content_hash(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The smooth / round post-pass every silhouette outline gets.
pub fn shape_rings(mut rings: Vec<Vec<[f32; 2]>>, smooth: bool, round: f32) -> Vec<Vec<[f32; 2]>> {
    if smooth {
        rings = rings.iter().map(|r| smooth_ring(r, 10)).collect();
    }
    if round > 0.0 {
        rings = crate::csg::rounded_rings(&rings, round)
            .into_iter()
            .map(|r| r.into_iter().map(|p| [p[0] as f32, p[1] as f32]).collect())
            .collect();
    }
    rings
}

fn read_reference(image: &str) -> Option<Vec<u8>> {
    std::fs::read(crate::catalog::project_root()?.join(image)).ok()
}

impl RefSource {
    fn trace(&self, png: &[u8]) -> Result<Vec<Vec<[f32; 2]>>, String> {
        let rings = reference_rings(
            png,
            &self.view,
            self.height,
            self.size,
            self.flip,
            self.regions,
            self.holes,
        )?;
        Ok(shape_rings(rings, self.smooth, self.round))
    }
}

impl SilhouetteDef {
    /// The current images' hashes (for the op cache key): a changed image
    /// misses the cache.
    pub fn live_key(&self) -> Vec<u64> {
        self.references
            .iter()
            .map(|r| read_reference(&r.image).map_or(r.hash, |b| content_hash(&b)))
            .collect()
    }

    /// This def with every reference whose image changed re-traced.
    pub fn live(&self) -> std::borrow::Cow<'_, SilhouetteDef> {
        let mut out = std::borrow::Cow::Borrowed(self);
        for r in &self.references {
            let Some(b) = read_reference(&r.image) else {
                continue;
            };
            if content_hash(&b) == r.hash {
                continue;
            }
            let Ok(rings) = r.trace(&b) else { continue };
            let d = out.to_mut();
            match view_profile(&r.view) {
                Some("front") => d.front = Some(rings),
                Some("top") => d.top = rings,
                _ => d.side = rings,
            }
        }
        out
    }
}

fn ident() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

impl SilhouetteDef {
    pub fn to_sdf(&self) -> SdfDef {
        let moved = self.translation != [0.0; 3];
        let turned = self.rotation != ident();
        SdfDef {
            shapes: vec![Shape {
                shape: "silhouette".into(),
                side: Some(self.side.clone()),
                top: Some(self.top.clone()),
                front: self.front.clone(),
                round: self.blend.filter(|b| *b > 0.0),
                at: moved.then_some(self.translation),
                rotation: turned.then_some(self.rotation),
                ..Shape::default()
            }],
            blend: Some(0.0),
            resolution: self.resolution,
            tris: self.tris,
            bake: self.bake,
            target_tris: self.target_tris,
            ..SdfDef::default()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        check_rings(&self.side).map_err(|e| format!("side: {e}"))?;
        check_rings(&self.top).map_err(|e| format!("top: {e}"))?;
        if let Some(f) = &self.front {
            check_rings(f).map_err(|e| format!("front: {e}"))?;
        }
        if self.blend.is_some_and(|b| !b.is_finite() || b < 0.0) {
            return Err("blend: must be >= 0 meters".into());
        }
        if self.resolution.is_some_and(|r| !r.is_finite() || r <= 0.0) {
            return Err("resolution: must be > 0 meters".into());
        }
        match self.tris {
            Some(t) if t < 4 => Err(format!("tris: at least 4 triangles, got {t}")),
            None if self.bake => Err("bake: needs `tris`".into()),
            _ => Ok(()),
        }
    }

    pub fn summary(&self) -> String {
        let n = |r: &Vec<Vec<[f32; 2]>>| r.iter().map(Vec::len).sum::<usize>();
        let mut s = format!("side {} pts, top {} pts", n(&self.side), n(&self.top));
        if let Some(f) = &self.front {
            s += &format!(", front {} pts", n(f));
        }
        if let Some(b) = self.blend {
            s += &format!(" blend={b}");
        }
        if let Some(r) = self.resolution {
            s += &format!(" resolution={r}");
        }
        if let Some(t) = self.tris {
            s += &format!(" tris={t}");
        }
        s
    }
}

/// Every ring has 3+ finite points and encloses some area.
pub fn check_rings(rings: &[Vec<[f32; 2]>]) -> Result<(), String> {
    if rings.is_empty() {
        return Err("a closed outline of at least 3 points".into());
    }
    for (i, r) in rings.iter().enumerate() {
        if r.len() < 3 {
            return Err(format!("ring {i}: at least 3 points, got {}", r.len()));
        }
        if r.iter().flatten().any(|v| !v.is_finite()) {
            return Err(format!("ring {i}: points must be finite"));
        }
        if area(r).abs() < 1e-10 {
            return Err(format!("ring {i}: encloses no area"));
        }
    }
    Ok(())
}

fn area(r: &[[f32; 2]]) -> f32 {
    let mut a = 0.0;
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    0.5 * a
}

/// Degrees of turning per sample of a smooth outline.
const SMOOTH_TURN_DEG: f32 = 1.5;

/// A closed smooth curve through every point of the ring: a cubic
/// Hermite spline over centripetal (square-root chord) parameters, at
/// least `per` samples a span, more where the span turns (one sample per
/// [`SMOOTH_TURN_DEG`] of turning, at most 96), so a curved outline is a
/// polyline whose facets meet at under two degrees and shade smooth.
/// Tangents are monotone per axis (Fritsch-Carlson): a point that is an
/// extreme along x or y gets a flat tangent there, and no span overshoots
/// its end points on either axis. So the curve never leaves its control
/// points' box (a flat car floor stays at its height instead of sagging
/// below it, a corner doesn't bulge outside the outline's box) and stays
/// smooth where it touches it: no clamped flat with a kink at each end.
pub fn smooth_ring(r: &[[f32; 2]], per: usize) -> Vec<[f32; 2]> {
    let n = r.len();
    if n < 3 {
        return r.to_vec();
    }
    let p = |i: usize| Vec2::from(r[i % n]);
    // Centripetal parameter step from point i to i + 1.
    let dt = |i: usize| p(i).distance(p(i + 1)).max(1e-6).sqrt();
    // Per-axis tangent at point i: the central difference, flat at a
    // per-axis extreme.
    let tangent = |i: usize| -> Vec2 {
        let (a, b, c) = (p(i + n - 1), p(i), p(i + 1));
        let h = dt(i + n - 1) + dt(i);
        let m = (c - a) / h;
        let flat = |x0: f32, x1: f32, x2: f32| (x1 - x0) * (x2 - x1) <= 0.0;
        Vec2::new(
            if flat(a.x, b.x, c.x) { 0.0 } else { m.x },
            if flat(a.y, b.y, c.y) { 0.0 } else { m.y },
        )
    };
    let tangents: Vec<Vec2> = (0..n).map(tangent).collect();
    let mut out = Vec::with_capacity(n * per);
    for i in 0..n {
        let (p1, p2) = (p(i), p(i + 1));
        let h = dt(i);
        let (mut m1, mut m2) = (tangents[i], tangents[(i + 1) % n]);
        // Fritsch-Carlson per axis: no overshoot inside the span.
        for ax in 0..2 {
            let d = (p2[ax] - p1[ax]) / h;
            if d == 0.0 {
                m1[ax] = 0.0;
                m2[ax] = 0.0;
                continue;
            }
            let (a, b) = ((m1[ax] / d).max(0.0), (m2[ax] / d).max(0.0));
            let (a, b) = if a * a + b * b > 9.0 {
                let k = 3.0 / (a * a + b * b).sqrt();
                (a * k, b * k)
            } else {
                (a, b)
            };
            m1[ax] = a * d;
            m2[ax] = b * d;
        }
        let (d0, d1) = (
            (p2 - p(i + n - 1)).normalize_or_zero(),
            (p(i + 2) - p1).normalize_or_zero(),
        );
        let turn = d0.dot(d1).clamp(-1.0, 1.0).acos().to_degrees();
        let per = per.max(((turn / SMOOTH_TURN_DEG).ceil() as usize).min(96));
        for k in 0..per {
            let u = k as f32 / per as f32;
            let (u2, u3) = (u * u, u * u * u);
            let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
            let h10 = u3 - 2.0 * u2 + u;
            let h01 = -2.0 * u3 + 3.0 * u2;
            let h11 = u3 - u2;
            let c = p1 * h00 + m1 * (h10 * h) + p2 * h01 + m2 * (h11 * h);
            out.push(c.to_array());
        }
    }
    out
}

/// Exact signed distance to a set of rings (even-odd inside = negative).
pub fn poly_sdf(rings: &[Vec<[f32; 2]>], p: Vec2) -> f32 {
    let mut d2 = f32::MAX;
    let mut inside = false;
    for r in rings {
        let n = r.len();
        for i in 0..n {
            let a = Vec2::from(r[i]);
            let b = Vec2::from(r[(i + 1) % n]);
            let e = b - a;
            let w = p - a;
            let h = (w.dot(e) / e.length_squared().max(1e-20)).clamp(0.0, 1.0);
            d2 = d2.min((w - e * h).length_squared());
            if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * e.x {
                inside = !inside;
            }
        }
    }
    let d = d2.sqrt();
    if inside { -d } else { d }
}

/// A 2D distance field sampled on a grid (bilinear inside, the distance to
/// the grid plus its edge value outside).
pub struct Grid2 {
    lo: Vec2,
    cell: f32,
    nx: usize,
    ny: usize,
    d: Vec<f32>,
}

impl Grid2 {
    fn new(rings: &[Vec<[f32; 2]>], cell: f32, margin: f32) -> Grid2 {
        let (lo, hi) = ring_box(rings);
        let m = Vec2::splat(margin + 2.0 * cell);
        let (lo, hi) = (lo - m, hi + m);
        let ext = hi - lo;
        let cell = cell.max(ext.max_element() / 2048.0).max(1e-6);
        let nx = (ext.x / cell).ceil() as usize + 1;
        let ny = (ext.y / cell).ceil() as usize + 1;
        let d = (0..nx * ny)
            .map(|i| {
                let (x, y) = (i % nx, i / nx);
                poly_sdf(rings, lo + Vec2::new(x as f32, y as f32) * cell)
            })
            .collect();
        Grid2 {
            lo,
            cell,
            nx,
            ny,
            d,
        }
    }

    fn sample(&self, p: Vec2) -> f32 {
        let hi = self.lo + Vec2::new((self.nx - 1) as f32, (self.ny - 1) as f32) * self.cell;
        let c = p.clamp(self.lo, hi);
        let g = (c - self.lo) / self.cell;
        let (x0, y0) = (
            (g.x.floor() as usize).min(self.nx - 2),
            (g.y.floor() as usize).min(self.ny - 2),
        );
        let (fx, fy) = (g.x - x0 as f32, g.y - y0 as f32);
        let at = |x: usize, y: usize| self.d[y * self.nx + x];
        let a = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
        let b = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
        a * (1.0 - fy) + b * fy + (p - c).length()
    }
}

fn ring_box(rings: &[Vec<[f32; 2]>]) -> (Vec2, Vec2) {
    let mut lo = Vec2::splat(f32::MAX);
    let mut hi = Vec2::splat(f32::MIN);
    for p in rings.iter().flatten() {
        lo = lo.min(Vec2::from(*p));
        hi = hi.max(Vec2::from(*p));
    }
    (lo, hi)
}

/// A compiled silhouette shape (local coordinates).
pub struct Sil {
    side: Grid2,
    top: Grid2,
    front: Option<Grid2>,
    round: f32,
    lo: Vec3,
    hi: Vec3,
}

fn smax(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.max(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.max(b) + h * h * k * 0.25
}

impl Sil {
    /// `cell` 0 = a coarse grid (only the box is wanted).
    pub fn new(
        side: &[Vec<[f32; 2]>],
        top: &[Vec<[f32; 2]>],
        front: Option<&Vec<Vec<[f32; 2]>>>,
        round: f32,
        cell: f32,
        band: f32,
    ) -> Sil {
        let (slo, shi) = ring_box(side);
        let (tlo, thi) = ring_box(top);
        let mut lo = Vec3::new(tlo.x, slo.y, slo.x.max(tlo.y));
        let mut hi = Vec3::new(thi.x, shi.y, shi.x.min(thi.y));
        if let Some(f) = front {
            let (flo, fhi) = ring_box(f);
            lo.x = lo.x.max(flo.x);
            hi.x = hi.x.min(fhi.x);
            lo.y = lo.y.max(flo.y);
            hi.y = hi.y.min(fhi.y);
        }
        let cell = if cell > 0.0 {
            cell
        } else {
            (hi - lo).max_element().max(1e-3) / 16.0
        };
        let margin = band + round;
        Sil {
            side: Grid2::new(side, cell, margin),
            top: Grid2::new(top, cell, margin),
            front: front.map(|f| Grid2::new(f, cell, margin)),
            round,
            lo,
            hi,
        }
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        (self.lo, self.hi)
    }

    pub fn eval(&self, q: Vec3) -> f32 {
        let mut d = smax(
            self.side.sample(Vec2::new(q.z, q.y)),
            self.top.sample(Vec2::new(q.x, q.z)),
            self.round,
        );
        if let Some(f) = &self.front {
            d = smax(d, f.sample(Vec2::new(q.x, q.y)), self.round);
        }
        d
    }
}

// ------------------------------------------------------------ references

/// The outlines of a mask's regions in pixel corners (x right, y down),
/// simplified to within `eps` pixels: the largest region's outer ring,
/// with `regions` every region at least that share of its area too, and
/// with `holes` the holes inside kept regions (over 4 px²). Rings read
/// even-odd.
pub fn trace_rings(
    mask: &[bool],
    w: usize,
    h: usize,
    eps: f32,
    regions: Option<f32>,
    holes: bool,
) -> Vec<Vec<[f32; 2]>> {
    let on = |x: i64, y: i64| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && mask[y as usize * w + x as usize]
    };
    let id = |x: usize, y: usize| (y * (w + 1) + x) as u32;
    let mut next: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let (xi, yi) = (x as i64, y as i64);
            let mut add = |a: u32, b: u32| next.entry(a).or_default().push(b);
            if !on(xi, yi - 1) {
                add(id(x, y), id(x + 1, y));
            }
            if !on(xi + 1, yi) {
                add(id(x + 1, y), id(x + 1, y + 1));
            }
            if !on(xi, yi + 1) {
                add(id(x + 1, y + 1), id(x, y + 1));
            }
            if !on(xi - 1, yi) {
                add(id(x, y + 1), id(x, y));
            }
        }
    }
    let pt = |c: u32| {
        let c = c as usize;
        [(c % (w + 1)) as f32, (c / (w + 1)) as f32]
    };
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    // Outer rings run clockwise on screen (positive shoelace area with y
    // down), holes the other way.
    let mut loops: Vec<(f32, Vec<[f32; 2]>)> = Vec::new();
    for s in starts {
        while let Some(b) = next.get_mut(&s).and_then(Vec::pop) {
            let mut ring = vec![pt(s)];
            let mut cur = b;
            while cur != s {
                ring.push(pt(cur));
                match next.get_mut(&cur).and_then(Vec::pop) {
                    Some(n) => cur = n,
                    None => break,
                }
            }
            if ring.len() >= 3 {
                loops.push((area(&ring), ring));
            }
        }
    }
    let biggest = loops.iter().map(|l| l.0).fold(0.0f32, f32::max);
    if biggest <= 0.0 {
        return Vec::new();
    }
    let mut outer: Vec<&Vec<[f32; 2]>> = Vec::new();
    for (a, r) in &loops {
        let keep = match regions {
            Some(f) => *a > 0.0 && *a >= f.max(0.0) * biggest,
            None => outer.is_empty() && *a == biggest,
        };
        if keep {
            outer.push(r);
        }
    }
    let mut rings: Vec<Vec<[f32; 2]>> = outer.iter().map(|r| simplify_closed(r, eps)).collect();
    if holes {
        for (a, r) in &loops {
            if *a < -4.0 {
                let probe = r.iter().map(|p| Vec2::from(*p)).sum::<Vec2>() / r.len() as f32;
                if outer.iter().any(|o| inside(o, probe)) {
                    rings.push(simplify_closed(r, eps));
                }
            }
        }
    }
    rings.retain(|r| r.len() >= 3);
    rings
}

fn inside(r: &[[f32; 2]], p: Vec2) -> bool {
    let mut c = false;
    let n = r.len();
    for i in 0..n {
        let (a, b) = (Vec2::from(r[i]), Vec2::from(r[(i + 1) % n]));
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            c = !c;
        }
    }
    c
}

fn simplify_closed(r: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    let p0 = Vec2::from(r[0]);
    let far = (0..r.len())
        .max_by(|&a, &b| {
            p0.distance_squared(Vec2::from(r[a]))
                .total_cmp(&p0.distance_squared(Vec2::from(r[b])))
        })
        .unwrap_or(0);
    let mut a: Vec<[f32; 2]> = r[..=far].to_vec();
    let mut b: Vec<[f32; 2]> = r[far..].to_vec();
    b.push(r[0]);
    a = dp(&a, eps);
    b = dp(&b, eps);
    a.pop();
    b.pop();
    a.extend(b);
    a
}

fn dp(pts: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (Vec2::from(pts[0]), Vec2::from(pts[pts.len() - 1]));
    let e = b - a;
    let mut worst = (0usize, 0.0f32);
    for (i, p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let w = Vec2::from(*p) - a;
        let d = if e.length_squared() < 1e-12 {
            w.length()
        } else {
            (w.perp_dot(e)).abs() / e.length()
        };
        if d > worst.1 {
            worst = (i, d);
        }
    }
    if worst.1 <= eps {
        return vec![pts[0], pts[pts.len() - 1]];
    }
    let mut l = dp(&pts[..=worst.0], eps);
    let r = dp(&pts[worst.0..], eps);
    l.pop();
    l.extend(r);
    l
}

/// Which profile a reference view fills.
pub fn view_profile(view: &str) -> Option<&'static str> {
    match view {
        "front" | "back" => Some("front"),
        "side" | "right" | "left" => Some("side"),
        "top" => Some("top"),
        _ => None,
    }
}

/// Traces a reference image into profile rings (meters) for `view`
/// (see `trace_rings` for `regions` / `holes`): scaled so the subject's
/// vertical extent is `height` (uniform), or to `size` [w, h]. Front /
/// side outlines stand on y = 0 centred horizontally; a top outline is
/// centred. Conventions (profile axes [a, b]):
/// - `front` (seen from +z): [x, y], image right = +x;
/// - `back` (seen from -z): image right = -x;
/// - `side` / `right` (seen from +x, the model's right): [z, y], image
///   right = -z, so a car drawn facing left (its nose at image left)
///   faces +z;
/// - `left` (seen from -x): image right = +z;
/// - `top` (plan, seen from +y, -z up the image): [x, z], image right =
///   +x, image down = +z.
///
/// `flip` mirrors the image left-right first (a side drawing facing the
/// other way).
pub fn reference_rings(
    png: &[u8],
    view: &str,
    height: Option<f32>,
    size: Option<[f32; 2]>,
    flip: bool,
    regions: Option<f32>,
    holes: bool,
) -> Result<Vec<Vec<[f32; 2]>>, String> {
    let c = crate::raster::decode_image(png)?;
    let mask = crate::raster::reference_mask(&c);
    let rings = trace_rings(&mask, c.w, c.h, 0.75, regions, holes);
    if rings.is_empty() {
        return Err("no subject found (no opaque or contrasting pixels)".into());
    }
    let (lo, hi) = ring_box(&rings);
    let (pw, ph) = ((hi.x - lo.x).max(1.0), (hi.y - lo.y).max(1.0));
    let (sx, sy) = match (size, height) {
        (Some([w, h]), _) => (w / pw, h / ph),
        (None, Some(h)) => (h / ph, h / ph),
        (None, None) => {
            return Err(
                "needs a size: `height` (meters, the image subject's vertical extent) or `size` [w, h]"
                    .into(),
            );
        }
    };
    let cx = 0.5 * (lo.x + hi.x);
    let cy = 0.5 * (lo.y + hi.y);
    let sign = if flip { -1.0 } else { 1.0 };
    let out = rings
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|p| {
                    let u = (p[0] - cx) * sx * sign;
                    match view {
                        "top" => [u, (p[1] - cy) * sy],
                        "back" | "side" | "right" => [-u, (hi.y - p[1]) * sy],
                        _ => [u, (hi.y - p[1]) * sy],
                    }
                })
                .collect()
        })
        .collect();
    Ok(out)
}

/// The outer outline of the reference's largest region (no holes).
pub fn reference_profile(
    png: &[u8],
    view: &str,
    height: Option<f32>,
    size: Option<[f32; 2]>,
) -> Result<Vec<[f32; 2]>, String> {
    let mut r = reference_rings(png, view, height, size, false, None, false)?;
    Ok(r.swap_remove(0))
}

/// Replaces every `from_reference` of `silhouette` items (and sdf
/// `silhouette` shapes, also in groups and repeats) with traced profiles.
/// `read` loads an image by its project path.
pub fn resolve_references(
    items: &mut [Value],
    read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    for (i, it) in items.iter_mut().enumerate() {
        resolve_item(it, read).map_err(|e| format!("op #{i}: {e}"))?;
    }
    Ok(())
}

fn resolve_item(
    v: &mut Value,
    read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let Some(o) = v.as_object_mut() else {
        return Ok(());
    };
    let ty = o
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ty == "repeat"
        && let Some(Value::Array(a)) = o.get_mut("items")
    {
        for x in a {
            resolve_item(x, read)?;
        }
    }
    if ty == "sdf"
        && let Some(Value::Array(a)) = o.get_mut("shapes")
    {
        for x in a {
            resolve_shape(x, read)?;
        }
    }
    if ty == "silhouette" {
        fill(o, read).map_err(|e| format!("silhouette: {e}"))?;
    }
    Ok(())
}

fn resolve_shape(
    v: &mut Value,
    read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let Some(o) = v.as_object_mut() else {
        return Ok(());
    };
    if let Some(Value::Array(a)) = o.get_mut("shapes") {
        for x in a {
            resolve_shape(x, read)?;
        }
    }
    if o.get("shape").and_then(Value::as_str) == Some("silhouette") {
        fill(o, read).map_err(|e| format!("silhouette shape: {e}"))?;
    }
    Ok(())
}

fn fill(
    o: &mut serde_json::Map<String, Value>,
    read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let Some(r) = o.remove("from_reference") else {
        return Ok(());
    };
    let list = match r {
        Value::Array(a) => a,
        one => vec![one],
    };
    let smooth = o.get("smooth").and_then(Value::as_bool) == Some(true);
    let round = o.get("round").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let silhouette_item = o.get("type").and_then(Value::as_str) == Some("silhouette");
    let mut sources = Vec::new();
    for r in list {
        let image = r
            .get("image")
            .and_then(Value::as_str)
            .ok_or("from_reference: {image, view, height | size, flip?, regions?, holes?}")?;
        let view = r.get("view").and_then(Value::as_str).unwrap_or("");
        let field = view_profile(view).ok_or_else(|| {
            format!(
                "from_reference.view: front (seen from +z, image right = +x), back, side / right (seen from +x: image right = -z, so a drawing facing left faces +z), left (seen from -x) or top (plan, image down = +z), got `{view}`"
            )
        })?;
        const FIELDS: [&str; 7] = [
            "image", "view", "height", "size", "flip", "regions", "holes",
        ];
        for k in r.as_object().into_iter().flat_map(|m| m.keys()) {
            if !FIELDS.contains(&k.as_str()) {
                return Err(format!(
                    "from_reference: unknown field `{k}` ({})",
                    FIELDS.join(", ")
                ));
            }
        }
        if o.contains_key(field) {
            return Err(format!(
                "from_reference: `{field}` is given twice (a profile and a reference)"
            ));
        }
        let src = RefSource {
            image: image.into(),
            view: view.into(),
            height: r.get("height").and_then(Value::as_f64).map(|v| v as f32),
            size: r.get("size").and_then(|s| {
                let a = s.as_array()?;
                Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            }),
            flip: r.get("flip").and_then(Value::as_bool) == Some(true),
            regions: r.get("regions").and_then(|v| match v {
                Value::Bool(true) => Some(0.02),
                v => v.as_f64().map(|f| f as f32),
            }),
            holes: r.get("holes").and_then(Value::as_bool) != Some(false),
            smooth,
            round,
            hash: 0,
        };
        let bytes = read(image)?;
        let rings = reference_rings(
            &bytes,
            view,
            src.height,
            src.size,
            src.flip,
            src.regions,
            src.holes,
        )
        .map_err(|e| format!("from_reference `{image}`: {e}"))?;
        o.insert(field.into(), json!(rings));
        sources.push(RefSource {
            hash: content_hash(&bytes),
            ..src
        });
    }
    if silhouette_item {
        o.insert("references".into(), json!(sources));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(a: f32, b: f32, c: f32, d: f32) -> Vec<Vec<[f32; 2]>> {
        vec![vec![[a, b], [c, b], [c, d], [a, d]]]
    }

    #[test]
    fn smooth_rings_stay_inside_their_control_box() {
        let r = [
            [2.2, 0.3],
            [2.25, 0.75],
            [1.0, 0.85],
            [0.4, 1.4],
            [-1.2, 1.4],
            [-2.2, 0.95],
            [-2.2, 0.3],
        ];
        let s = smooth_ring(&r, 10);
        let (lo, hi) = ring_box(std::slice::from_ref(&s));
        assert!(lo.y >= 0.3 - 1e-5 && hi.y <= 1.4 + 1e-5, "{lo} {hi}");
        assert!(lo.x >= -2.2 - 1e-5 && hi.x <= 2.25 + 1e-5, "{lo} {hi}");
    }

    #[test]
    fn three_rectangles_make_a_box() {
        let def = SilhouetteDef {
            side: rect(-0.3, 0.0, 0.3, 0.4),
            top: rect(-0.2, -0.3, 0.2, 0.3),
            front: Some(rect(-0.2, 0.0, 0.2, 0.4)),
            rotation: ident(),
            ..SilhouetteDef::default()
        };
        def.validate().unwrap_or_else(|e| panic!("{e}"));
        let none = |n: &str| -> Result<std::sync::Arc<crate::Part>, crate::ModelerError> {
            Err(crate::ModelerError::Invalid(n.into()))
        };
        let p = crate::sdf::build("b", &def.to_sdf(), &none).unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = p.bounds();
        let want = (Vec3::new(-0.2, 0.0, -0.3), Vec3::new(0.2, 0.4, 0.3));
        assert!((lo - want.0).abs().max_element() < 0.01, "{lo}");
        assert!((hi - want.1).abs().max_element() < 0.01, "{hi}");
        let vol = crate::inspect::volume_centroid(&p).0 as f32;
        let box_vol = 0.4 * 0.4 * 0.6;
        assert!((vol - box_vol).abs() < 0.03 * box_vol, "{vol} vs {box_vol}");
        assert_eq!(crate::verify::open_edge_count(&p), 0);
    }

    #[test]
    fn a_traced_disc_is_round_and_scaled() {
        let (w, h) = (64usize, 64usize);
        let mut px = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - 31.5, y as f32 - 31.5);
                if dx * dx + dy * dy < 20.0 * 20.0 {
                    px[(y * w + x) * 4..][..3].copy_from_slice(&[0, 0, 0]);
                }
            }
        }
        let png =
            crate::raster::encode_png(w as u32, h as u32, &px).unwrap_or_else(|e| panic!("{e}"));
        let r = reference_profile(&png, "front", Some(1.0), None).unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = ring_box(std::slice::from_ref(&r));
        assert!(
            (hi.y - lo.y - 1.0).abs() < 1e-4 && lo.y.abs() < 1e-4,
            "{lo} {hi}"
        );
        assert!((hi.x - lo.x - 1.0).abs() < 0.08, "{lo} {hi}");
        let a = area(&r).abs();
        assert!((a - std::f32::consts::PI * 0.25).abs() < 0.05, "{a}");
        // The JSON pass fills the profile and drops the field.
        let mut items = vec![json!({"type": "silhouette", "name": "s",
            "from_reference": {"image": "x.png", "view": "side", "height": 0.5}})];
        resolve_references(&mut items, &|_| Ok(png.clone())).unwrap_or_else(|e| panic!("{e}"));
        assert!(items[0].get("side").is_some() && items[0].get("from_reference").is_none());
    }

    /// A ring (disc with a hole) plus a small separate dot, `r` the outer
    /// radius in pixels, centred at `cx`.
    fn ring_png(r: f32, cx: f32) -> Vec<u8> {
        let (w, h) = (96usize, 64usize);
        let mut px = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - 32.0);
                let d = (dx * dx + dy * dy).sqrt();
                let dot = (x as f32 - 85.0).abs() < 4.0 && (y as f32 - 32.0).abs() < 4.0;
                if (d < r && d > r * 0.45) || dot {
                    px[(y * w + x) * 4..][..3].copy_from_slice(&[0, 0, 0]);
                }
            }
        }
        crate::raster::encode_png(w as u32, h as u32, &px).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn traced_rings_keep_holes_and_regions() {
        let png = ring_png(20.0, 40.0);
        let c = crate::raster::decode_image(&png).unwrap_or_else(|e| panic!("{e}"));
        let m = crate::raster::reference_mask(&c);
        assert_eq!(trace_rings(&m, c.w, c.h, 0.75, None, false).len(), 1);
        assert_eq!(trace_rings(&m, c.w, c.h, 0.75, None, true).len(), 2);
        assert_eq!(trace_rings(&m, c.w, c.h, 0.75, Some(0.02), true).len(), 3);
        // flip mirrors: the dot (image right) moves to the other side.
        let side = reference_rings(&png, "side", Some(1.0), None, false, Some(0.02), true)
            .unwrap_or_else(|e| panic!("{e}"));
        let flipped = reference_rings(&png, "side", Some(1.0), None, true, Some(0.02), true)
            .unwrap_or_else(|e| panic!("{e}"));
        let dot_z = |rs: &Vec<Vec<[f32; 2]>>| {
            rs.iter()
                .min_by(|a, b| area(a).abs().total_cmp(&area(b).abs()))
                .map(|r| r[0][0])
                .unwrap_or(0.0)
        };
        // Side views: image right = -z.
        assert!(dot_z(&side) < 0.0 && dot_z(&flipped) > 0.0);
    }

    /// The server's apply_ops path: the pre-pass reads a PNG from the
    /// project, the batch parses and evaluates, and a later edit of the
    /// image re-traces the op on re-derive.
    #[test]
    fn from_reference_end_to_end_and_retrace() {
        let dir = std::env::temp_dir().join(format!("sil_ref_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("refs")).unwrap_or_else(|e| panic!("{e}"));
        let img = dir.join("refs/side.png");
        std::fs::write(&img, ring_png(20.0, 40.0)).unwrap_or_else(|e| panic!("{e}"));
        let prev = crate::catalog::set_call_project(Some(Some(dir.clone())));
        let read = |f: &str| std::fs::read(dir.join(f)).map_err(|e| e.to_string());
        let mut items = vec![json!({"type": "silhouette", "name": "wheel",
            "from_reference": {"image": "refs/side.png", "view": "side", "height": 0.6,
                               "flip": true, "regions": true},
            "top": [[-0.05, -0.4], [0.05, -0.4], [0.05, 0.4], [-0.05, 0.4]]})];
        resolve_references(&mut items, &read).unwrap_or_else(|e| panic!("{e}"));
        let mut s = crate::commands::Session::default();
        let res = s.apply_values(None, items);
        let out = (|| {
            res.map_err(|e| e.to_string())?;
            let Some(crate::Op::Silhouette { def, .. }) = s.model.ops.first() else {
                return Err("no silhouette op".to_string());
            };
            if def.side.len() != 3 || def.references.len() != 1 || !def.references[0].flip {
                return Err(format!("{} rings, {:?}", def.side.len(), def.references));
            }
            let h0 = s.model.part("wheel").ok_or("no part")?.bounds().1.y;
            if (h0 - 0.6).abs() > 0.03 {
                return Err(format!("height {h0}"));
            }
            // A taller subject in the image (relative to the dot): re-derive
            // re-traces, so the ring's share of the 0.6 m changes the width.
            let w0 = {
                let (lo, hi) = s.model.part("wheel").ok_or("no part")?.bounds();
                hi.z - lo.z
            };
            std::fs::write(&img, ring_png(12.0, 40.0)).map_err(|e| e.to_string())?;
            s.model.refresh().map_err(|e| e.to_string())?;
            let (lo, hi) = s.model.part("wheel").ok_or("no part")?.bounds();
            if (hi.z - lo.z - w0).abs() < 0.02 {
                return Err(format!("not re-traced: width {w0} -> {}", hi.z - lo.z));
            }
            Ok(())
        })();
        crate::catalog::set_call_project(prev);
        let _ = std::fs::remove_dir_all(&dir);
        out.unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn smooth_outline_walls_shade_like_the_curve() {
        let side = vec![vec![
            [0.5, 0.02],
            [0.45, 0.2],
            [0.0, 0.32],
            [-0.5, 0.12],
            [-0.5, 0.02],
        ]];
        let top = vec![vec![
            [-0.03, 0.5],
            [0.12, 0.3],
            [0.18, -0.2],
            [0.0, -0.5],
            [-0.18, -0.2],
            [-0.12, 0.3],
        ]];
        let def = SilhouetteDef {
            side: shape_rings(side, true, 0.0),
            top: shape_rings(top.clone(), true, 0.0),
            rotation: ident(),
            ..SilhouetteDef::default()
        };
        let none = |n: &str| -> Result<std::sync::Arc<crate::Part>, crate::ModelerError> {
            Err(crate::ModelerError::Invalid(n.into()))
        };
        let p = crate::sdf::build("fin", &def.to_sdf(), &none).unwrap_or_else(|e| panic!("{e}"));
        // The true wall normal: a dense sampling of the same curve.
        let dense = smooth_ring(&top[0], 400);
        let n = dense.len();
        let seg_n = |i: usize| {
            let (a, b) = (Vec2::from(dense[i]), Vec2::from(dense[(i + 1) % n]));
            let d = (b - a).normalize_or_zero();
            Vec2::new(d.y, -d.x)
        };
        let ring_area: f32 = (0..n)
            .map(|i| dense[i][0] * dense[(i + 1) % n][1] - dense[(i + 1) % n][0] * dense[i][1])
            .sum();
        let flip = if ring_area > 0.0 { 1.0 } else { -1.0 };
        let cos_kink = 0.5f32.to_radians().cos();
        let kinks: Vec<Vec2> = (0..n)
            .filter(|&i| seg_n(i).dot(seg_n((i + 1) % n)) < cos_kink)
            .map(|i| Vec2::from(dense[(i + 1) % n]))
            .collect();
        let mut worst = 0.0f32;
        let mut count = 0;
        for (v, nm) in p.verts.iter().zip(&p.normals) {
            if v[1] < 0.05 || v[1] > 0.1 || nm[1].abs() > 0.05 {
                continue;
            }
            let q = Vec2::new(v[0], v[2]);
            let (mut best, mut bi) = (f32::MAX, 0);
            for (i, pt) in dense.iter().enumerate() {
                let d = Vec2::from(*pt).distance_squared(q);
                if d < best {
                    best = d;
                    bi = i;
                }
            }
            // Skip the curve's own kinks (where the per-axis clamp flattens it).
            if kinks.iter().any(|k| k.distance(q) < 0.02) {
                continue;
            }
            let truth = seg_n(bi) * flip;
            let m = Vec2::new(nm[0], nm[2]).normalize_or_zero();
            let deg = m.dot(truth).clamp(-1.0, 1.0).acos().to_degrees();
            worst = worst.max(deg);
            count += 1;
        }
        eprintln!("fin wall: {count} vertices, worst normal error {worst:.2} deg");
        assert!(count > 50, "{count}");
        assert!(
            worst < 1.0,
            "wall normals off the smooth curve by {worst} deg"
        );
    }
}
