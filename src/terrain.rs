//! Terrain: a ground slab whose top is a noise heightfield at real size.
//!
//! The output is one closed solid: the heightfield on top (smooth normals),
//! four vertical skirt walls (flat normals) and a flat bottom at `base`, so
//! booleans, `place`, `verify grounded` and rays all work on it. Heights are
//! a pure function of (x, z, settings): the same seed grows the same ground,
//! and a bigger `size` extends it without moving what was already there.

use serde::{Deserialize, Serialize};

use crate::Part;
use crate::material::noise::{Lattice, gradient};

/// A level pad: the ground inside eases to `height` (default: the ground's
/// own height at `at`), blending back over `falloff` meters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pad {
    pub at: [f32; 2],
    /// Circle radius (ignored when `size` is given).
    #[serde(default)]
    pub radius: f32,
    /// A rectangle [width, depth] instead of a circle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    pub falloff: f32,
}

/// A carved bed (road, river, trench) along a polyline in x/z: level across
/// its width, following the ground lengthwise, `depth` below it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Path {
    pub points: Vec<[f32; 2]>,
    pub width: f32,
    pub depth: f32,
    pub falloff: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Terrain {
    /// [width (x), depth (z)], meters, centred on the origin.
    pub size: [f32; 2],
    /// Meters per grid cell.
    pub resolution: f32,
    /// Relief: the style's top maps to y = height, its bottom to 0.
    pub height: f32,
    /// Feature size, meters (the largest hills / dune spacing).
    pub scale: f32,
    pub octaves: u32,
    /// Amplitude kept per octave (0.5).
    pub gain: f32,
    /// hills, ridges, dunes, mesa or flat.
    pub style: String,
    pub seed: u64,
    /// y of the flat bottom (below every top point).
    pub base: f32,
    /// mesa: number of terraces.
    pub terraces: u32,
    /// dunes: wind direction, degrees about +y (0 = crests run along x).
    pub wind: f32,
    /// Meters over which the edges ease down to y = 0 (0 = off).
    pub edge_falloff: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flatten: Vec<Pad>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<Path>,
    pub translation: [f32; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f32; 4]>,
}

pub const STYLES: &[&str] = &["hills", "ridges", "dunes", "mesa", "flat"];

/// Most grid cells one terrain may have (1000 x 1000).
pub const MAX_CELLS: usize = 1_000_000;

impl Default for Terrain {
    fn default() -> Self {
        Terrain {
            size: [60.0, 60.0],
            resolution: 0.5,
            height: 6.0,
            scale: 20.0,
            octaves: 5,
            gain: 0.5,
            style: "hills".into(),
            seed: 1,
            base: -1.0,
            terraces: 5,
            wind: 0.0,
            edge_falloff: 0.0,
            flatten: Vec::new(),
            paths: Vec::new(),
            translation: [0.0; 3],
            rotation: None,
        }
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

impl Terrain {
    /// Grid cells along x and z.
    pub fn cells(&self) -> (usize, usize) {
        let n = |len: f32| ((len / self.resolution).ceil() as usize).max(1);
        (n(self.size[0]), n(self.size[1]))
    }

    pub fn validate(&self) -> Result<(), String> {
        let pos = |v: f32, f: &str| {
            if v.is_finite() && v > 0.0 {
                Ok(())
            } else {
                Err(format!("{f} must be > 0 (got {v})"))
            }
        };
        pos(self.size[0], "size[0] (width)")?;
        pos(self.size[1], "size[1] (depth)")?;
        pos(self.resolution, "resolution")?;
        pos(self.scale, "scale")?;
        if !self.height.is_finite() || self.height < 0.0 {
            return Err(format!("height must be >= 0 (got {})", self.height));
        }
        if !(1..=10).contains(&self.octaves) {
            return Err(format!("octaves must be 1..10 (got {})", self.octaves));
        }
        if !(0.0..=1.0).contains(&self.gain) {
            return Err(format!("gain must be 0..1 (got {})", self.gain));
        }
        if !STYLES.contains(&self.style.as_str()) {
            return Err(format!(
                "style: unknown `{}` ({})",
                self.style,
                STYLES.join(", ")
            ));
        }
        if self.terraces == 0 || self.terraces > 64 {
            return Err(format!("terraces must be 1..64 (got {})", self.terraces));
        }
        if self.edge_falloff < 0.0 {
            return Err("edge_falloff must be >= 0".into());
        }
        let (nx, nz) = self.cells();
        if nx * nz > MAX_CELLS {
            let min_res = ((self.size[0] * self.size[1]) / MAX_CELLS as f32).sqrt();
            return Err(format!(
                "{nx} x {nz} cells is too fine (max {MAX_CELLS}): use resolution >= {:.2}",
                min_res * 1.01
            ));
        }
        for (i, p) in self.flatten.iter().enumerate() {
            let r = p.size.map_or(p.radius, |s| s[0].min(s[1]));
            if r.is_nan() || r <= 0.0 || p.falloff < 0.0 {
                return Err(format!(
                    "flatten[{i}]: needs radius > 0 (or size [w, d]) and falloff >= 0"
                ));
            }
        }
        for (i, p) in self.paths.iter().enumerate() {
            if p.points.len() < 2 {
                return Err(format!("paths[{i}]: points needs at least 2 [x, z] points"));
            }
            if p.width.is_nan() || p.width <= 0.0 || p.falloff < 0.0 {
                return Err(format!("paths[{i}]: width must be > 0, falloff >= 0"));
            }
        }
        Ok(())
    }

    fn seed32(&self) -> u32 {
        (self.seed ^ (self.seed >> 32)) as u32
    }

    /// Signed fractal noise in about -1..1 (gradient fBm, lacunarity 2).
    fn fbm(&self, x: f32, z: f32, scale: f32, octaves: u32, seed: u32) -> f32 {
        let l = Lattice::new(scale, 0.0);
        let (mut sum, mut norm, mut amp, mut k) = (0.0, 0.0, 1.0, 1);
        for o in 0..octaves {
            let n = gradient([x, z, 0.0], 2, l.octave(k), seed.wrapping_add(o * 1013));
            sum += (2.0 * n - 1.0) * amp;
            norm += amp;
            amp *= self.gain;
            k *= 2;
        }
        sum / norm.max(1e-6)
    }

    /// The style's height in 0..1 at local (x, z), before pads and paths.
    fn shape(&self, x: f32, z: f32) -> f32 {
        let s = self.seed32();
        let sc = self.scale;
        // A gentle domain warp keeps the lattice from showing.
        let warp = |x: f32, z: f32| {
            let wx = self.fbm(x, z, sc * 1.7, 2, s ^ 0x5151);
            let wz = self.fbm(x + 31.7, z - 17.3, sc * 1.7, 2, s ^ 0xA2A2);
            (x + wx * sc * 0.35, z + wz * sc * 0.35)
        };
        match self.style.as_str() {
            "flat" => 0.0,
            "hills" => {
                let (wx, wz) = warp(x, z);
                let n = self.fbm(wx, wz, sc, self.octaves, s);
                // Broad valleys, rounder tops.
                let u = (0.5 + n * 1.25).clamp(0.0, 1.0);
                u * u * (1.6 - 0.6 * u)
            }
            "ridges" => {
                let (wx, wz) = warp(x, z);
                let l = Lattice::new(sc, 0.0);
                let (mut sum, mut norm, mut amp, mut k, mut w) = (0.0, 0.0, 1.0, 1, 1.0f32);
                for o in 0..self.octaves {
                    let n = gradient([wx, wz, 0.0], 2, l.octave(k), s.wrapping_add(o * 1013));
                    let r = 1.0 - (2.0 * n - 1.0).abs() * 1.6;
                    let r = r.clamp(0.0, 1.0);
                    let r = r * r * w;
                    // Fine crests ride on the big ones (ridged multifractal).
                    w = (r * 1.5).clamp(0.3, 1.0);
                    sum += r * amp;
                    norm += amp;
                    amp *= self.gain;
                    k *= 2;
                }
                let u = (sum / norm.max(1e-6) * 1.35).clamp(0.0, 1.0);
                u.powf(1.3)
            }
            "dunes" => {
                let a = self.wind.to_radians();
                let (ca, sa) = (a.cos(), a.sin());
                // Across-wind coordinate, bent by noise so crests meander.
                let along = x * ca - z * sa;
                let across = x * sa + z * ca;
                let bend = self.fbm(along, across, sc * 2.5, 2, s ^ 0x77) * sc * 0.6;
                let t = (across + bend) / sc;
                let ph = t - t.floor();
                // Long windward slope, short steep slip face.
                let p = if ph < 0.78 {
                    smoothstep(0.0, 0.78, ph)
                } else {
                    1.0 - smoothstep(0.78, 1.0, ph)
                };
                let amp = 0.55 + 0.45 * self.fbm(along, across, sc * 3.0, 2, s ^ 0x99);
                let ripple = self.fbm(x, z, sc * 0.25, self.octaves.min(3), s ^ 0x33) * 0.06;
                (p * amp.clamp(0.0, 1.0) + ripple + 0.05).clamp(0.0, 1.0)
            }
            "mesa" => {
                let (wx, wz) = warp(x, z);
                let n = self.fbm(wx, wz, sc, self.octaves, s);
                let u = (0.5 + n * 1.3).clamp(0.0, 1.0);
                // Terraces: flat treads, steep risers, a little rubble noise.
                let k = self.terraces as f32;
                let t = u * k;
                let step = t.floor();
                let f = t - step;
                let tread = (step + smoothstep(0.55, 0.85, f)) / k;
                let rubble = self.fbm(x, z, sc * 0.15, 2, s ^ 0x44) * 0.015;
                (tread + rubble).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    /// Ground height before pads and paths: the style, eased to 0 at the edges.
    fn raw(&self, x: f32, z: f32) -> f32 {
        let mut h = self.shape(x, z) * self.height;
        if self.edge_falloff > 0.0 {
            let [w, d] = self.size;
            let e = (w * 0.5 - x.abs()).min(d * 0.5 - z.abs());
            h *= smoothstep(0.0, self.edge_falloff, e);
        }
        h
    }

    /// Ground height after paths (pads not yet applied).
    fn carved(&self, x: f32, z: f32) -> f32 {
        let mut h = self.raw(x, z);
        for p in &self.paths {
            let (dist, cx, cz, tx, tz) = nearest_on(&p.points, x, z);
            let half = p.width * 0.5;
            let w = 1.0 - smoothstep(half, half + p.falloff, dist);
            if w <= 0.0 {
                continue;
            }
            // Bed level from the centreline, averaged lengthwise so it runs smooth.
            let mut bed = 0.0;
            for k in -2..=2 {
                let s = k as f32 * p.width * 0.5;
                bed += self.raw(cx + tx * s, cz + tz * s);
            }
            let bed = bed / 5.0 - p.depth;
            h = lerp(h, bed, w);
        }
        h
    }

    /// The final top height at local (x, z) (before translation).
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        let mut h = self.carved(x, z);
        for p in &self.flatten {
            let target = p.height.unwrap_or_else(|| self.carved(p.at[0], p.at[1]));
            let (dx, dz) = (x - p.at[0], z - p.at[1]);
            let dist = match p.size {
                Some([w, d]) => {
                    let qx = dx.abs() - w * 0.5;
                    let qz = dz.abs() - d * 0.5;
                    (qx.max(0.0).powi(2) + qz.max(0.0).powi(2)).sqrt()
                }
                None => (dx * dx + dz * dz).sqrt() - p.radius,
            };
            let w = 1.0 - smoothstep(0.0, p.falloff, dist);
            h = lerp(h, target, w);
        }
        h
    }

    /// Builds the closed solid.
    pub fn build(&self, name: &str) -> Result<Part, String> {
        self.validate()?;
        let (nx, nz) = self.cells();
        let [w, d] = self.size;
        let (dx, dz) = (w / nx as f32, d / nz as f32);
        let xs: Vec<f32> = (0..=nx).map(|i| -w * 0.5 + i as f32 * dx).collect();
        let zs: Vec<f32> = (0..=nz).map(|j| -d * 0.5 + j as f32 * dz).collect();
        let row = nx + 1;
        let idx = |i: usize, j: usize| j * row + i;
        // Heights, row by row in parallel.
        use rayon::prelude::*;
        let hs: Vec<f32> = (0..=nz)
            .into_par_iter()
            .flat_map_iter(|j| {
                let z = zs[j];
                xs.iter().map(move |&x| self.height_at(x, z))
            })
            .collect();
        let (mut lo, mut lo_at) = (f32::INFINITY, 0);
        for (k, &h) in hs.iter().enumerate() {
            if !h.is_finite() {
                return Err("terrain height is not finite (check the numbers)".into());
            }
            if h < lo {
                lo = h;
                lo_at = k;
            }
        }
        if lo <= self.base + 1e-3 {
            let (i, j) = (lo_at % row, lo_at / row);
            return Err(format!(
                "the ground dips to y = {lo:.2} at [{:.1}, {:.1}], at or below base {}: lower `base` (e.g. {:.1}) or carve less deep",
                xs[i],
                zs[j],
                self.base,
                (lo - 0.5).floor()
            ));
        }

        let tris = 2 * nx * nz + 4 * (nx + nz) * 3;
        let mut part = Part::new(name);
        part.verts.reserve(hs.len() + 6 * (nx + nz) + 1);
        part.indices.reserve(tris * 3);

        // Top: smooth normals by central differences.
        for j in 0..=nz {
            for i in 0..=nx {
                let h = hs[idx(i, j)];
                let (i0, i1) = (i.saturating_sub(1), (i + 1).min(nx));
                let (j0, j1) = (j.saturating_sub(1), (j + 1).min(nz));
                let gx = (hs[idx(i1, j)] - hs[idx(i0, j)]) / (xs[i1] - xs[i0]);
                let gz = (hs[idx(i, j1)] - hs[idx(i, j0)]) / (zs[j1] - zs[j0]);
                let n = norm([-gx, 1.0, -gz]);
                part.verts.push([xs[i], h, zs[j]]);
                part.normals.push(n);
                part.uvs.push([xs[i], zs[j]]);
            }
        }
        for j in 0..nz {
            for i in 0..nx {
                let (a, b, c, e) = (idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1));
                // Split along the shorter diagonal-ish: alternate for symmetry.
                let q = [a, e, c, b].map(|v| v as u32);
                if (i + j) % 2 == 0 {
                    part.indices.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
                } else {
                    part.indices.extend([q[0], q[1], q[3], q[1], q[2], q[3]]);
                }
            }
        }

        // The boundary ring, counter-clockwise seen from above.
        let mut ring: Vec<(usize, usize)> = Vec::with_capacity(2 * (nx + nz));
        ring.extend((0..nx).map(|i| (i, 0)));
        ring.extend((0..nz).map(|j| (nx, j)));
        ring.extend((1..=nx).rev().map(|i| (i, nz)));
        ring.extend((1..=nz).rev().map(|j| (0, j)));
        let center = part.verts.len() as u32;
        part.verts.push([0.0, self.base, 0.0]);
        part.normals.push([0.0, -1.0, 0.0]);
        part.uvs
            .push(crate::uv::metric_uv(1, -1.0, bevy::math::Vec3::ZERO));
        let b = self.base;
        for k in 0..ring.len() {
            let (ia, ja) = ring[k];
            let (ib, jb) = ring[(k + 1) % ring.len()];
            let ta = [xs[ia], hs[idx(ia, ja)], zs[ja]];
            let tb = [xs[ib], hs[idx(ib, jb)], zs[jb]];
            let (ba, bb) = ([ta[0], b, ta[2]], [tb[0], b, tb[2]]);
            // Outward: the edge's side of the rectangle.
            let (axis, sign, out) = if ja == jb && ja == 0 {
                (2u8, -1.0, [0.0, 0.0, -1.0])
            } else if ja == jb {
                (2, 1.0, [0.0, 0.0, 1.0])
            } else if ia == nx {
                (0, 1.0, [1.0, 0.0, 0.0])
            } else {
                (0, -1.0, [-1.0, 0.0, 0.0])
            };
            let s = part.verts.len() as u32;
            for p in [ta, tb, bb, ba] {
                part.verts.push(p);
                part.normals.push(out);
                part.uvs.push(crate::uv::metric_uv(axis, sign, p.into()));
            }
            push_quad(&mut part, [s, s + 1, s + 2, s + 3], out);
            // Bottom fan.
            let s = part.verts.len() as u32;
            for p in [ba, bb] {
                part.verts.push(p);
                part.normals.push([0.0, -1.0, 0.0]);
                part.uvs.push(crate::uv::metric_uv(1, -1.0, p.into()));
            }
            push_tri(&mut part, [center, s, s + 1], [0.0, -1.0, 0.0]);
        }
        part.translation = self.translation;
        if let Some(r) = self.rotation {
            part.rotation = r;
        }
        Ok(part)
    }
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    v.map(|c| c / l)
}

fn face_normal(p: &Part, t: [u32; 3]) -> [f32; 3] {
    let [a, b, c] = t.map(|i| p.verts[i as usize]);
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

/// A triangle wound so its face normal points along `out`.
fn push_tri(p: &mut Part, t: [u32; 3], out: [f32; 3]) {
    let n = face_normal(p, t);
    if n[0] * out[0] + n[1] * out[1] + n[2] * out[2] >= 0.0 {
        p.indices.extend(t);
    } else {
        p.indices.extend([t[0], t[2], t[1]]);
    }
}

/// A planar quad a-b-c-d (in ring order) wound to face `out`.
fn push_quad(p: &mut Part, q: [u32; 4], out: [f32; 3]) {
    let n1 = face_normal(p, [q[0], q[1], q[2]]);
    let flip = n1[0] * out[0] + n1[1] * out[1] + n1[2] * out[2] < 0.0;
    if flip {
        p.indices.extend([q[0], q[2], q[1], q[0], q[3], q[2]]);
    } else {
        p.indices.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
    }
}

/// Distance from (x, z) to the polyline, the nearest point on it and the
/// unit tangent there.
fn nearest_on(pts: &[[f32; 2]], x: f32, z: f32) -> (f32, f32, f32, f32, f32) {
    let mut best = (f32::INFINITY, x, z, 1.0, 0.0);
    for s in pts.windows(2) {
        let ([ax, az], [bx, bz]) = (s[0], s[1]);
        let (ex, ez) = (bx - ax, bz - az);
        let len2 = ex * ex + ez * ez;
        let t = if len2 > 0.0 {
            (((x - ax) * ex + (z - az) * ez) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (cx, cz) = (ax + ex * t, az + ez * t);
        let dist = ((x - cx).powi(2) + (z - cz).powi(2)).sqrt();
        if dist < best.0 {
            let l = len2.sqrt().max(1e-9);
            best = (dist, cx, cz, ex / l, ez / l);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Every directed edge (by welded position) once, its reverse once, and
    /// a positive signed volume: closed and outward-wound.
    fn closed_outward(p: &Part) -> f64 {
        let key = |v: [f32; 3]| v.map(|c| (c * 1e5).round() as i64);
        let mut edges: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
        let mut vol = 0.0f64;
        for t in p.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| p.verts[i as usize]);
            for (u, v) in [(a, b), (b, c), (c, a)] {
                *edges.entry((key(u), key(v))).or_default() += 1;
            }
            let [a, b, c] = [a, b, c].map(|v| v.map(f64::from));
            vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0;
        }
        for ((u, v), n) in &edges {
            assert_eq!(*n, 1, "edge {u:?}->{v:?} used {n} times");
            assert_eq!(edges.get(&(*v, *u)), Some(&1), "edge {u:?}->{v:?} unpaired");
        }
        vol
    }

    fn bounds(p: &Part) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for v in &p.verts {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        (lo, hi)
    }

    #[test]
    fn every_style_is_closed_outward_and_in_bounds() {
        for style in STYLES {
            let t = Terrain {
                size: [30.0, 20.0],
                resolution: 0.5,
                style: (*style).into(),
                ..Default::default()
            };
            let p = t.build("g").unwrap_or_else(|e| panic!("{style}: {e}"));
            let vol = closed_outward(&p);
            assert!(vol > 0.0, "{style}: volume {vol}");
            let (lo, hi) = bounds(&p);
            assert!((lo[0] + 15.0).abs() < 1e-4 && (hi[0] - 15.0).abs() < 1e-4);
            assert!((lo[2] + 10.0).abs() < 1e-4 && (hi[2] - 10.0).abs() < 1e-4);
            assert!((lo[1] + 1.0).abs() < 1e-5, "{style}: bottom {}", lo[1]);
            assert!(hi[1] <= 6.0 + 1e-3, "{style}: top {}", hi[1]);
            if *style != "flat" {
                assert!(hi[1] > 1.5, "{style}: too little relief, top {}", hi[1]);
            }
            assert_eq!(p.verts.len(), p.normals.len());
            assert_eq!(p.verts.len(), p.uvs.len());
        }
    }

    #[test]
    fn flatten_pad_is_level_at_its_height() {
        let t = Terrain {
            flatten: vec![Pad {
                at: [5.0, -3.0],
                radius: 4.0,
                size: None,
                height: Some(2.5),
                falloff: 3.0,
            }],
            ..Default::default()
        };
        for (x, z) in [(5.0, -3.0), (8.9, -3.0), (5.0, 0.9), (2.2, -5.5)] {
            assert!((t.height_at(x, z) - 2.5).abs() < 1e-5, "at {x},{z}");
        }
        // Default height: the ground's own at the centre.
        let mut t2 = t.clone();
        t2.flatten[0].height = None;
        let h0 = t2.carved(5.0, -3.0);
        assert!((t2.height_at(7.0, -1.0) - h0).abs() < 1e-5);
        // A rectangle pad.
        let mut t3 = t.clone();
        t3.flatten[0].size = Some([8.0, 2.0]);
        assert!((t3.height_at(8.9, -2.1) - 2.5).abs() < 1e-5);
        t3.build("g").unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn paths_carve_below_the_banks() {
        let t = Terrain {
            paths: vec![Path {
                points: vec![[-30.0, 0.0], [30.0, 4.0]],
                width: 3.0,
                depth: 0.5,
                falloff: 2.0,
            }],
            ..Default::default()
        };
        let plain = Terrain::default();
        let (a, b) = (t.height_at(0.0, 2.0), plain.height_at(0.0, 2.0));
        assert!(a < b - 0.2, "carved {a} vs {b}");
        // Far from the path nothing changes.
        assert_eq!(t.height_at(0.0, 20.0), plain.height_at(0.0, 20.0));
        let p = t.build("g").unwrap_or_else(|e| panic!("{e}"));
        assert!(closed_outward(&p) > 0.0);
    }

    #[test]
    fn deterministic_per_seed_and_extends_without_moving() {
        let a = Terrain::default()
            .build("g")
            .unwrap_or_else(|e| panic!("{e}"));
        let b = Terrain::default()
            .build("g")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.verts, b.verts);
        let c = Terrain {
            seed: 2,
            ..Default::default()
        };
        assert_ne!(
            c.height_at(3.0, 4.0),
            Terrain::default().height_at(3.0, 4.0)
        );
        let big = Terrain {
            size: [200.0, 200.0],
            ..Default::default()
        };
        assert_eq!(
            big.height_at(3.0, 4.0),
            Terrain::default().height_at(3.0, 4.0)
        );
    }

    #[test]
    fn errors_name_the_fix() {
        let deep = Terrain {
            style: "flat".into(),
            base: -0.2,
            paths: vec![Path {
                points: vec![[-10.0, 0.0], [10.0, 0.0]],
                width: 2.0,
                depth: 1.0,
                falloff: 1.0,
            }],
            ..Default::default()
        };
        let e = deep.build("g").err().unwrap_or_default();
        assert!(e.contains("lower `base`"), "{e}");
        let fine = Terrain {
            size: [2000.0, 2000.0],
            resolution: 0.5,
            ..Default::default()
        };
        let e = fine.validate().err().unwrap_or_default();
        assert!(e.contains("resolution >="), "{e}");
        let bad = Terrain {
            style: "lava".into(),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn fast_at_120_cells() {
        let t0 = std::time::Instant::now();
        let p = Terrain::default()
            .build("g")
            .unwrap_or_else(|e| panic!("{e}"));
        let ms = t0.elapsed().as_millis();
        assert_eq!(p.tri_count(), 2 * 120 * 120 + 3 * 2 * 240);
        // Debug builds are slow; this only catches something pathological.
        assert!(ms < 3000, "{ms} ms");
    }
}
