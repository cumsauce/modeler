//! Seeded lattice noise in 2D and 3D: value, gradient (Perlin-style) and
//! Worley/cellular, each optionally periodic so a tile-domain texture repeats
//! with no seam. Pure functions of (point, seed, period): results never depend
//! on evaluation order or thread count.

/// Integer hash (a murmur3-style finalizer over the mixed coordinates).
#[inline]
pub fn hash3(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    let mut h = seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add((x as u32).wrapping_mul(0x85EB_CA6B))
        .wrapping_add((y as u32).wrapping_mul(0xC2B2_AE35))
        .wrapping_add((z as u32).wrapping_mul(0x27D4_EB2F));
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    h
}

/// A hash as a float in [0, 1).
#[inline]
pub fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / 16_777_216.0
}

/// Lattice cells along each axis: `period` > 0 wraps the lattice (cell ids
/// taken modulo it), 0 means unbounded.
#[derive(Clone, Copy, Debug)]
pub struct Lattice {
    /// Cell size in the coordinates' units (meters).
    pub cell: f32,
    /// Cells per period on x / y / z (0 = no wrap).
    pub period: [i32; 3],
}

impl Lattice {
    /// A lattice of roughly `size`-sized cells. With a tile period `tile`
    /// (> 0) the cell is adjusted so a whole number of cells fits the tile on
    /// x and y, and the lattice wraps there; z (unused by 2D coords) never wraps.
    pub fn new(size: f32, tile: f32) -> Self {
        let size = size.max(1e-6);
        if tile > 0.0 {
            let n = (tile / size).round().max(1.0);
            Lattice {
                cell: tile / n,
                period: [n as i32, n as i32, 0],
            }
        } else {
            Lattice {
                cell: size,
                period: [0; 3],
            }
        }
    }

    /// The same lattice `k` times finer (octaves), periods scaled to match.
    pub fn octave(self, k: i32) -> Self {
        Lattice {
            cell: self.cell / k as f32,
            period: self.period.map(|p| p * k),
        }
    }

    #[inline]
    fn wrap(&self, i: i32, axis: usize) -> i32 {
        let p = self.period[axis];
        if p > 0 { i.rem_euclid(p) } else { i }
    }
}

#[inline]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Value noise in [0, 1].
pub fn value(p: [f32; 3], dims: usize, l: Lattice, seed: u32) -> f32 {
    let q = p.map(|c| c / l.cell);
    let i = q.map(|c| c.floor() as i32);
    let f = [q[0] - i[0] as f32, q[1] - i[1] as f32, q[2] - i[2] as f32];
    let h = |dx: i32, dy: i32, dz: i32| {
        unit(hash3(
            l.wrap(i[0] + dx, 0),
            l.wrap(i[1] + dy, 1),
            if dims == 3 { l.wrap(i[2] + dz, 2) } else { 0 },
            seed,
        ))
    };
    let (u, v) = (fade(f[0]), fade(f[1]));
    let layer = |dz: i32| {
        lerp(
            lerp(h(0, 0, dz), h(1, 0, dz), u),
            lerp(h(0, 1, dz), h(1, 1, dz), u),
            v,
        )
    };
    if dims == 3 {
        lerp(layer(0), layer(1), fade(f[2]))
    } else {
        layer(0)
    }
}

/// Gradient (Perlin-style) noise mapped to roughly [0, 1] (0.5 = zero).
pub fn gradient(p: [f32; 3], dims: usize, l: Lattice, seed: u32) -> f32 {
    let q = p.map(|c| c / l.cell);
    let i = q.map(|c| c.floor() as i32);
    let f = [q[0] - i[0] as f32, q[1] - i[1] as f32, q[2] - i[2] as f32];
    let g = |dx: i32, dy: i32, dz: i32| -> f32 {
        let h = hash3(
            l.wrap(i[0] + dx, 0),
            l.wrap(i[1] + dy, 1),
            if dims == 3 { l.wrap(i[2] + dz, 2) } else { 0 },
            seed,
        );
        let (x, y, z) = (f[0] - dx as f32, f[1] - dy as f32, f[2] - dz as f32);
        if dims == 3 {
            // One of the 12 cube-edge gradients.
            match h % 12 {
                0 => x + y,
                1 => -x + y,
                2 => x - y,
                3 => -x - y,
                4 => x + z,
                5 => -x + z,
                6 => x - z,
                7 => -x - z,
                8 => y + z,
                9 => -y + z,
                10 => y - z,
                _ => -y - z,
            }
        } else {
            let a = unit(h) * std::f32::consts::TAU;
            a.cos() * x + a.sin() * y
        }
    };
    let (u, v) = (fade(f[0]), fade(f[1]));
    let layer = |dz: i32| {
        lerp(
            lerp(g(0, 0, dz), g(1, 0, dz), u),
            lerp(g(0, 1, dz), g(1, 1, dz), u),
            v,
        )
    };
    let n = if dims == 3 {
        lerp(layer(0), layer(1), fade(f[2])) * 0.75
    } else {
        layer(0)
    };
    (0.5 + n * std::f32::consts::FRAC_1_SQRT_2).clamp(0.0, 1.0)
}

/// Worley / cellular noise: distances (in the coordinates' units) to the
/// nearest and second-nearest feature point, and the nearest cell's id.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub f1: f32,
    pub f2: f32,
    pub id: u32,
}

pub fn worley(p: [f32; 3], dims: usize, l: Lattice, seed: u32, jitter: f32) -> Cell {
    let q = p.map(|c| c / l.cell);
    let i = q.map(|c| c.floor() as i32);
    let (mut f1, mut f2, mut id) = (f32::MAX, f32::MAX, 0u32);
    let zr = if dims == 3 { -1..=1 } else { 0..=0 };
    for dz in zr {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let c = [i[0] + dx, i[1] + dy, i[2] + dz];
                let w = [
                    l.wrap(c[0], 0),
                    l.wrap(c[1], 1),
                    if dims == 3 { l.wrap(c[2], 2) } else { 0 },
                ];
                let h = hash3(w[0], w[1], w[2], seed);
                let o = [
                    0.5 + (unit(h) - 0.5) * jitter,
                    0.5 + (unit(hash3(w[0], w[1], w[2], seed ^ 0xA5A5_A5A5)) - 0.5) * jitter,
                    if dims == 3 {
                        0.5 + (unit(hash3(w[0], w[1], w[2], seed ^ 0x5A5A_5A5A)) - 0.5) * jitter
                    } else {
                        0.0
                    },
                ];
                let d = [
                    c[0] as f32 + o[0] - q[0],
                    c[1] as f32 + o[1] - q[1],
                    if dims == 3 {
                        c[2] as f32 + o[2] - q[2]
                    } else {
                        0.0
                    },
                ];
                let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if dist < f1 {
                    f2 = f1;
                    f1 = dist;
                    id = h;
                } else if dist < f2 {
                    f2 = dist;
                }
            }
        }
    }
    Cell {
        f1: f1 * l.cell,
        f2: f2 * l.cell,
        id,
    }
}

/// Fractal sum of `octaves` layers of `base`, each `lacunarity` (rounded to
/// a whole number so periods stay whole) times finer and `gain` times weaker.
/// `mode`: "fbm" (plain sum), "ridged" (sharp creases), "turbulence" (|n|).
/// Result in [0, 1].
pub fn fractal(
    base: impl Fn([f32; 3], Lattice, u32) -> f32,
    p: [f32; 3],
    l: Lattice,
    seed: u32,
    octaves: u32,
    lacunarity: f32,
    gain: f32,
    mode: &str,
) -> f32 {
    let lac = lacunarity.round().max(1.0) as i32;
    let (mut sum, mut norm, mut amp, mut k) = (0.0, 0.0, 1.0, 1);
    for o in 0..octaves.max(1) {
        let n = base(p, l.octave(k), seed.wrapping_add(o * 1013));
        let v = match mode {
            "ridged" => {
                let r = 1.0 - (2.0 * n - 1.0).abs();
                r * r
            }
            "turbulence" => (2.0 * n - 1.0).abs(),
            _ => n,
        };
        sum += v * amp;
        norm += amp;
        amp *= gain;
        k *= lac;
    }
    (sum / norm).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_noise_repeats_exactly_across_the_tile() {
        let l = Lattice::new(0.13, 1.0);
        for k in 0..50 {
            let y = k as f32 * 0.0191;
            for (a, b) in [
                (value([0.0, y, 0.0], 2, l, 7), value([1.0, y, 0.0], 2, l, 7)),
                (
                    gradient([0.3, y, 0.0], 2, l, 7),
                    gradient([1.3, y, 0.0], 2, l, 7),
                ),
                (
                    worley([y, 0.25, 0.0], 2, l, 7, 1.0).f1,
                    worley([y, 1.25, 0.0], 2, l, 7, 1.0).f1,
                ),
            ] {
                assert!((a - b).abs() < 1e-5, "{a} vs {b} at y {y}");
            }
        }
        // Whole number of cells per tile.
        assert!((1.0 / l.cell - (1.0f32 / l.cell).round()).abs() < 1e-4);
    }

    #[test]
    fn noise_is_in_range_and_seeded() {
        let l = Lattice::new(0.1, 0.0);
        let mut differ = false;
        for k in 0..200 {
            let p = [k as f32 * 0.037, k as f32 * 0.011, k as f32 * 0.023];
            for n in [
                value(p, 3, l, 1),
                gradient(p, 3, l, 1),
                gradient(p, 2, l, 1),
            ] {
                assert!((0.0..=1.0).contains(&n), "{n}");
            }
            differ |= (value(p, 3, l, 1) - value(p, 3, l, 2)).abs() > 1e-3;
        }
        assert!(differ, "seed changes the noise");
    }
}
