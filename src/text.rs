//! 3D text: glyph outlines from a TrueType font, flattened to rings and
//! extruded into one closed solid (signs, labels, consoles, plaques).
//! Bundled fonts (SIL Open Font License, `fonts/`): Fira Sans regular and
//! bold, Fira Mono; any `.ttf` / `.otf` path works too.

use ttf_parser::{Face, OutlineBuilder};

const SANS: &[u8] = include_bytes!("../fonts/FiraSans-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/FiraSans-Bold.ttf");
const MONO: &[u8] = include_bytes!("../fonts/FiraMono-Medium.ttf");

pub const FONTS: &[&str] = &["sans", "bold", "mono"];

/// Collects a glyph's contours as polylines, curves flattened.
struct Rings {
    rings: Vec<Vec<[f32; 2]>>,
    cur: Vec<[f32; 2]>,
    at: [f32; 2],
    /// Font units -> meters, and the glyph's pen position (font units).
    k: f32,
    pen: [f32; 2],
}

impl Rings {
    fn pt(&self, x: f32, y: f32) -> [f32; 2] {
        [(x + self.pen[0]) * self.k, (y + self.pen[1]) * self.k]
    }
}

const CURVE_STEPS: usize = 6;

impl OutlineBuilder for Rings {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.at = [x, y];
        let p = self.pt(x, y);
        self.cur.push(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.at = [x, y];
        let p = self.pt(x, y);
        self.cur.push(p);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let [x0, y0] = self.at;
        for i in 1..=CURVE_STEPS {
            let t = i as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            let px = u * u * x0 + 2.0 * u * t * x1 + t * t * x;
            let py = u * u * y0 + 2.0 * u * t * y1 + t * t * y;
            let p = self.pt(px, py);
            self.cur.push(p);
        }
        self.at = [x, y];
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let [x0, y0] = self.at;
        for i in 1..=CURVE_STEPS {
            let t = i as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            let p = self.pt(
                a * x0 + b * x1 + c * x2 + d * x,
                a * y0 + b * y1 + c * y2 + d * y,
            );
            self.cur.push(p);
        }
        self.at = [x, y];
    }
    fn close(&mut self) {
        let mut ring = std::mem::take(&mut self.cur);
        // Drop the closing duplicate and repeated points.
        ring.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-7 && (a[1] - b[1]).abs() < 1e-7);
        if ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() >= 3 {
            self.rings.push(ring);
        }
    }
}

/// The rings of `text` laid out in the plane (x right, y up), meters: the
/// first line's baseline at y = 0, lines going down, `size` = capital
/// height, `align` left / center / right about x = 0.
pub fn outline(
    text: &str,
    font: &str,
    size: f32,
    align: &str,
    line_spacing: f32,
    letter_spacing: f32,
) -> Result<Vec<Vec<[f32; 2]>>, String> {
    let owned;
    let data: &[u8] = match font {
        "sans" | "" => SANS,
        "bold" => BOLD,
        "mono" => MONO,
        path => {
            owned = std::fs::read(path).map_err(|e| {
                format!(
                    "font: `{path}` is not a bundled font ({}) and could not be read: {e}",
                    FONTS.join(", ")
                )
            })?;
            &owned
        }
    };
    let face = Face::parse(data, 0).map_err(|e| format!("font `{font}`: {e}"))?;
    let cap = face
        .capital_height()
        .filter(|c| *c > 0)
        .map_or(face.units_per_em() as f32 * 0.7, f32::from);
    let k = size / cap;
    let line_h = face.height() as f32 * line_spacing;
    let mut all = Vec::new();
    for (li, line) in text.split('\n').enumerate() {
        let mut b = Rings {
            rings: Vec::new(),
            cur: Vec::new(),
            at: [0.0, 0.0],
            k,
            pen: [0.0, -(li as f32) * line_h],
        };
        let mut missing = Vec::new();
        for ch in line.chars() {
            let Some(id) = face.glyph_index(ch) else {
                missing.push(ch);
                continue;
            };
            face.outline_glyph(id, &mut b);
            b.close();
            let adv = face.glyph_hor_advance(id).unwrap_or(0) as f32;
            b.pen[0] += adv + letter_spacing * cap;
        }
        if !missing.is_empty() {
            return Err(format!(
                "font `{font}` has no glyph for {:?}",
                missing.iter().collect::<String>()
            ));
        }
        let width = (b.pen[0] - letter_spacing * cap).max(0.0) * k;
        let shift = match align {
            "left" => 0.0,
            "right" => -width,
            _ => -width / 2.0,
        };
        for mut r in b.rings {
            for p in &mut r {
                p[0] += shift;
            }
            all.push(r);
        }
    }
    if all.is_empty() {
        return Err("text: nothing to draw (only spaces?)".into());
    }
    Ok(all)
}

/// The text as a closed solid standing up: letters face +z (readable from
/// the front), baseline on y = 0, `depth` thick toward -z.
pub fn build(
    name: &str,
    text: &str,
    font: &str,
    size: f32,
    depth: f32,
    align: &str,
    line_spacing: f32,
    letter_spacing: f32,
) -> Result<crate::Part, crate::ModelerError> {
    if !(size > 0.0 && depth > 0.0) {
        return Err(format!("text `{name}`: size and depth must be > 0").into());
    }
    let rings = outline(text, font, size, align, line_spacing, letter_spacing)
        .map_err(|e| format!("text `{name}`: {e}"))?;
    let mut part = crate::csg::extrude(name, &rings, depth, 0.0, [1.0, 1.0], 1, 0.0)?;
    // Footprint (x, z) up y -> stand it up: footprint z becomes height and
    // the thickness goes to -z, so the face reads from the front.
    for v in &mut part.verts {
        let [x, y, z] = *v;
        *v = [x, z, -y];
    }
    for n in &mut part.normals {
        let [x, y, z] = *n;
        *n = [x, z, -y];
    }
    // Shift so the front face is at z = 0.
    for v in &mut part.verts {
        v[2] += depth;
    }
    Ok(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_a_closed_solid_of_the_right_size() {
        let p = build("sign", "EXIT 42\nob", "bold", 0.2, 0.03, "center", 1.2, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&p), 0);
        let (lo, hi) = p.verts.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), v| {
            (
                [lo[0].min(v[0]), lo[1].min(v[1]), lo[2].min(v[2])],
                [hi[0].max(v[0]), hi[1].max(v[1]), hi[2].max(v[2])],
            )
        });
        // Capitals are 0.2 tall above the baseline; centred; 3 cm deep.
        assert!((hi[1] - 0.2).abs() < 0.02, "{hi:?}");
        assert!(lo[1] < -0.2, "two lines: {lo:?}");
        assert!((lo[0] + hi[0]).abs() < 0.05, "centred: {lo:?} {hi:?}");
        assert!((hi[2] - lo[2] - 0.03).abs() < 1e-4 && hi[2].abs() < 1e-4);
        let vol: f32 = p
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let v = |i: u32| bevy::math::Vec3::from(p.verts[i as usize]);
                v(t[0]).dot(v(t[1]).cross(v(t[2]))) / 6.0
            })
            .sum();
        assert!(vol > 0.0, "outward: {vol}");
        let Err(e) = outline("x", "nope.ttf", 0.1, "left", 1.0, 0.0) else {
            panic!("missing font")
        };
        assert!(e.contains("bundled"), "{e}");
    }
}
