//! A procedural photo-studio environment: a grey cyclorama (brighter
//! overhead, a dark floor below the horizon) lit by four soft boxes. It is an
//! analytic function of direction, so the preview's GPU cubemap and the CPU
//! material sphere (`material_view`, the M pane) see the same light, and a
//! roughness blur is just wider, dimmer boxes (energy kept).
//!
//! Radiance is linear HDR: the boxes run to ~7 so glossy parts show a real
//! highlight, the cyclorama sits near 0.3. World axes: y up, front = +z.

/// (direction it shines from, angular radius in radians, linear radiance)
const BOXES: [([f32; 3], f32, [f32; 3]); 4] = [
    // Key: a big box high front-left.
    ([-0.45, 0.75, 0.5], 0.34, [7.0, 6.8, 6.4]),
    // Overhead scrim.
    ([0.0, 1.0, 0.0], 0.5, [2.4, 2.4, 2.5]),
    // Rim: a smaller, cooler box behind right, lights silhouettes.
    ([0.75, 0.35, -0.55], 0.22, [5.0, 5.2, 5.6]),
    // Fill: a broad dim card front-right.
    ([0.85, 0.15, 0.5], 0.45, [1.2, 1.2, 1.25]),
];

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
    v.map(|c| c / l)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Radiance arriving from direction `d` (need not be unit), blurred by
/// `blur` radians (0 = sharp; the roughness lobe width for reflections).
pub fn radiance(d: [f32; 3], blur: f32) -> [f32; 3] {
    let d = norm(d);
    let b = blur.max(0.0);
    // Cyclorama: horizon grey rising to a brighter top, a dark warm floor.
    let w = 0.06 + b * 0.6;
    let sky_t = d[1].max(0.0);
    let sky = [0, 1, 2]
        .map(|c| [0.28, 0.29, 0.31][c] + ([0.42, 0.43, 0.47][c] - [0.28, 0.29, 0.31][c]) * sky_t);
    let floor = [0, 1, 2].map(|c| [0.10, 0.095, 0.09][c] * (1.0 + d[1]).max(0.4));
    let k = smoothstep(-w, w, d[1]);
    let mut out = [0, 1, 2].map(|c| floor[c] + (sky[c] - floor[c]) * k);
    for (dir, r, col) in BOXES {
        let dir = norm(dir);
        let cos = (d[0] * dir[0] + d[1] * dir[1] + d[2] * dir[2]).clamp(-1.0, 1.0);
        let a = cos.acos();
        // Blur widens the box and dims it by the area ratio.
        let rb = (r * r + b * b).sqrt();
        let e = 0.03 + b * 0.5;
        let m = 1.0 - smoothstep(rb - e, rb + e, a);
        if m > 0.0 {
            let s = (r / rb).powi(2) * m;
            for c in 0..3 {
                out[c] += col[c] * s;
            }
        }
    }
    out
}

/// Cosine-weighted irradiance / pi (what a diffuse surface facing `n`
/// reflects for albedo 1), approximated by a very wide blur.
pub fn irradiance(n: [f32; 3]) -> [f32; 3] {
    radiance(n, 1.3)
}

/// The blur that approximates a GGX lobe of this perceptual roughness.
pub fn roughness_blur(rough: f32) -> f32 {
    let a = rough.clamp(0.0, 1.0);
    a * a * 1.2
}

/// The world direction of a cubemap texel: face order +x, -x, +y, -y, +z,
/// -z; (s, t) in -1..1 with t down the image (the WebGPU / Bevy layout).
/// Bevy samples cubemaps with z flipped, so the world direction is the cube
/// direction with z negated.
pub fn cube_dir(face: usize, s: f32, t: f32) -> [f32; 3] {
    let c = match face {
        0 => [1.0, -t, -s],
        1 => [-1.0, -t, s],
        2 => [s, 1.0, t],
        3 => [s, -1.0, -t],
        4 => [s, -t, 1.0],
        _ => [-s, -t, -1.0],
    };
    [c[0], c[1], -c[2]]
}

/// IEEE half-float bits (non-negative, finite inputs; tiny values flush to 0).
fn f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let m = b & 0x7f_ffff;
    if e <= 0 {
        return sign;
    }
    if e >= 31 {
        return sign | 0x7c00;
    }
    sign | ((e as u16) << 10) | ((m >> 13) as u16)
}

/// The studio as an Rgba16Float cubemap (6 square faces of `size`,
/// concatenated in face order), ready for a GPU-filtered environment light.
pub fn cubemap_rgba16f(size: usize) -> Vec<u8> {
    use rayon::prelude::*;
    let faces: Vec<Vec<u8>> = (0..6)
        .into_par_iter()
        .map(|f| {
            let mut px = Vec::with_capacity(size * size * 8);
            for y in 0..size {
                for x in 0..size {
                    let s = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                    let t = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                    let c = radiance(cube_dir(f, s, t), 0.0);
                    for v in [c[0], c[1], c[2], 1.0] {
                        px.extend_from_slice(&f16_bits(v).to_le_bytes());
                    }
                }
            }
            px
        })
        .collect();
    faces.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_is_brighter_above_and_blur_keeps_energy() {
        let up = radiance([0.0, 1.0, 0.0], 0.0);
        let down = radiance([0.0, -1.0, 0.0], 0.0);
        assert!(up[1] > down[1] * 5.0, "{up:?} {down:?}");
        // The key box is a highlight well above the cyclorama.
        let key = radiance([-0.45, 0.75, 0.5], 0.0);
        assert!(key[0] > 5.0, "{key:?}");
        // A blurred box is dimmer at its centre but spreads wider.
        let blurred = radiance([-0.45, 0.75, 0.5], 0.6);
        assert!(blurred[0] < key[0]);
        let off = norm([-0.45, 0.75, 0.5]);
        let side = norm([off[0] + 0.35, off[1] - 0.2, off[2] + 0.3]);
        assert!(radiance(side, 0.6)[0] > radiance(side, 0.0)[0]);
    }

    #[test]
    fn cube_faces_cover_their_axes() {
        // Face centres point along their axis (z flipped for Bevy).
        assert_eq!(cube_dir(0, 0.0, 0.0), [1.0, 0.0, 0.0]);
        assert_eq!(cube_dir(2, 0.0, 0.0), [0.0, 1.0, 0.0]);
        assert_eq!(cube_dir(4, 0.0, 0.0), [0.0, 0.0, -1.0]);
        let data = cubemap_rgba16f(8);
        assert_eq!(data.len(), 8 * 8 * 6 * 8);
        // 1.0 in half precision.
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(0.5), 0x3800);
    }
}
