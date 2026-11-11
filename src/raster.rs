//! Tiny CPU raster helpers shared by the inspect images and the agent-view
//! overlays: an RGBA canvas with lines, filled polygons, 5x7 bitmap text, and
//! PNG encode/decode. Deliberately dependency-free beyond `png`.

/// An RGBA8 image, row-major, top row first.
#[derive(Clone)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, bg: [u8; 4]) -> Self {
        let mut px = vec![0u8; w * h * 4];
        for c in px.as_chunks_mut::<4>().0 {
            *c = bg;
        }
        Self { w, h, px }
    }

    pub fn from_rgba(w: usize, h: usize, px: Vec<u8>) -> Self {
        Self { w, h, px }
    }

    pub fn set(&mut self, x: i64, y: i64, c: [u8; 4]) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        self.px[i..i + 4].copy_from_slice(&c);
    }

    /// A 1 px line (`thick` > 1 draws a square brush).
    pub fn line(&mut self, a: (f64, f64), b: (f64, f64), c: [u8; 4], thick: i64) {
        let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil().max(1.0) as i64;
        let r = thick / 2;
        for s in 0..=steps {
            let t = s as f64 / steps as f64;
            let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            for dy in -r..=r {
                for dx in -r..=r {
                    self.set(x.round() as i64 + dx, y.round() as i64 + dy, c);
                }
            }
        }
    }

    /// Fills the even-odd union of `rings` (pixel coordinates).
    pub fn fill_rings(&mut self, rings: &[Vec<(f64, f64)>], c: [u8; 4]) {
        for y in 0..self.h {
            let sy = y as f64 + 0.5;
            let mut xs: Vec<f64> = Vec::new();
            for r in rings {
                for k in 0..r.len() {
                    let (p, q) = (r[k], r[(k + 1) % r.len()]);
                    if (p.1 <= sy) != (q.1 <= sy) {
                        xs.push(p.0 + (sy - p.1) / (q.1 - p.1) * (q.0 - p.0));
                    }
                }
            }
            xs.sort_by(f64::total_cmp);
            for pair in xs.chunks(2) {
                if let [x0, x1] = pair {
                    let (a, b) = (x0.round().max(0.0) as usize, x1.round().max(0.0) as usize);
                    for x in a..b.min(self.w) {
                        self.set(x as i64, y as i64, c);
                    }
                }
            }
        }
    }

    /// White 5x7 text on a darkened box at (x0, y0), `scale` px per dot.
    pub fn text(&mut self, x0: usize, y0: usize, text: &str, scale: usize) {
        let (w, h) = (self.w, self.h);
        let (bw, bh) = ((text.chars().count() * 6 + 2) * scale, 9 * scale);
        for y in y0..(y0 + bh).min(h) {
            for x in x0..(x0 + bw).min(w) {
                let i = (y * w + x) * 4;
                self.px[i..i + 3].iter_mut().for_each(|c| *c /= 4);
                self.px[i + 3] = 255;
            }
        }
        for (k, ch) in text.chars().enumerate() {
            let g = glyph(ch);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (16 >> col) == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let x = x0 + (1 + k * 6 + col) * scale + dx;
                            let y = y0 + (1 + row) * scale + dy;
                            self.set(x as i64, y as i64, [255; 4]);
                        }
                    }
                }
            }
        }
    }

    /// Pixel width of `text` at `scale`, matching [`Self::text`].
    pub fn text_width(text: &str, scale: usize) -> usize {
        (text.chars().count() * 6 + 2) * scale
    }

    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        encode_png(self.w as u32, self.h as u32, &self.px)
    }
}

pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| format!("png body: {e}"))?;
    }
    Ok(out)
}

pub fn decode_png(bytes: &[u8]) -> Result<Canvas, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png decode: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("png: size overflow")?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png decode: {e}"))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err("png decode: expected 8-bit RGBA".into());
    }
    buf.truncate(info.buffer_size());
    Ok(Canvas::from_rgba(
        info.width as usize,
        info.height as usize,
        buf,
    ))
}

/// The `i`th of a set of distinct, saturated colours (golden-ratio hues),
/// for part-ID renders and section plots.
pub fn id_color(i: usize) -> [f32; 3] {
    // Twelve hand-picked, mutually distinct colours first (Kelly-style).
    const FIXED: [[u8; 3]; 12] = [
        [230, 25, 75],
        [60, 180, 75],
        [255, 225, 25],
        [0, 130, 200],
        [245, 130, 48],
        [145, 30, 180],
        [70, 240, 240],
        [240, 50, 230],
        [170, 110, 40],
        [250, 190, 212],
        [0, 128, 128],
        [255, 255, 255],
    ];
    if let Some(c) = FIXED.get(i) {
        return c.map(|v| v as f32 / 255.0);
    }
    let h = (i as f32 * 0.618_034 + 0.08).fract() * 6.0;
    let (s, v) = (0.75, if i.is_multiple_of(2) { 0.95 } else { 0.7 });
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

pub fn id_color_u8(i: usize) -> [u8; 4] {
    let [r, g, b] = id_color(i);
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255]
}

/// Before/after pixel diff: unchanged pixels greyed and dimmed; changed
/// ones (any channel off by more than 24) magenta where something now is, and
/// cyan where something was (the pixel became background). Returns the image,
/// the changed fraction and its pixel bounding box.
pub fn diff(before: &Canvas, after: &Canvas) -> Result<(Canvas, f64, Option<[usize; 4]>), String> {
    if before.w != after.w || before.h != after.h {
        return Err("diff: the earlier render has a different size".into());
    }
    let near = |a: &[u8], b: &[u8]| a.iter().zip(b).all(|(p, q)| p.abs_diff(*q) <= 24);
    // Background = the most common colour of the earlier render (sampled).
    let mut counts: std::collections::HashMap<[u8; 3], usize> = Default::default();
    for c in before.px.as_chunks::<4>().0.iter().step_by(7) {
        *counts
            .entry([c[0] & 0xF8, c[1] & 0xF8, c[2] & 0xF8])
            .or_default() += 1;
    }
    let bg = counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, _)| c)
        .unwrap_or([0; 3]);
    let mut out = after.clone();
    let (mut n, mut bb) = (0usize, None::<[usize; 4]>);
    for y in 0..after.h {
        for x in 0..after.w {
            let i = (y * after.w + x) * 4;
            let (a, b) = (&before.px[i..i + 3], &after.px[i..i + 3]);
            if near(a, b) {
                let g = ((b[0] as u32 + b[1] as u32 + b[2] as u32) / 3 / 2 + 40) as u8;
                out.px[i..i + 3].copy_from_slice(&[g, g, g]);
                continue;
            }
            n += 1;
            let gone = near(b, &bg);
            let c = if gone {
                [0, 220, 255, 255]
            } else {
                [255, 0, 220, 255]
            };
            out.px[i..i + 4].copy_from_slice(&c);
            bb = Some(match bb {
                None => [x, y, x, y],
                Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
            });
        }
    }
    Ok((out, n as f64 / (after.w * after.h).max(1) as f64, bb))
}

/// Formats a length in metres for labels: "1.25 m", "35 mm", "0.4 mm".
pub fn fmt_len(m: f64) -> String {
    let a = m.abs();
    if a >= 1.0 {
        format!("{m:.2} M")
    } else if a >= 0.01 {
        format!("{:.0} MM", m * 1000.0)
    } else {
        format!("{:.1} MM", m * 1000.0)
    }
}

/// 5x7 bitmap glyphs (rows top to bottom, 5 low bits); lowercase draws as
/// uppercase, unknown characters as blanks.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 17, 25, 21, 19, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [31, 2, 4, 2, 1, 17, 14],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 12, 4, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '%' => [24, 25, 2, 4, 8, 19, 3],
        '/' => [0, 1, 2, 4, 8, 16, 0],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '<' => [2, 4, 8, 16, 8, 4, 2],
        '>' => [8, 4, 2, 1, 2, 4, 8],
        '*' => [0, 4, 21, 14, 21, 4, 0],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips_and_diff_finds_the_change() {
        let a = Canvas::new(20, 10, [10, 10, 10, 255]);
        let mut b = a.clone();
        b.fill_rings(
            &[vec![(5.0, 2.0), (9.0, 2.0), (9.0, 6.0), (5.0, 6.0)]],
            [200, 0, 0, 255],
        );
        let png = b.to_png().unwrap_or_else(|e| panic!("{e}"));
        let back = decode_png(&png).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.px, b.px);
        let (_, frac, bb) = diff(&a, &back).unwrap_or_else(|e| panic!("{e}"));
        assert!((frac - 16.0 / 200.0).abs() < 1e-9, "{frac}");
        assert_eq!(bb, Some([5, 2, 8, 5]));
    }

    #[test]
    fn id_colors_differ() {
        let c: Vec<_> = (0..8).map(id_color_u8).collect();
        for i in 0..8 {
            for j in i + 1..8 {
                assert_ne!(c[i], c[j]);
            }
        }
    }
}
