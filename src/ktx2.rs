//! KTX2 texture files for glTF exports: GPU block-compressed (BC7 colour,
//! BC5 normals, BC4 single channels) with a full mip chain, zstd
//! supercompressed. Bevy's default 3D features (`ktx2`, `zstd_rust`) load
//! them as they are: the GPU keeps them compressed (1 byte per texel or
//! less, against 4 for a decoded PNG) and needs no mip generation.
//!
//! Encoding is deterministic, so a file's content hash names it and the
//! same bake from any model is one file.

use texture_compressor::{Effort, Format, ImageView, Settings};

/// What a texture holds, which picks its block format and mip filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// sRGB colour + alpha (base colour, emission): BC7, sRGB.
    Srgb,
    /// Linear RGB(A) data (metallic-roughness, packed ORM): BC7.
    Linear,
    /// A tangent-space normal map, (n + 1) / 2: BC5 (x, y; Bevy rebuilds z).
    Normal,
    /// One linear channel in R (occlusion): BC4.
    Gray,
}

impl Kind {
    fn format(self) -> (Format, u32, u8, u8, usize) {
        // (encoder format, vkFormat, DFD colour model, transfer, bytes/block)
        match self {
            Kind::Srgb => (Format::Bc7, 146, 134, 2, 16),
            Kind::Linear => (Format::Bc7, 145, 134, 1, 16),
            Kind::Normal => (Format::Bc5, 141, 132, 1, 16),
            Kind::Gray => (Format::Bc4, 139, 131, 1, 8),
        }
    }
}

fn to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

/// The next mip down: a 2x2 box filter (wrapping, so a tiling texture
/// stays seamless at every level), in linear light for sRGB colour and
/// renormalised for normals.
fn downsample(px: &[u8], w: usize, h: usize, kind: Kind) -> (Vec<u8>, usize, usize) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0f32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let sx = (x * 2 + dx) % w;
                let sy = (y * 2 + dy) % h;
                let p = &px[(sy * w + sx) * 4..][..4];
                for c in 0..4 {
                    acc[c] += if kind == Kind::Srgb && c < 3 {
                        to_linear(p[c])
                    } else {
                        p[c] as f32 / 255.0
                    };
                }
            }
            let mut v = acc.map(|a| a / 4.0);
            if kind == Kind::Normal {
                let n = [v[0] * 2.0 - 1.0, v[1] * 2.0 - 1.0, v[2] * 2.0 - 1.0];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
                for c in 0..3 {
                    v[c] = n[c] / len * 0.5 + 0.5;
                }
            }
            let o = &mut out[(y * nw + x) * 4..][..4];
            for c in 0..4 {
                o[c] = if kind == Kind::Srgb && c < 3 {
                    to_srgb(v[c])
                } else {
                    (v[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8
                };
            }
        }
    }
    (out, nw, nh)
}

/// Encodes an RGBA8 image as a KTX2 file: every mip level down to 1x1,
/// block-compressed, zstd supercompressed.
pub fn encode(rgba: &[u8], w: usize, h: usize, kind: Kind) -> Result<Vec<u8>, String> {
    encode_with(rgba, w, h, kind, None)
}

/// [`encode`], keeping a cutout's alpha coverage at every mip level when
/// `cutoff` is given (leaves stay leafy at a distance).
pub fn encode_with(
    rgba: &[u8],
    w: usize,
    h: usize,
    kind: Kind,
    cutoff: Option<f32>,
) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return Err(format!("ktx2: bad image {w}x{h} ({} bytes)", rgba.len()));
    }
    // GPUs (wgpu) refuse a block-compressed texture whose base size is not
    // a whole number of 4x4 blocks: stretch such an image to the next
    // multiple of 4 (uvs are normalised, so the mapping is unchanged).
    let (rgba, w, h) = if !w.is_multiple_of(4) || !h.is_multiple_of(4) {
        let (nw, nh) = (w.next_multiple_of(4), h.next_multiple_of(4));
        let mut out = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            let sy = (y * h / nh).min(h - 1);
            for x in 0..nw {
                let sx = (x * w / nw).min(w - 1);
                out[(y * nw + x) * 4..][..4].copy_from_slice(&rgba[(sy * w + sx) * 4..][..4]);
            }
        }
        (std::borrow::Cow::Owned(out), nw, nh)
    } else {
        (std::borrow::Cow::Borrowed(rgba), w, h)
    };
    let (format, vk, model, transfer, block_bytes) = kind.format();
    let settings = Settings::new(format, Effort::Balanced);
    // levels[i] = (zstd bytes, uncompressed length), level 0 = full size.
    let mut levels: Vec<(Vec<u8>, usize)> = Vec::new();
    let (mut px, mut lw, mut lh) = (rgba.to_vec(), w, h);
    let target = cutoff.map(|c| {
        let k = (c * 255.0) as u8;
        px.as_chunks::<4>().0.iter().filter(|p| p[3] > k).count() as f32
            / (px.len() / 4).max(1) as f32
    });
    loop {
        let view = ImageView::rgba8(&px, lw as u32, lh as u32, lw * 4)
            .map_err(|e| format!("ktx2: {e:?}"))?;
        let blocks = texture_compressor::encode_parallel(view, &settings)
            .map_err(|e| format!("ktx2: encoding {lw}x{lh}: {e:?}"))?;
        let z = ruzstd::encoding::compress_to_vec(
            blocks.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        levels.push((z, blocks.len()));
        if lw == 1 && lh == 1 {
            break;
        }
        (px, lw, lh) = downsample(&px, lw, lh, kind);
        if let (Some(c), Some(t)) = (cutoff, target) {
            crate::material::pbr::keep_coverage(&mut px, c, t);
        }
    }
    debug_assert_eq!(levels[0].1, w.div_ceil(4) * h.div_ceil(4) * block_bytes);

    // Data format descriptor: one basic block, a sample per channel.
    let samples: &[(u16, u8, u8)] = match kind {
        Kind::Normal => &[(0, 63, 0), (64, 63, 1)],
        Kind::Gray => &[(0, 63, 0)],
        _ => &[(0, 127, 0)],
    };
    let block_size = 24 + 16 * samples.len();
    let mut dfd = Vec::new();
    dfd.extend_from_slice(&((4 + block_size) as u32).to_le_bytes());
    dfd.extend_from_slice(&0u32.to_le_bytes()); // vendor 0, type 0
    dfd.extend_from_slice(&2u16.to_le_bytes()); // version 2
    dfd.extend_from_slice(&(block_size as u16).to_le_bytes());
    dfd.extend_from_slice(&[model, 1, transfer, 0]); // BT.709 primaries
    dfd.extend_from_slice(&[3, 3, 0, 0]); // 4x4 blocks
    dfd.extend_from_slice(&[0; 8]); // bytesPlane: 0 when supercompressed
    for &(offset, bits, channel) in samples {
        dfd.extend_from_slice(&offset.to_le_bytes());
        dfd.extend_from_slice(&[bits, channel]);
        dfd.extend_from_slice(&[0; 4]); // sample position
        dfd.extend_from_slice(&0u32.to_le_bytes());
        dfd.extend_from_slice(&u32::MAX.to_le_bytes());
    }

    let n = levels.len();
    let header = 12 + 9 * 4 + 4 * 4 + 2 * 8;
    let dfd_at = header + n * 24;
    let data_at = dfd_at + dfd.len();
    let mut out = Vec::new();
    out.extend_from_slice(&[
        0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
    ]);
    for v in [vk, 1, w as u32, h as u32, 0, 0, 1, n as u32, 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in [dfd_at as u32, dfd.len() as u32, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[0; 16]); // no supercompression global data
    // Level data is stored smallest mip first; the index lists level 0 first.
    let mut offsets = vec![0usize; n];
    let mut at = data_at;
    for i in (0..n).rev() {
        offsets[i] = at;
        at += levels[i].0.len();
    }
    for (i, (z, raw)) in levels.iter().enumerate() {
        for v in [offsets[i] as u64, z.len() as u64, *raw as u64] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(&dfd);
    for (z, _) in levels.iter().rev() {
        out.extend_from_slice(z);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ktx2_file_has_every_mip_and_is_deterministic() {
        let (w, h) = (64, 32);
        let px: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let a = encode(&px, w, h, Kind::Srgb).unwrap_or_else(|e| panic!("{e}"));
        let b = encode(&px, w, h, Kind::Srgb).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a, b);
        assert_eq!(&a[..4], &[0xAB, 0x4B, 0x54, 0x58]);
        let levels = u32::from_le_bytes([a[40], a[41], a[42], a[43]]);
        assert_eq!(levels, 7); // 64x32 .. 1x1
    }

    #[test]
    fn an_odd_sized_image_is_stretched_to_whole_blocks() {
        let (w, h) = (37, 10);
        let px = vec![200u8; w * h * 4];
        let a = encode(&px, w, h, Kind::Linear).unwrap_or_else(|e| panic!("{e}"));
        let dim = |i: usize| u32::from_le_bytes([a[i], a[i + 1], a[i + 2], a[i + 3]]);
        assert_eq!((dim(20), dim(24)), (40, 12));
    }
}
